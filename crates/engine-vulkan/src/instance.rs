//! Loader, instance, validation, physical-device selection and logical device.
//!
//! [`Gpu`] is the root owner: it destroys the allocator, the device, the
//! surface, the debug messenger and the instance, in that order, when dropped.
//! Everything that was created from the device must be destroyed first (the
//! `Core` in `core.rs` guarantees this).

use crate::WindowSource;
use crate::error::{self, VkCheck};
use crate::memory::Memory;
use crate::translate::DEPTH_FORMAT;
use ash::vk;
use engine_render::{RenderError, Result};
use std::ffi::{CStr, CString, c_void};
use std::sync::{Arc, Mutex};

// --------------------------------------------------------------- validation

/// Severity of a recorded validation message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationSeverity {
    Warning,
    Error,
}

/// One message from the Vulkan validation layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationMessage {
    pub severity: ValidationSeverity,
    pub message: String,
}

#[derive(Default)]
struct ValidationState {
    active: bool,
    messages: Vec<ValidationMessage>,
}

/// Shared list the debug messenger appends to. Clone it before moving the
/// backend into a `RenderDevice` to inspect messages afterwards.
#[derive(Clone, Default)]
pub struct ValidationLog {
    state: Arc<Mutex<ValidationState>>,
}

impl ValidationLog {
    fn lock(&self) -> std::sync::MutexGuard<'_, ValidationState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether the validation layer was found and enabled for the instance.
    pub fn is_active(&self) -> bool {
        self.lock().active
    }

    /// Every recorded message, oldest first.
    pub fn messages(&self) -> Vec<ValidationMessage> {
        self.lock().messages.clone()
    }

    /// Text of every recorded `Error` message.
    pub fn errors(&self) -> Vec<String> {
        self.lock()
            .messages
            .iter()
            .filter(|m| m.severity == ValidationSeverity::Error)
            .map(|m| m.message.clone())
            .collect()
    }

    /// Remove and return all recorded messages.
    pub fn take(&self) -> Vec<ValidationMessage> {
        std::mem::take(&mut self.lock().messages)
    }

    fn set_active(&self, active: bool) {
        self.lock().active = active;
    }
}

fn record(state: &Mutex<ValidationState>, severity: ValidationSeverity, message: String) {
    if let Ok(mut state) = state.lock() {
        state.messages.push(ValidationMessage { severity, message });
    }
}

unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user: *mut c_void,
) -> vk::Bool32 {
    if data.is_null() || user.is_null() {
        return vk::FALSE;
    }
    // SAFETY: `user` is the pointer to the `Mutex<ValidationState>` inside the
    // `Arc` held by `DebugMessenger`, which outlives the messenger.
    let state = unsafe { &*(user as *const Mutex<ValidationState>) };
    // SAFETY: the loader passes a valid callback-data struct for the call.
    let text = unsafe {
        let message = (*data).p_message;
        if message.is_null() {
            String::new()
        } else {
            CStr::from_ptr(message).to_string_lossy().into_owned()
        }
    };
    let severity = if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        ValidationSeverity::Error
    } else {
        ValidationSeverity::Warning
    };
    record(state, severity, text);
    vk::FALSE
}

struct DebugMessenger {
    loader: ash::ext::debug_utils::Instance,
    raw: vk::DebugUtilsMessengerEXT,
    /// Keeps the callback's user-data alive for the messenger's lifetime.
    _log: ValidationLog,
}

// ----------------------------------------------------------------- instance

pub(crate) struct SurfaceHandle {
    pub loader: ash::khr::surface::Instance,
    pub raw: vk::SurfaceKHR,
}

pub(crate) struct Instance {
    pub entry: ash::Entry,
    pub raw: ash::Instance,
    pub surface: Option<SurfaceHandle>,
    debug: Option<DebugMessenger>,
}

