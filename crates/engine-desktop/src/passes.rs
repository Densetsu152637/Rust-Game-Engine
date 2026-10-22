use crate::{GpuContext, RenderTargetInfo};
use anyhow::{Result, ensure};
use engine_core::bevy_ecs::{prelude::Resource, world::World};

/// Native extension point with unrestricted device, queue and encoder access.
///
/// Native passes are **wgpu-specific by design**: they receive the raw
/// `wgpu::Device`, queue, command encoder and texture view. They run only when
/// the wgpu backend is selected; starting the runner with another backend while
/// passes are registered is an error rather than silently skipping them.
///
/// `prepare` runs on registration and resize/format change. `encode` runs after
/// engine draws in stable registration order, on the host thread. Implementations
/// own their pipelines/buffers and may issue render, compute and transfer passes.
/// Use LoadOp::Load to composite over engine output. Return errors to stop the
/// runner; all owned objects then drop normally. Custom passes may read the
/// extracted ECS world, but must not mutate simulation state during rendering.
pub trait NativeRenderPass: Send + Sync + 'static {
    fn prepare(&mut self, gpu: &GpuContext, target: RenderTargetInfo) -> Result<()>;
    fn encode(
        &mut self,
        gpu: &GpuContext,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        info: RenderTargetInfo,
        world: Option<&World>,
    ) -> Result<()>;
}

/// Insert this resource from external gameplay configuration. The runner takes
/// ownership at startup. Names are unique, order is insertion order.
#[derive(Resource, Default)]
pub struct NativeRenderPasses {
    pub(crate) passes: Vec<(String, Box<dyn NativeRenderPass>)>,
}

impl NativeRenderPasses {
    pub fn register(&mut self, name: impl Into<String>, pass: impl NativeRenderPass) -> Result<()> {
        let name = name.into();
        ensure!(!name.is_empty(), "render pass name is empty");
        ensure!(
            !self.passes.iter().any(|(n, _)| n == &name),
            "duplicate render pass {name}"
        );
        ensure!(self.passes.len() < 256, "native render pass limit exceeded");
        self.passes.push((name, Box::new(pass)));
        Ok(())
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    /// Registration never touches the device, so a stub with no GPU is enough.
    struct Stub;
    impl NativeRenderPass for Stub {
        fn prepare(&mut self, _: &GpuContext, _: RenderTargetInfo) -> Result<()> {
            Ok(())
        }
        fn encode(
            &mut self,
            _: &GpuContext,
            _: &mut wgpu::CommandEncoder,
            _: &wgpu::TextureView,
            _: RenderTargetInfo,
            _: Option<&World>,
        ) -> Result<()> {
            Ok(())
        }
    }

    fn names(passes: &NativeRenderPasses) -> Vec<&str> {
        passes
            .passes
            .iter()
            .map(|(name, _)| name.as_str())
            .collect()
    }

    #[test]
    fn new_registry_is_empty() {
        assert!(NativeRenderPasses::default().passes.is_empty());
    }

    #[test]
    fn registration_preserves_insertion_order() {
        let mut passes = NativeRenderPasses::default();
        for name in ["shadow", "bloom", "ui-overlay"] {
            passes.register(name, Stub).unwrap();
        }
        assert_eq!(names(&passes), ["shadow", "bloom", "ui-overlay"]);
    }

    #[test]
    fn empty_name_is_rejected_and_does_not_register() {
        let mut passes = NativeRenderPasses::default();
        let error = passes.register("", Stub).unwrap_err();
        assert!(error.to_string().contains("name is empty"), "{error}");
        assert!(passes.passes.is_empty());
    }

    #[test]
    fn duplicate_name_is_rejected_with_the_name_in_the_message() {
        let mut passes = NativeRenderPasses::default();
        passes.register("bloom", Stub).unwrap();
        let error = passes.register("bloom", Stub).unwrap_err();
        assert!(
            error.to_string().contains("duplicate render pass bloom"),
            "{error}"
        );
        assert_eq!(names(&passes), ["bloom"]);
    }

    #[test]
    fn names_are_case_sensitive() {
        let mut passes = NativeRenderPasses::default();
        passes.register("Bloom", Stub).unwrap();
        passes.register("bloom", Stub).unwrap();
        assert_eq!(passes.passes.len(), 2);
    }

    #[test]
    fn rejected_duplicate_leaves_later_registration_order_intact() {
        let mut passes = NativeRenderPasses::default();
        passes.register("a", Stub).unwrap();
        assert!(passes.register("a", Stub).is_err());
        passes.register("b", Stub).unwrap();
        assert_eq!(names(&passes), ["a", "b"]);
    }

    #[test]
    fn registry_accepts_exactly_256_passes() {
        let mut passes = NativeRenderPasses::default();
        for index in 0..256 {
            passes.register(format!("pass-{index}"), Stub).unwrap();
        }
        let error = passes.register("one-too-many", Stub).unwrap_err();
        assert!(error.to_string().contains("limit exceeded"), "{error}");
        assert_eq!(passes.passes.len(), 256);
    }

    #[test]
    fn render_target_info_is_copy_and_keeps_its_fields() {
        let info = RenderTargetInfo {
            width: 64,
            height: 32,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
        };
        let copy = info;
        assert_eq!(
            (copy.width, copy.height, copy.format),
            (64, 32, info.format)
        );
    }
}
