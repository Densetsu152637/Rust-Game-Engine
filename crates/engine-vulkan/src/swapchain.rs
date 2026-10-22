//! Window swapchain: creation, recreation, acquire and present.
//!
//! Recreation (resize, out-of-date, suboptimal) waits for the device to go idle
//! first, so the old swapchain, views, framebuffers and depth can be destroyed
//! immediately without tracking which frames still use them.

use crate::error::{self, VkCheck};
use crate::frame::create_semaphore;
use crate::instance::Gpu;
use crate::pipeline::{PassKind, PipelineStore, create_render_pass};
use crate::resources::create_image_view;
use crate::target::{DepthBuffer, create_framebuffer};
use crate::translate::{self, COLOR_FORMAT};
use ash::vk;
use engine_render::{RenderError, Result};

/// Acquire timeout; a frame that cannot get an image in this time is skipped.
const ACQUIRE_TIMEOUT_NS: u64 = 2_000_000_000;

/// One swapchain generation and everything derived from it.
pub(crate) struct Swapchain {
    pub raw: vk::SwapchainKHR,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
    views: Vec<vk::ImageView>,
    pub framebuffers: Vec<vk::Framebuffer>,
    depth: Option<DepthBuffer>,
    /// One per swapchain image: signaled by the frame's submit, waited by present.
    pub render_finished: Vec<vk::Semaphore>,
}

impl Swapchain {
    fn empty() -> Self {
        Self {
            raw: vk::SwapchainKHR::null(),
            format: vk::Format::UNDEFINED,
            extent: vk::Extent2D::default(),
            views: Vec::new(),
            framebuffers: Vec::new(),
            depth: None,
            render_finished: Vec::new(),
        }
    }

    /// Destroy everything. The device must be idle.
    fn destroy(mut self, gpu: &mut Gpu, loader: &ash::khr::swapchain::Device) {
        // SAFETY: the device is idle, so nothing uses these objects.
        unsafe {
            for framebuffer in self.framebuffers.drain(..) {
                gpu.device.destroy_framebuffer(framebuffer, None);
            }
            for view in self.views.drain(..) {
                gpu.device.destroy_image_view(view, None);
            }
            for semaphore in self.render_finished.drain(..) {
                gpu.device.destroy_semaphore(semaphore, None);
            }
        }
        if let Some(depth) = self.depth.take() {
            depth.destroy(gpu);
        }
        // SAFETY: the device is idle; a null swapchain is ignored.
        unsafe { loader.destroy_swapchain(self.raw, None) };
    }
}

/// Result of (re)creating the swapchain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Recreated {
    Ready,
    /// The surface has zero extent (minimized); no swapchain exists.
    Suspended,
}

pub(crate) enum Acquired {
    Image { index: u32, suboptimal: bool },
    OutOfDate,
    Timeout,
}

pub(crate) enum Presented {
    Ok,
    Suboptimal,
    OutOfDate,
}

/// The window side of the backend.
pub(crate) struct SurfaceState {
    pub loader: ash::khr::swapchain::Device,
    /// Size the host last reported, used when the surface has no fixed extent.
    pub requested: (u32, u32),
    pub swapchain: Option<Swapchain>,
}

impl SurfaceState {
    pub(crate) fn new(gpu: &Gpu, size: (u32, u32)) -> Self {
        Self {
            loader: ash::khr::swapchain::Device::new(&gpu.instance.raw, &gpu.device),
            requested: size,
            swapchain: None,
        }
    }

    pub(crate) fn extent(&self) -> Option<vk::Extent2D> {
        self.swapchain.as_ref().map(|s| s.extent)
    }

    /// Create (or recreate) the swapchain for the current surface state.
    pub(crate) fn recreate(
        &mut self,
        gpu: &mut Gpu,
        store: &mut PipelineStore,
    ) -> Result<Recreated> {
        let (caps, formats) = {
            let surface =
                gpu.instance
                    .surface
                    .as_ref()
                    .ok_or_else(|| RenderError::Unsupported {
                        feature: "surface-presentation".into(),
                        reason: "the backend was created without a window".into(),
                    })?;
            // SAFETY: the surface and physical device belong to this instance.
            unsafe {
                (
                    surface
                        .loader
                        .get_physical_device_surface_capabilities(gpu.physical, surface.raw)
                        .vk("vkGetPhysicalDeviceSurfaceCapabilitiesKHR")?,
                    surface
                        .loader
                        .get_physical_device_surface_formats(gpu.physical, surface.raw)
                        .vk("vkGetPhysicalDeviceSurfaceFormatsKHR")?,
                )
            }
        };
        let extent = translate::choose_extent(&caps, self.requested);
        gpu.wait_idle()?;
        if extent.width == 0 || extent.height == 0 {
            if let Some(old) = self.swapchain.take() {
                old.destroy(gpu, &self.loader);
            }
            return Ok(Recreated::Suspended);
        }
        let format =
            translate::choose_surface_format(&formats).ok_or_else(|| RenderError::Unsupported {
                feature: "surface-presentation".into(),
                reason: format!(
                    "the surface offers no 8-bit sRGB format (formats: {:?}); targets and \
                     textures are sRGB, so presenting would change colors",
                    formats.iter().map(|f| f.format).collect::<Vec<_>>()
                ),
            })?;
        if !caps
            .supported_usage_flags
            .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        {
            return Err(RenderError::Unsupported {
                feature: "surface-presentation".into(),
                reason: "swapchain images cannot be color attachments".into(),
            });
        }

        let old = self.swapchain.take();
        self.ensure_surface_pass(gpu, store, format.format, old.as_ref().map(|s| s.format))?;
        let mut fresh = Swapchain::empty();
        let built = self.build(
            gpu,
            store,
            &mut fresh,
            &caps,
            format,
            extent,
            old.as_ref().map_or(vk::SwapchainKHR::null(), |s| s.raw),
        );
        if let Some(old) = old {
            old.destroy(gpu, &self.loader);
        }
        match built {
            Ok(()) => {
                self.swapchain = Some(fresh);
                Ok(Recreated::Ready)
            }
            Err(error) => {
                fresh.destroy(gpu, &self.loader);
                Err(error)
            }
        }
    }