fn cstring(text: &str) -> CString {
    // Interior NULs cannot occur in the names we pass; strip them defensively.
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

fn layer_present(layers: &[vk::LayerProperties], name: &CStr) -> bool {
    layers
        .iter()
        .any(|layer| layer.layer_name_as_c_str().is_ok_and(|n| n == name))
}

fn extension_present(extensions: &[vk::ExtensionProperties], name: &CStr) -> bool {
    extensions
        .iter()
        .any(|ext| ext.extension_name_as_c_str().is_ok_and(|n| n == name))
}

impl Instance {
    pub(crate) fn new(
        config: &crate::VulkanConfig,
        window: Option<&dyn WindowSource>,
        library: Option<&std::ffi::OsStr>,
        log: &ValidationLog,
    ) -> Result<Self> {
        // SAFETY: loading the Vulkan loader runs its initializers; the returned
        // `Entry` keeps the library mapped for as long as it lives.
        let entry = unsafe {
            match library {
                Some(path) => ash::Entry::load_from(path),
                None => ash::Entry::load(),
            }
        }
        .map_err(|e| error::loader_unavailable(&e))?;

        // SAFETY: plain query on a loaded entry.
        let version = unsafe { entry.try_enumerate_instance_version() }
            .vk("vkEnumerateInstanceVersion")?
            .unwrap_or(vk::API_VERSION_1_0);
        if version < vk::API_VERSION_1_1 {
            return Err(RenderError::Unavailable(
                "the installed Vulkan loader only supports Vulkan 1.0; update the GPU driver or \
                 Vulkan runtime to get Vulkan 1.1 or newer"
                    .into(),
            ));
        }
        let api_version = version.min(vk::API_VERSION_1_3);

        // SAFETY: plain queries on a loaded entry.
        let layers = unsafe { entry.enumerate_instance_layer_properties() }
            .vk("vkEnumerateInstanceLayerProperties")?;
        // SAFETY: as above.
        let extensions = unsafe { entry.enumerate_instance_extension_properties(None) }
            .vk("vkEnumerateInstanceExtensionProperties")?;

        let validation_layer = c"VK_LAYER_KHRONOS_validation";
        let validation = config.enable_validation
            && layer_present(&layers, validation_layer)
            && extension_present(&extensions, ash::ext::debug_utils::NAME);

        let mut enabled_extensions: Vec<*const std::ffi::c_char> = Vec::new();
        if let Some(window) = window {
            let display = window
                .display_handle()
                .map_err(|e| {
                    RenderError::Unavailable(format!("window has no display handle: {e}"))
                })?
                .as_raw();
            let required = ash_window::enumerate_required_extensions(display)
                .map_err(|r| error::init_failure("surface extension lookup", r))?;
            enabled_extensions.extend_from_slice(required);
        }
        if validation {
            enabled_extensions.push(ash::ext::debug_utils::NAME.as_ptr());
        }
        let mut flags = vk::InstanceCreateFlags::empty();
        if extension_present(&extensions, ash::khr::portability_enumeration::NAME) {
            enabled_extensions.push(ash::khr::portability_enumeration::NAME.as_ptr());
            flags |= vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
        }
        let enabled_layers: Vec<*const std::ffi::c_char> = if validation {
            vec![validation_layer.as_ptr()]
        } else {
            Vec::new()
        };

        let app_name = cstring(&config.application_name);
        let engine_name = c"engine-vulkan";
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(1)
            .engine_name(engine_name)
            .engine_version(1)
            .api_version(api_version);
        let create_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_layer_names(&enabled_layers)
            .enabled_extension_names(&enabled_extensions)
            .flags(flags);
        // SAFETY: all pointers in `create_info` reference locals alive for the call.
        let raw = unsafe { entry.create_instance(&create_info, None) }
            .map_err(|r| error::init_failure("vkCreateInstance", r))?;

        let mut instance = Self {
            entry,
            raw,
            surface: None,
            debug: None,
        };
        // Any early return below drops `instance`, which destroys what exists.
        if validation {
            instance.debug = Some(instance.create_messenger(log)?);
            log.set_active(true);
        }
        if let Some(window) = window {
            instance.surface = Some(instance.create_surface(window)?);
        }
        Ok(instance)
    }

    fn create_messenger(&self, log: &ValidationLog) -> Result<DebugMessenger> {
        let loader = ash::ext::debug_utils::Instance::new(&self.entry, &self.raw);
        let user = Arc::as_ptr(&log.state) as *mut c_void;
        let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
            .message_severity(
                vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                    | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
            )
            .message_type(
                vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                    | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                    | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
            )
            .pfn_user_callback(Some(debug_callback))
            .user_data(user);
        // SAFETY: `info` is valid; `user` points into an `Arc` that the returned
        // `DebugMessenger` keeps alive until the messenger is destroyed.
        let raw = unsafe { loader.create_debug_utils_messenger(&info, None) }
            .vk("vkCreateDebugUtilsMessengerEXT")?;
        Ok(DebugMessenger {
            loader,
            raw,
            _log: log.clone(),
        })
    }

    fn create_surface(&self, window: &dyn WindowSource) -> Result<SurfaceHandle> {
        let display = window
            .display_handle()
            .map_err(|e| RenderError::Unavailable(format!("window has no display handle: {e}")))?
            .as_raw();
        let handle = window
            .window_handle()
            .map_err(|e| RenderError::Unavailable(format!("window has no window handle: {e}")))?
            .as_raw();
        // SAFETY: the handles come from a live window the backend keeps alive for
        // as long as the surface exists.
        let raw =
            unsafe { ash_window::create_surface(&self.entry, &self.raw, display, handle, None) }
                .map_err(|r| error::init_failure("vkCreateSurfaceKHR", r))?;
        Ok(SurfaceHandle {
            loader: ash::khr::surface::Instance::new(&self.entry, &self.raw),
            raw,
        })
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the surface and messenger were created from this instance and
        // nothing else uses them once the owner is dropped; the instance is
        // destroyed last.
        unsafe {
            if let Some(surface) = self.surface.take() {
                surface.loader.destroy_surface(surface.raw, None);
            }
            if let Some(debug) = self.debug.take() {
                debug.loader.destroy_debug_utils_messenger(debug.raw, None);
            }
            self.raw.destroy_instance(None);
        }
    }
}

// ---------------------------------------------------------- device selection

/// One queue family of a candidate device, as plain data.
#[derive(Clone, Debug)]
pub(crate) struct QueueFamilyCandidate {
    pub graphics: bool,
    /// Can present to the window surface (always false without one).
    pub present: bool,
    pub queue_count: u32,
}

/// Everything device selection looks at, as plain data so scoring is testable.
#[derive(Clone, Debug)]
pub(crate) struct DeviceCandidate {
    pub name: String,
    pub device_type: vk::PhysicalDeviceType,
    pub api_version: u32,
    pub families: Vec<QueueFamilyCandidate>,
    pub has_swapchain_extension: bool,
    pub supports_depth_d32: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub index: usize,
    pub queue_family: u32,
    pub score: u64,
}

/// Rank a device, or say why it cannot be used.
pub(crate) fn evaluate(
    candidate: &DeviceCandidate,
    needs_present: bool,
) -> std::result::Result<(u32, u64), String> {
    let name = &candidate.name;
    if candidate.api_version < vk::API_VERSION_1_1 {
        return Err(format!("{name}: only supports Vulkan 1.0"));
    }
    if !candidate.supports_depth_d32 {
        return Err(format!("{name}: no D32_SFLOAT depth attachment support"));
    }
    if needs_present && !candidate.has_swapchain_extension {
        return Err(format!("{name}: VK_KHR_swapchain is not available"));
    }
    let family = candidate
        .families
        .iter()
        .position(|f| f.graphics && f.queue_count > 0 && (!needs_present || f.present));
    let Some(family) = family else {
        return Err(if needs_present {
            format!("{name}: no graphics queue that can present to the window")
        } else {
            format!("{name}: no graphics queue")
        });
    };
    let type_score = match candidate.device_type {
        vk::PhysicalDeviceType::DISCRETE_GPU => 4000,
        vk::PhysicalDeviceType::INTEGRATED_GPU => 3000,
        vk::PhysicalDeviceType::VIRTUAL_GPU => 2000,
        vk::PhysicalDeviceType::CPU => 1000,
        _ => 500,
    };
    // Newer API versions break ties between devices of the same class.
    let api_score = u64::from(vk::api_version_minor(candidate.api_version));
    Ok((family as u32, type_score + api_score))
}

/// Pick the best usable device (first wins ties). On failure, one reason per
/// rejected device.
pub(crate) fn select_device(
    candidates: &[DeviceCandidate],
    needs_present: bool,
) -> std::result::Result<Selection, Vec<String>> {
    let mut best: Option<Selection> = None;
    let mut reasons = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        match evaluate(candidate, needs_present) {
            Ok((queue_family, score)) => {
                if best.is_none_or(|b| score > b.score) {
                    best = Some(Selection {
                        index,
                        queue_family,
                        score,
                    });
                }
            }
            Err(reason) => reasons.push(reason),
        }
    }
    best.ok_or(reasons)
}

