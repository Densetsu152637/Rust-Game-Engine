use crate::GpuContext;
use anyhow::{Result, ensure};
use engine_core::bevy_ecs::{prelude::Resource, world::World};

#[derive(Clone, Copy, Debug)]
pub struct RenderTargetInfo {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}

/// Native extension point with unrestricted device, queue and encoder access.
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