    /// Keep one surface render pass matching the swapchain format. Pipelines
    /// built for a vanished format are destroyed (the device is idle).
    fn ensure_surface_pass(
        &self,
        gpu: &mut Gpu,
        store: &mut PipelineStore,
        format: vk::Format,
        previous: Option<vk::Format>,
    ) -> Result<()> {
        if store.surface_pass.is_some_and(|(f, _)| f == format) {
            return Ok(());
        }
        let pass = create_render_pass(&gpu.device, format, PassKind::Surface)?;
        if let Some((old_format, old_pass)) = store.surface_pass.replace((format, pass)) {
            // SAFETY: the device is idle (see `recreate`).
            unsafe { gpu.device.destroy_render_pass(old_pass, None) };
            if old_format != COLOR_FORMAT {
                for pipeline in store.take_format(old_format) {
                    // SAFETY: the device is idle.
                    unsafe { gpu.device.destroy_pipeline(pipeline, None) };
                }
            }
        }
        let _ = previous;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        &self,
        gpu: &mut Gpu,
        store: &PipelineStore,
        fresh: &mut Swapchain,
        caps: &vk::SurfaceCapabilitiesKHR,
        format: vk::SurfaceFormatKHR,
        extent: vk::Extent2D,
        old: vk::SwapchainKHR,
    ) -> Result<()> {
        let surface = gpu
            .instance
            .surface
            .as_ref()
            .map(|s| s.raw)
            .ok_or_else(|| RenderError::Backend("surface disappeared".into()))?;
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(translate::choose_image_count(caps))
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(translate::choose_composite_alpha(caps))
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(old);
        // SAFETY: `info` references the live surface and (possibly null) old swapchain.
        fresh.raw = unsafe { self.loader.create_swapchain(&info, None) }
            .map_err(|r| error::from_vk("vkCreateSwapchainKHR", r))?;
        fresh.format = format.format;
        fresh.extent = extent;
        // SAFETY: the swapchain was just created by this loader.
        let images =
            unsafe { self.loader.get_swapchain_images(fresh.raw) }.vk("vkGetSwapchainImagesKHR")?;
        let depth = DepthBuffer::create(gpu, extent.width, extent.height)?;
        let depth_view = depth.view;
        fresh.depth = Some(depth);
        let pass = store
            .surface_pass
            .map(|(_, pass)| pass)
            .ok_or_else(|| RenderError::Backend("surface render pass missing".into()))?;
        for image in images {
            let view = create_image_view(
                &gpu.device,
                image,
                format.format,
                vk::ImageAspectFlags::COLOR,
            )?;
            fresh.views.push(view);
            fresh.framebuffers.push(create_framebuffer(
                &gpu.device,
                pass,
                view,
                depth_view,
                extent.width,
                extent.height,
            )?);
            fresh.render_finished.push(create_semaphore(&gpu.device)?);
        }
        Ok(())
    }

    pub(crate) fn acquire(&self, semaphore: vk::Semaphore) -> Result<Acquired> {
        let Some(swapchain) = &self.swapchain else {
            return Ok(Acquired::OutOfDate);
        };
        // SAFETY: the swapchain and semaphore belong to this device; the semaphore
        // is unsignaled (its previous wait completed with the slot's fence).
        let result = unsafe {
            self.loader.acquire_next_image(
                swapchain.raw,
                ACQUIRE_TIMEOUT_NS,
                semaphore,
                vk::Fence::null(),
            )
        };
        match result {
            Ok((index, suboptimal)) => Ok(Acquired::Image { index, suboptimal }),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Ok(Acquired::OutOfDate),
            Err(vk::Result::TIMEOUT | vk::Result::NOT_READY) => Ok(Acquired::Timeout),
            Err(result) => Err(error::from_vk("vkAcquireNextImageKHR", result)),
        }
    }

    pub(crate) fn present(
        &self,
        queue: vk::Queue,
        index: u32,
        wait: vk::Semaphore,
    ) -> Result<Presented> {
        let Some(swapchain) = &self.swapchain else {
            return Ok(Presented::OutOfDate);
        };
        let swapchains = [swapchain.raw];
        let indices = [index];
        let waits = [wait];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&waits)
            .swapchains(&swapchains)
            .image_indices(&indices);
        // SAFETY: the queue, swapchain and semaphore belong to this device and the
        // image `index` was acquired and rendered by the matching submit.
        match unsafe { self.loader.queue_present(queue, &info) } {
            Ok(false) => Ok(Presented::Ok),
            Ok(true) | Err(vk::Result::SUBOPTIMAL_KHR) => Ok(Presented::Suboptimal),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Ok(Presented::OutOfDate),
            Err(result) => Err(error::from_vk("vkQueuePresentKHR", result)),
        }
    }

    /// Destroy the swapchain. The device must be idle.
    pub(crate) fn destroy(&mut self, gpu: &mut Gpu) {
        if let Some(swapchain) = self.swapchain.take() {
            swapchain.destroy(gpu, &self.loader);
        }
    }
}