// ---------------------------------------------------------------------- gpu

/// Facts about the chosen device that the rest of the backend consults.
#[derive(Clone, Debug)]
pub(crate) struct DeviceInfo {
    pub name: String,
    pub limits: vk::PhysicalDeviceLimits,
    pub portability_subset: bool,
    pub device_type: vk::PhysicalDeviceType,
    pub api_version: u32,
}

/// The root Vulkan owner: instance, device, queue and allocator.
pub(crate) struct Gpu {
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub physical: vk::PhysicalDevice,
    pub info: DeviceInfo,
    /// `None` only while dropping, so the allocator goes before the device.
    memory: Option<Memory>,
    pub instance: Instance,
}

impl Gpu {
    pub(crate) fn new(
        config: &crate::VulkanConfig,
        window: Option<&dyn WindowSource>,
        library: Option<&std::ffi::OsStr>,
        log: &ValidationLog,
    ) -> Result<Self> {
        let instance = Instance::new(config, window, library, log)?;
        // SAFETY: the instance is valid.
        let physical_devices = unsafe { instance.raw.enumerate_physical_devices() }
            .map_err(|r| error::init_failure("vkEnumeratePhysicalDevices", r))?;

        let mut candidates = Vec::with_capacity(physical_devices.len());
        let mut extension_lists = Vec::with_capacity(physical_devices.len());
        for &physical in &physical_devices {
            let (candidate, extensions) = describe_device(&instance, physical)?;
            candidates.push(candidate);
            extension_lists.push(extensions);
        }
        let needs_present = window.is_some();
        let selection = select_device(&candidates, needs_present)
            .map_err(|reasons| error::no_suitable_device(&reasons))?;
        let physical = physical_devices[selection.index];
        let candidate = &candidates[selection.index];

        // SAFETY: `physical` came from this instance.
        let properties = unsafe { instance.raw.get_physical_device_properties(physical) };

        let mut extensions: Vec<*const std::ffi::c_char> = Vec::new();
        if needs_present {
            extensions.push(ash::khr::swapchain::NAME.as_ptr());
        }
        let portability = extension_present(
            &extension_lists[selection.index],
            ash::khr::portability_subset::NAME,
        );
        if portability {
            extensions.push(ash::khr::portability_subset::NAME.as_ptr());
        }
        let priorities = [1.0f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(selection.queue_family)
            .queue_priorities(&priorities)];
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&extensions);
        // SAFETY: `device_info` references locals alive for the call.
        let device = unsafe { instance.raw.create_device(physical, &device_info, None) }
            .map_err(|r| error::init_failure("vkCreateDevice", r))?;
        // SAFETY: the queue family/index were requested in `queue_info`.
        let queue = unsafe { device.get_device_queue(selection.queue_family, 0) };

