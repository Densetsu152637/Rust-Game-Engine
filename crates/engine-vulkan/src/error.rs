//! Mapping of Vulkan, allocator and loader failures onto [`RenderError`].
//!
//! Only two kinds of failure get their own variant because callers react to
//! them: [`RenderError::Unavailable`] (nothing to render with, with an
//! actionable message) and [`RenderError::DeviceLost`] (recreate everything).
//! Everything else is a [`RenderError::Backend`] carrying the failing call.

use ash::vk;
use engine_render::{RenderError, Result};
use gpu_allocator::AllocationError;

/// Map a `vk::Result` returned by `context` (usually the Vulkan function name).
pub(crate) fn from_vk(context: &str, result: vk::Result) -> RenderError {
    match result {
        vk::Result::ERROR_DEVICE_LOST => {
            RenderError::DeviceLost(format!("{context}: VK_ERROR_DEVICE_LOST"))
        }
        vk::Result::ERROR_OUT_OF_HOST_MEMORY => {
            RenderError::Backend(format!("{context}: out of host memory"))
        }
        vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => {
            RenderError::Backend(format!("{context}: out of GPU memory"))
        }
        vk::Result::ERROR_SURFACE_LOST_KHR => RenderError::Backend(format!(
            "{context}: the presentation surface was lost (the window may have been destroyed)"
        )),
        other => RenderError::Backend(format!("{context}: {other:?}")),
    }
}

/// Map a failure while bringing Vulkan up (instance or device creation). A
/// driver that cannot provide Vulkan at all is `Unavailable`, not a bug.
pub(crate) fn init_failure(context: &str, result: vk::Result) -> RenderError {
    match result {
        vk::Result::ERROR_INCOMPATIBLE_DRIVER
        | vk::Result::ERROR_INITIALIZATION_FAILED
        | vk::Result::ERROR_LAYER_NOT_PRESENT
        | vk::Result::ERROR_EXTENSION_NOT_PRESENT
        | vk::Result::ERROR_FEATURE_NOT_PRESENT => RenderError::Unavailable(format!(
            "{context} failed with {result:?}; no usable Vulkan driver is installed or it is too old. \
             Install or update the GPU driver (it ships the Vulkan runtime) and try again"
        )),
        other => from_vk(context, other),
    }
}

/// Map an allocator failure.
pub(crate) fn from_alloc(context: &str, error: AllocationError) -> RenderError {
    match error {
        AllocationError::OutOfMemory => {
            RenderError::Backend(format!("{context}: out of GPU or host memory"))
        }
        other => RenderError::Backend(format!("{context}: {other}")),
    }
}

/// The Vulkan loader library could not be loaded.
pub(crate) fn loader_unavailable(detail: &dyn std::fmt::Display) -> RenderError {
    RenderError::Unavailable(format!(
        "the Vulkan loader could not be loaded ({detail}); install or update the GPU driver or the \
         Vulkan runtime (vulkan-1.dll on Windows, libvulkan.so.1 on Linux, MoltenVK on macOS)"
    ))
}

/// No physical device passed selection; `reasons` has one line per rejected device.
pub(crate) fn no_suitable_device(reasons: &[String]) -> RenderError {
    let detail = if reasons.is_empty() {
        "the Vulkan loader reported no physical devices".to_string()
    } else {
        reasons.join("; ")
    };
    RenderError::Unavailable(format!(
        "no suitable Vulkan device found ({detail}); install or update the GPU driver, or run on \
         hardware that supports Vulkan 1.1 with a graphics queue, a D32 depth format and (for \
         windows) presentation support"
    ))
}

/// Result extension that tags `vk::Result` errors with the failing call.
pub(crate) trait VkCheck<T> {
    fn vk(self, context: &str) -> Result<T>;
}

impl<T> VkCheck<T> for std::result::Result<T, vk::Result> {
    fn vk(self, context: &str) -> Result<T> {
        self.map_err(|result| from_vk(context, result))
    }
}

/// Result extension for allocator calls.
pub(crate) trait AllocCheck<T> {
    fn alloc(self, context: &str) -> Result<T>;
}

impl<T> AllocCheck<T> for std::result::Result<T, AllocationError> {
    fn alloc(self, context: &str) -> Result<T> {
        self.map_err(|error| from_alloc(context, error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_lost_maps_to_its_own_variant() {
        let error = from_vk("vkQueueSubmit", vk::Result::ERROR_DEVICE_LOST);
        assert!(matches!(error, RenderError::DeviceLost(ref m) if m.contains("vkQueueSubmit")));
    }

    #[test]
    fn memory_exhaustion_is_a_backend_failure_naming_the_call() {
        let error = from_vk("vkAllocateMemory", vk::Result::ERROR_OUT_OF_DEVICE_MEMORY);
        let RenderError::Backend(message) = error else {
            panic!("expected Backend");
        };
        assert!(message.contains("vkAllocateMemory") && message.contains("GPU memory"));
        let error = from_alloc("buffer", AllocationError::OutOfMemory);
        assert!(matches!(error, RenderError::Backend(ref m) if m.contains("out of")));
    }

    #[test]
    fn incompatible_driver_at_init_is_unavailable_with_advice() {
        let error = init_failure("vkCreateInstance", vk::Result::ERROR_INCOMPATIBLE_DRIVER);
        let RenderError::Unavailable(message) = error else {
            panic!("expected Unavailable");
        };
        assert!(message.contains("driver"), "{message}");
        // The same code after init is an ordinary backend failure.
        assert!(matches!(
            from_vk("vkCreateImage", vk::Result::ERROR_INITIALIZATION_FAILED),
            RenderError::Backend(_)
        ));
    }

    #[test]
    fn missing_loader_message_says_what_is_missing_and_how_to_fix_it() {
        let RenderError::Unavailable(message) = loader_unavailable(&"not found") else {
            panic!("expected Unavailable");
        };
        assert!(message.contains("Vulkan loader"), "{message}");
        assert!(message.contains("not found"), "{message}");
        assert!(message.contains("install or update"), "{message}");
    }

    #[test]
    fn no_device_message_lists_every_rejection() {
        let reasons = vec!["GPU A: no graphics queue".into(), "GPU B: API 1.0".into()];
        let RenderError::Unavailable(message) = no_suitable_device(&reasons) else {
            panic!("expected Unavailable");
        };
        assert!(message.contains("GPU A: no graphics queue"));
        assert!(message.contains("GPU B: API 1.0"));
        let RenderError::Unavailable(empty) = no_suitable_device(&[]) else {
            panic!("expected Unavailable");
        };
        assert!(empty.contains("no physical devices"));
    }

    #[test]
    fn check_extensions_tag_the_context() {
        let failed: std::result::Result<(), vk::Result> = Err(vk::Result::ERROR_DEVICE_LOST);
        assert!(matches!(
            failed.vk("vkWaitForFences"),
            Err(RenderError::DeviceLost(_))
        ));
        let ok: std::result::Result<u8, vk::Result> = Ok(3);
        assert_eq!(ok.vk("x").unwrap(), 3);
    }
}
