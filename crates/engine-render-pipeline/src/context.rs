//! What a feature sees: device, encoder, view data, frame textures and the
//! stage's input/output.

use crate::feature::Stage;
use crate::instancing::DrawStats;
use crate::shader::ShaderComposer;
use crate::targets::{FrameTargets, Target};
use crate::view::{ViewData, Viewport};
use anyhow::{Result, bail, ensure};
use bevy_ecs::world::World;
use engine_render_wgpu::GpuContext;

/// Bind group layouts features build pipelines against.
pub struct PipelineLayouts {
    /// `engine::view` (group 0): view uniform, light list, shadows, atlas.
    pub view: wgpu::BindGroupLayout,
    /// `engine::mesh` instances (group 3): `array<EngineInstance>`.
    pub instances: wgpu::BindGroupLayout,
    /// `engine::material` textures (group 1).
    pub material_textures: wgpu::BindGroupLayout,
    /// `engine::material` uniforms (group 2, dynamic offsets).
    pub material_params: wgpu::BindGroupLayout,
}

/// Passed to [`crate::PipelineFeature::prepare`] once per device.
pub struct FeatureSetup<'a> {
    pub gpu: &'a GpuContext,
    pub layouts: &'a PipelineLayouts,
    /// Engine modules (and the 3D `engine::material`); compose feature
    /// shaders that `#import engine::view` with it.
    pub composer: &'a ShaderComposer,
}

/// The target the output and overlay stages draw into.
#[derive(Clone, Debug)]
pub struct OutputTarget {
    pub view: wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    /// Full target size in pixels.
    pub size: [u32; 2],
    /// Where the 3D view lands in the target (letterboxing outside it).
    pub viewport: Viewport,
}

/// Frame textures and stage I/O of one view, shared by its features.
#[derive(Default)]
pub(crate) struct FrameIo {
    /// Taken from the view state for the frame and returned after it.
    pub targets: Option<FrameTargets>,
    /// HDR colour the next post/tonemap feature reads.
    pub current_hdr: Option<wgpu::TextureView>,
    /// Which texture `current_hdr` is.
    pub source: HdrSource,
    pub output: Option<OutputTarget>,
}

/// Which texture holds the current HDR colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum HdrSource {
    #[default]
    Scene,
    Upscaled,
    /// Supplied by the host (for example the DLSS output); not readable back.
    Host,
    Post(usize),
}

/// Per-frame, per-view context of [`crate::PipelineFeature::encode`].
pub struct FeatureContext<'a> {
    pub gpu: &'a GpuContext,
    /// Encoder of the current stage segment; submitted in stage order.
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub stage: Stage,
    pub view: &'a ViewData,
    /// `engine::view` bind group of this view (group 0). In the Shadow stage
    /// its `engine_shadow_atlas` is a 1x1 placeholder, because the atlas is
    /// being rendered.
    pub view_bind_group: &'a wgpu::BindGroup,
    pub layouts: &'a PipelineLayouts,
    /// The extracted ECS world, when the host supplies one. Read only.
    pub world: Option<&'a World>,
    pub(crate) io: &'a mut FrameIo,
    pub(crate) stats: &'a mut DrawStats,
    pub(crate) name: &'a str,
}

impl<'a> FeatureContext<'a> {
    /// Name of the running feature.
    pub fn feature_name(&self) -> &str {
        self.name
    }

    fn targets(&self) -> Result<&FrameTargets> {
        match self.io.targets.as_ref() {
            Some(targets) => Ok(targets),
            None => bail!("{}: this frame has no 3D view (2D-only frame)", self.name),
        }
    }

    fn scene_stage(&self) -> Result<()> {
        ensure!(
            matches!(
                self.stage,
                Stage::Prepare
                    | Stage::Shadow
                    | Stage::Prepass
                    | Stage::Opaque
                    | Stage::Sky
                    | Stage::Transparent
            ),
            "{}: scene targets are written in the scene stages, not {}",
            self.name,
            self.stage.name()
        );
        Ok(())
    }

    /// HDR scene colour (`Rgba16Float`, render size).
    pub fn hdr(&self) -> Result<&Target> {
        Ok(&self.targets()?.hdr)
    }

    /// Scene depth (`Depth32Float`, render size). Complete after the
    /// Transparent stage; sampled reads are valid outside passes writing it.
    pub fn depth(&self) -> Result<&Target> {
        Ok(&self.targets()?.depth)
    }