        let info = DeviceInfo {
            name: candidate.name.clone(),
            limits: properties.limits,
            portability_subset: portability,
            device_type: properties.device_type,
            api_version: properties.api_version,
        };
        let memory = match Memory::new(&instance.raw, &device, physical) {
            Ok(memory) => memory,
            Err(error) => {
                // SAFETY: nothing else was created from the device yet.
                unsafe { device.destroy_device(None) };
                return Err(error);
            }
        };
        Ok(Self {
            device,
            queue,
            queue_family: selection.queue_family,
            physical,
            info,
            memory: Some(memory),
            instance,
        })
    }

    pub(crate) fn memory(&mut self) -> &mut Memory {
        self.memory
            .as_mut()
            .expect("allocator exists until the Gpu is dropped")
    }

    pub(crate) fn wait_idle(&self) -> Result<()> {
        // SAFETY: the device is valid; waiting has no other preconditions.
        unsafe { self.device.device_wait_idle() }.vk("vkDeviceWaitIdle")
    }

    /// `minUniformBufferOffsetAlignment`, never below 1.
    pub(crate) fn uniform_alignment(&self) -> u64 {
        self.info.limits.min_uniform_buffer_offset_alignment.max(1)
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // SAFETY: waiting is always legal; afterwards the allocator (which owns
        // device memory) is dropped before the device it allocated from.
        unsafe {
            let _ = self.device.device_wait_idle();
        }
        self.memory.take();
        // SAFETY: every child object was destroyed by the owner before this drop.
        unsafe { self.device.destroy_device(None) };
    }
}

