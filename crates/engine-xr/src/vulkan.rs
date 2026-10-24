//! `XR_KHR_vulkan_enable2` graphics binding through wgpu-hal: the runtime
//! creates the Vulkan instance and device (adding the extensions it needs),
//! wgpu adopts both, and swapchain images are wrapped as `wgpu::Texture`s.
#![allow(unsafe_code)]

use crate::result_name;
use ash::vk::{self, Handle};
use openxr as xr;
use std::ffi::{c_char, c_void};
use wgpu::hal::{api::Vulkan as HalVulkan, vulkan as hal_vk};

/// A wgpu device on the Vulkan device the XR runtime selected.
pub(crate) struct VulkanGraphics {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: wgpu::Adapter,
    pub instance: wgpu::Instance,
    pub raw_instance: vk::Instance,
    pub raw_physical_device: vk::PhysicalDevice,
    pub raw_device: vk::Device,
    pub queue_family_index: u32,
    pub queue_index: u32,
    /// Vulkan API version the instance was created with.
    pub api_version: u32,
}

/// Device capabilities the renderer needs on the XR device.
#[derive(Clone, Debug)]
pub(crate) struct GraphicsRequest {
    pub features: wgpu::Features,
    pub limits: wgpu::Limits,
}

/// The Vulkan API version to request: 1.2 when the runtime's range allows it,
/// else the nearest bound; at least 1.1.
pub(crate) fn choose_api_version(min: xr::Version, max: xr::Version) -> Result<u32, String> {
    let as_pair = |v: xr::Version| (v.major(), v.minor());
    let (min, max) = (as_pair(min), as_pair(max));
    if min > max {
        return Err(format!(
            "the runtime reports an empty Vulkan range {}.{}..={}.{}",
            min.0, min.1, max.0, max.1
        ));
    }
    if max < (1, 1) {
        return Err(format!(
            "the runtime supports Vulkan up to {}.{}, the engine needs 1.1 or newer",
            max.0, max.1
        ));
    }
    let chosen = (1, 2).clamp(min.max((1, 1)), max);
    Ok(vk::make_api_version(0, chosen.0 as u32, chosen.1 as u32, 0))
}

/// The `VkGetInstanceProcAddr` of `entry`, typed for OpenXR.
fn xr_proc_addr(entry: &ash::Entry) -> xr::sys::platform::VkGetInstanceProcAddr {
    // SAFETY: both types are `extern "system"` functions taking a
    // pointer-sized instance handle and a C string and returning an optional
    // function pointer; ash's handle newtype is `repr(transparent)` over the
    // raw handle, so the ABI is identical.
    unsafe {
        std::mem::transmute::<vk::PFN_vkGetInstanceProcAddr, xr::sys::platform::VkGetInstanceProcAddr>(
            entry.static_fn().get_instance_proc_addr,
        )
    }
}