    /// World normals (`Rgba16Float`); `None` unless a running feature
    /// declared `needs.normals`. Written by `engine.prepass.normals`.
    pub fn normals(&self) -> Option<&Target> {
        self.io.targets.as_ref()?.normals.as_ref()
    }

    /// Motion vectors (`Rg16Float`, render pixels); `None` unless a feature
    /// or the host needs motion. Written by `engine.prepass.motion`.
    pub fn motion(&self) -> Option<&Target> {
        self.io.targets.as_ref()?.motion.as_ref()
    }

    pub fn render_size(&self) -> [u32; 2] {
        self.view.render_size
    }

    /// A render pass over HDR colour and scene depth (both loaded and
    /// stored) with the viewport set to the render size. Scene stages only.
    pub fn scene_pass(&mut self, label: &str) -> Result<wgpu::RenderPass<'static>> {
        self.scene_stage()?;
        let targets = self.targets()?;
        let (hdr, depth) = (targets.hdr.view.clone(), targets.depth.view.clone());
        let pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &hdr,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        Ok(pass.forget_lifetime())
    }

    /// `(source, destination)` for an HDR post-processing feature: read the
    /// current HDR colour, write the full output-size destination (which
    /// becomes current). PostProcess stage only.
    pub fn post_process_io(&mut self) -> Result<(wgpu::TextureView, wgpu::TextureView)> {
        ensure!(
            self.stage == Stage::PostProcess,
            "{}: post_process_io is for the post_process stage",
            self.name
        );
        let source = self
            .io
            .current_hdr
            .clone()
            .ok_or_else(|| anyhow::anyhow!("{}: no HDR colour this frame", self.name))?;
        let index = match self.io.source {
            HdrSource::Post(0) => 1,
            _ => 0,
        };
        let device = self.gpu.device.clone();
        let targets = self
            .io
            .targets
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("{}: no 3D view this frame", self.name))?;
        let destination = targets.ensure_post(&device, index).view.clone();
        self.io.current_hdr = Some(destination.clone());
        self.io.source = HdrSource::Post(index);
        Ok((source, destination))
    }

    /// `(render-size HDR, output-size destination)` for an upscale feature;
    /// the destination becomes the current HDR colour. Upscale stage only.
    pub fn upscale_io(&mut self) -> Result<(wgpu::TextureView, wgpu::TextureView)> {
        ensure!(
            self.stage == Stage::Upscale,
            "{}: upscale_io is for the upscale stage",
            self.name
        );
        let device = self.gpu.device.clone();
        let targets = self
            .io
            .targets
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("{}: no 3D view this frame", self.name))?;
        let source = targets.hdr.view.clone();
        let destination = targets.ensure_upscaled(&device).view.clone();
        self.io.current_hdr = Some(destination.clone());
        self.io.source = HdrSource::Upscaled;
        Ok((source, destination))
    }

    /// The current HDR colour at the output size (Tonemap: the output
    /// operator's input).
    pub fn hdr_input(&self) -> Result<wgpu::TextureView> {
        self.io
            .current_hdr
            .clone()
            .ok_or_else(|| anyhow::anyhow!("{}: no HDR colour this frame", self.name))
    }

    /// The output target (Tonemap and Overlay stages).
    pub fn output(&self) -> Result<&OutputTarget> {
        ensure!(
            matches!(self.stage, Stage::Tonemap | Stage::Overlay),
            "{}: the output target is for the tonemap and overlay stages",
            self.name
        );
        self.io
            .output
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("{}: no output target", self.name))
    }

    /// A pass that clears the whole output target to opaque black and sets
    /// the viewport to the view's rectangle: what an output operator draws
    /// its fullscreen triangle in. Tonemap stage only.
    pub fn output_pass(&mut self, label: &str) -> Result<wgpu::RenderPass<'static>> {
        ensure!(
            self.stage == Stage::Tonemap,
            "{}: output_pass is for the tonemap stage",
            self.name
        );
        let output = self.output()?.clone();
        let mut pass = self
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            })
            .forget_lifetime();
        let v = output.viewport;
        pass.set_viewport(
            v.x as f32,
            v.y as f32,
            v.width as f32,
            v.height as f32,
            0.0,
            1.0,
        );
        Ok(pass)
    }

    /// Add instanced draw statistics to this view's report.
    pub fn record_draws(&mut self, stats: DrawStats) {
        *self.stats += stats;
    }
}