fn describe_device(
    instance: &Instance,
    physical: vk::PhysicalDevice,
) -> Result<(DeviceCandidate, Vec<vk::ExtensionProperties>)> {
    let raw = &instance.raw;
    // SAFETY: `physical` was enumerated from `raw`; these are pure queries.
    let (properties, families, extensions, depth) = unsafe {
        (
            raw.get_physical_device_properties(physical),
            raw.get_physical_device_queue_family_properties(physical),
            raw.enumerate_device_extension_properties(physical),
            raw.get_physical_device_format_properties(physical, DEPTH_FORMAT),
        )
    };
    let extensions = extensions.vk("vkEnumerateDeviceExtensionProperties")?;
    let mut family_candidates = Vec::with_capacity(families.len());
    for (index, family) in families.iter().enumerate() {
        let present = match &instance.surface {
            Some(surface) => {
                // SAFETY: the surface and device belong to this instance.
                unsafe {
                    surface.loader.get_physical_device_surface_support(
                        physical,
                        index as u32,
                        surface.raw,
                    )
                }
                .vk("vkGetPhysicalDeviceSurfaceSupportKHR")?
            }
            None => false,
        };
        family_candidates.push(QueueFamilyCandidate {
            graphics: family.queue_flags.contains(vk::QueueFlags::GRAPHICS),
            present,
            queue_count: family.queue_count,
        });
    }
    let name = properties
        .device_name_as_c_str()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown device".into());
    let candidate = DeviceCandidate {
        name,
        device_type: properties.device_type,
        api_version: properties.api_version,
        families: family_candidates,
        has_swapchain_extension: extension_present(&extensions, ash::khr::swapchain::NAME),
        supports_depth_d32: depth
            .optimal_tiling_features
            .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT),
    };
    Ok((candidate, extensions))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(graphics: bool, present: bool) -> QueueFamilyCandidate {
        QueueFamilyCandidate {
            graphics,
            present,
            queue_count: 1,
        }
    }

    fn device(name: &str, kind: vk::PhysicalDeviceType) -> DeviceCandidate {
        DeviceCandidate {
            name: name.into(),
            device_type: kind,
            api_version: vk::API_VERSION_1_3,
            families: vec![family(true, true)],
            has_swapchain_extension: true,
            supports_depth_d32: true,
        }
    }

    #[test]
    fn discrete_beats_integrated_beats_everything_else() {
        let candidates = [
            device("cpu", vk::PhysicalDeviceType::CPU),
            device("igpu", vk::PhysicalDeviceType::INTEGRATED_GPU),
            device("dgpu", vk::PhysicalDeviceType::DISCRETE_GPU),
            device("virtual", vk::PhysicalDeviceType::VIRTUAL_GPU),
        ];
        assert_eq!(select_device(&candidates, true).unwrap().index, 2);
        assert_eq!(select_device(&candidates[..2], false).unwrap().index, 1);
        assert_eq!(
            select_device(&[candidates[3].clone(), candidates[0].clone()], false)
                .unwrap()
                .index,
            0
        );
    }

    #[test]
    fn first_device_wins_a_tie_and_newer_api_breaks_it() {
        let a = device("a", vk::PhysicalDeviceType::DISCRETE_GPU);
        let b = device("b", vk::PhysicalDeviceType::DISCRETE_GPU);
        assert_eq!(
            select_device(&[a.clone(), b.clone()], false).unwrap().index,
            0
        );
        let mut old = a;
        old.api_version = vk::API_VERSION_1_1;
        assert_eq!(select_device(&[old, b], false).unwrap().index, 1);
    }

    #[test]
    fn unusable_devices_are_rejected_with_a_reason() {
        let mut no_graphics = device("compute-only", vk::PhysicalDeviceType::DISCRETE_GPU);
        no_graphics.families = vec![family(false, false)];
        let mut old = device("old", vk::PhysicalDeviceType::DISCRETE_GPU);
        old.api_version = vk::API_VERSION_1_0;
        let mut no_depth = device("no-depth", vk::PhysicalDeviceType::DISCRETE_GPU);
        no_depth.supports_depth_d32 = false;
        let mut no_swapchain = device("no-swapchain", vk::PhysicalDeviceType::DISCRETE_GPU);
        no_swapchain.has_swapchain_extension = false;
        let reasons = select_device(&[no_graphics, old, no_depth, no_swapchain], true).unwrap_err();
        assert_eq!(reasons.len(), 4);
        assert!(reasons[0].contains("no graphics queue"));
        assert!(reasons[1].contains("1.0"));
        assert!(reasons[2].contains("D32"));
        assert!(reasons[3].contains("VK_KHR_swapchain"));
        // Without a window the swapchain extension is not required.
        let mut offscreen = device("headless-ok", vk::PhysicalDeviceType::DISCRETE_GPU);
        offscreen.has_swapchain_extension = false;
        assert!(select_device(&[offscreen], false).is_ok());
    }

    #[test]
    fn windowed_selection_needs_a_graphics_family_that_can_present() {
        let mut split = device("split", vk::PhysicalDeviceType::DISCRETE_GPU);
        split.families = vec![family(true, false), family(false, true)];
        let reasons = select_device(std::slice::from_ref(&split), true).unwrap_err();
        assert!(reasons[0].contains("present"));
        // Offscreen only needs graphics, and picks the first matching family.
        assert_eq!(
            select_device(&[split.clone()], false).unwrap().queue_family,
            0
        );
        split.families = vec![family(true, false), family(true, true)];
        assert_eq!(select_device(&[split], true).unwrap().queue_family, 1);
    }

    #[test]
    fn empty_device_list_has_no_selection_and_no_reasons() {
        assert_eq!(select_device(&[], false).unwrap_err(), Vec::<String>::new());
    }

    #[test]
    fn validation_log_records_and_filters_by_severity() {
        let log = ValidationLog::default();
        assert!(!log.is_active());
        record(&log.state, ValidationSeverity::Warning, "careful".into());
        record(&log.state, ValidationSeverity::Error, "broken".into());
        assert_eq!(log.messages().len(), 2);
        assert_eq!(log.errors(), vec!["broken".to_string()]);
        assert_eq!(log.take().len(), 2);
        assert!(log.messages().is_empty());
        log.set_active(true);
        assert!(log.is_active());
    }

    #[test]
    fn debug_callback_records_messages_with_their_severity_and_tolerates_nulls() {
        let log = ValidationLog::default();
        let user = Arc::as_ptr(&log.state) as *mut c_void;
        let boom = vk::DebugUtilsMessengerCallbackDataEXT::default().message(c"boom");
        let warn = vk::DebugUtilsMessengerCallbackDataEXT::default().message(c"careful");
        let types = vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION;
        // SAFETY: the data structs and the user pointer (an `Arc` kept alive by
        // `log`) are valid for the duration of each call.
        unsafe {
            debug_callback(
                vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                types,
                &boom,
                user,
            );
            debug_callback(
                vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                types,
                &warn,
                user,
            );
            // Missing data or user pointer must be ignored rather than crash.
            debug_callback(
                vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                types,
                std::ptr::null(),
                user,
            );
            debug_callback(
                vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                types,
                &boom,
                std::ptr::null_mut(),
            );
            // A struct without a message text is recorded as an empty message.
            let empty = vk::DebugUtilsMessengerCallbackDataEXT::default();
            debug_callback(
                vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                types,
                &empty,
                user,
            );
        }
        let messages = log.messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].message, "boom");
        assert_eq!(messages[0].severity, ValidationSeverity::Error);
        assert_eq!(messages[1].message, "careful");
        assert_eq!(messages[1].severity, ValidationSeverity::Warning);
        assert_eq!(log.errors(), vec!["boom".to_string(), String::new()]);
    }

    #[test]
    fn missing_loader_library_is_unavailable() {
        let log = ValidationLog::default();
        let config = crate::VulkanConfig::default();
        let result = Instance::new(
            &config,
            None,
            Some(std::ffi::OsStr::new("definitely-not-a-vulkan-loader.dll")),
            &log,
        );
        let Err(RenderError::Unavailable(message)) = result else {
            panic!("expected Unavailable");
        };
        assert!(message.contains("Vulkan loader"), "{message}");
    }
}