/// Create the runtime-chosen Vulkan instance and device and adopt them in wgpu.
pub(crate) fn create_graphics(
    xr_instance: &xr::Instance,
    system: xr::SystemId,
    request: &GraphicsRequest,
) -> Result<VulkanGraphics, String> {
    let requirements = xr_instance
        .graphics_requirements::<xr::Vulkan>(system)
        .map_err(|r| {
            format!(
                "xrGetVulkanGraphicsRequirements2KHR failed with {}",
                result_name(r)
            )
        })?;
    let api_version = choose_api_version(
        requirements.min_api_version_supported,
        requirements.max_api_version_supported,
    )?;
    // SAFETY: loads the system Vulkan loader library; ash resolves only its
    // exported entry points.
    let entry =
        unsafe { ash::Entry::load() }.map_err(|e| format!("Vulkan loader unavailable: {e}"))?;
    let flags = wgpu::InstanceFlags::empty();
    let extensions = hal_vk::Instance::desired_extensions(&entry, api_version, flags)
        .map_err(|e| format!("Vulkan instance extensions: {e}"))?;
    let extension_names: Vec<*const c_char> = extensions.iter().map(|e| e.as_ptr()).collect();
    let app_info = vk::ApplicationInfo::default()
        .application_name(c"engine-xr")
        .engine_name(c"engine-xr")
        .api_version(api_version);
    let create_info = vk::InstanceCreateInfo::default()
        .application_info(&app_info)
        .enabled_extension_names(&extension_names);
    // SAFETY: `create_info`, `app_info` and the extension names outlive the
    // call; the procedure address is the loaded loader's (see xr_proc_addr).
    let raw_instance = unsafe {
        xr_instance.create_vulkan_instance(
            system,
            xr_proc_addr(&entry),
            &create_info as *const vk::InstanceCreateInfo as *const c_void,
        )
    }
    .map_err(|r| format!("xrCreateVulkanInstanceKHR failed with {}", result_name(r)))?
    .map_err(|r| {
        format!(
            "vkCreateInstance (through the runtime) failed with {:?}",
            vk::Result::from_raw(r)
        )
    })?;
    let raw_instance = vk::Instance::from_raw(raw_instance as usize as u64);
    // SAFETY: the instance was just created through `entry`'s loader.
    let ash_instance = unsafe { ash::Instance::load(entry.static_fn(), raw_instance) };
    // SAFETY: `ash_instance` was created from `entry` with exactly `extensions`
    // (the runtime may add more, which is allowed), `api_version` and `flags`.
    // Ownership passes to wgpu-hal (no drop callback), so every later failure
    // destroys the instance when `hal_instance` drops.
    let hal_instance = unsafe {
        hal_vk::Instance::from_raw(
            entry,
            ash_instance.clone(),
            api_version,
            0,
            None,
            extensions,
            flags,
            wgpu::MemoryBudgetThresholds::default(),
            false,
            None,
        )
    };
    let hal_instance = match hal_instance {
        Ok(instance) => instance,
        Err(error) => {
            // SAFETY: nothing else references the instance yet.
            unsafe { ash_instance.destroy_instance(None) };
            return Err(format!("wgpu-hal rejected the Vulkan instance: {error}"));
        }
    };
    // SAFETY: the instance handle is the one the runtime created for `system`.
    let physical = unsafe {
        xr_instance.vulkan_graphics_device(system, raw_instance.as_raw() as usize as *const c_void)
    }
    .map_err(|r| {
        format!(
            "xrGetVulkanGraphicsDevice2KHR failed with {}",
            result_name(r)
        )
    })?;
    let raw_physical_device = vk::PhysicalDevice::from_raw(physical as usize as u64);
    let exposed = hal_instance
        .expose_adapter(raw_physical_device)
        .ok_or("wgpu cannot use the runtime's Vulkan physical device")?;
    if !exposed.features.contains(request.features) {
        return Err(format!(
            "the XR GPU lacks requested features {:?}",
            request.features - exposed.features
        ));
    }
    if !request.limits.check_limits(&exposed.capabilities.limits) {
        return Err("the XR GPU does not satisfy the requested limits".into());
    }
    // SAFETY: physical device of this live instance.
    let families =
        unsafe { ash_instance.get_physical_device_queue_family_properties(raw_physical_device) };
    let queue_family_index = families
        .iter()
        .position(|f| {
            f.queue_flags
                .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
        })
        .ok_or("the XR GPU has no graphics+compute queue family")?
        as u32;
    let device_extensions = exposed.adapter.required_device_extensions(request.features);
    let mut physical_features = exposed
        .adapter
        .physical_device_features(&device_extensions, request.features);
    let extension_names: Vec<*const c_char> =
        device_extensions.iter().map(|e| e.as_ptr()).collect();
    let priorities = [1.0_f32];
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family_index)
        .queue_priorities(&priorities)];
    let device_info = physical_features.add_to_device_create(
        vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(&extension_names),
    );
    // SAFETY: `device_info` and everything it points to (queue infos,
    // extension names, the feature chain in `physical_features`) outlive the
    // call; the physical device is the one the runtime selected.
    let raw_device = unsafe {
        xr_instance.create_vulkan_device(
            system,
            xr_proc_addr(hal_instance.shared_instance().entry()),
            raw_physical_device.as_raw() as usize as *const c_void,
            &device_info as *const vk::DeviceCreateInfo as *const c_void,
        )
    }
    .map_err(|r| format!("xrCreateVulkanDeviceKHR failed with {}", result_name(r)))?
    .map_err(|r| {
        format!(
            "vkCreateDevice (through the runtime) failed with {:?}",
            vk::Result::from_raw(r)
        )
    })?;
    let raw_device = vk::Device::from_raw(raw_device as usize as u64);
    // SAFETY: the device was just created on this instance.
    let ash_device = unsafe { ash::Device::load(ash_instance.fp_v1_0(), raw_device) };
    let cleanup = ash_device.clone();
    let queue_index = 0;
    // SAFETY: created from this adapter's physical device with
    // `device_extensions` (a superset of the required ones: the runtime may
    // add more), `physical_features` and `queue_family_index`. Ownership passes
    // to wgpu-hal.
    let open = unsafe {
        exposed.adapter.device_from_raw(
            ash_device,
            None,
            &device_extensions,
            request.features,
            &request.limits,
            &wgpu::MemoryHints::Performance,
            queue_family_index,
            queue_index,
        )
    };
    let open = match open {
        Ok(open) => open,
        Err(error) => {
            // SAFETY: wgpu-hal did not take the device; nothing else uses it.
            unsafe { cleanup.destroy_device(None) };
            return Err(format!("wgpu-hal rejected the Vulkan device: {error}"));
        }
    };
    // SAFETY: the hal instance is a valid Vulkan instance owned by wgpu now.
    let instance = unsafe { wgpu::Instance::from_hal::<HalVulkan>(hal_instance) };
    // SAFETY: `exposed` was produced by this instance's hal instance.
    let adapter = unsafe { instance.create_adapter_from_hal(exposed) };
    // SAFETY: `open` was created from `adapter`'s hal adapter with exactly
    // `request.features`.
    let (device, queue) = unsafe {
        adapter.create_device_from_hal(
            open,
            &wgpu::DeviceDescriptor {
                label: Some("engine-xr device"),
                required_features: request.features,
                required_limits: request.limits.clone(),
                ..Default::default()
            },
        )
    }
    .map_err(|e| format!("wgpu device import failed: {e}"))?;
    Ok(VulkanGraphics {
        device,
        queue,
        adapter,
        instance,
        raw_instance,
        raw_physical_device,
        raw_device,
        queue_family_index,
        queue_index,
        api_version,
    })
}

/// Session plus frame waiter and stream on `graphics`.
pub(crate) type SessionParts = (
    xr::Session<xr::Vulkan>,
    xr::FrameWaiter,
    xr::FrameStream<xr::Vulkan>,
);

/// Create the OpenXR session bound to `graphics`.
pub(crate) fn create_session(
    xr_instance: &xr::Instance,
    system: xr::SystemId,
    graphics: &VulkanGraphics,
) -> xr::Result<SessionParts> {
    let info = xr::vulkan::SessionCreateInfo {
        instance: graphics.raw_instance.as_raw() as usize as *const c_void,
        physical_device: graphics.raw_physical_device.as_raw() as usize as *const c_void,
        device: graphics.raw_device.as_raw() as usize as *const c_void,
        queue_family_index: graphics.queue_family_index,
        queue_index: graphics.queue_index,
    };
    // SAFETY: the handles were created through this instance's
    // XR_KHR_vulkan_enable2 for `system`, and `XrSession` drops the session
    // (and every swapchain) before `graphics`.
    unsafe { xr_instance.create_session::<xr::Vulkan>(system, &info) }
}

/// Wrap one runtime-owned swapchain image as a `wgpu::Texture`.
pub(crate) fn wrap_swapchain_image(
    device: &wgpu::Device,
    image: u64,
    format: wgpu::TextureFormat,
    extent: [u32; 2],
    label: &str,
) -> Result<wgpu::Texture, String> {
    let size = wgpu::Extent3d {
        width: extent[0],
        height: extent[1],
        depth_or_array_layers: 1,
    };
    let hal_descriptor = wgpu::hal::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUses::COLOR_TARGET
            | wgpu::TextureUses::RESOURCE
            | wgpu::TextureUses::COPY_SRC,
        memory_flags: wgpu::hal::MemoryFlags::empty(),
        view_formats: vec![],
    };
    // SAFETY: borrows the hal device only for the wrap below.
    let hal_device =
        unsafe { device.as_hal::<HalVulkan>() }.ok_or("the XR device is not a Vulkan device")?;
    // SAFETY: `image` is a live swapchain image the runtime created on this
    // device with colour-attachment, sampled and transfer-source usage, this
    // format and extent, one layer and one mip, and no mutable-format flag
    // (so no view formats). Memory is external and the drop callback is a
    // no-op: wgpu never frees runtime-owned images, and `XrSession` waits for
    // the device to idle and drops textures before their swapchain.
    let texture = unsafe {
        hal_device.texture_from_raw(
            vk::Image::from_raw(image),
            &hal_descriptor,
            Some(Box::new(|| {})),
            hal_vk::TextureMemory::External,
        )
    };
    drop(hal_device);
    // SAFETY: the hal texture belongs to this device and matches the
    // descriptor; its layout is treated as undefined (contents are always
    // overwritten), which is valid for any actual layout.
    Ok(unsafe {
        device.create_texture_from_hal::<HalVulkan>(
            texture,
            &wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
            wgpu::TextureUses::UNINITIALIZED,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vulkan_version_prefers_1_2_within_the_runtime_range() {
        let v = |major, minor| xr::Version::new(major, minor, 0);
        assert_eq!(
            choose_api_version(v(1, 0), v(1, 3)),
            Ok(vk::API_VERSION_1_2)
        );
        assert_eq!(
            choose_api_version(v(1, 3), v(1, 3)),
            Ok(vk::API_VERSION_1_3)
        );
        assert_eq!(
            choose_api_version(v(1, 0), v(1, 1)),
            Ok(vk::API_VERSION_1_1)
        );
        assert!(
            choose_api_version(v(1, 0), v(1, 0))
                .unwrap_err()
                .contains("1.1 or newer")
        );
    }
}
