//! `physics.debug_lines`: the physics debug view and deformable debug segments as lines.

use crate::buffers::GrowBuffer;
use crate::config::PhysicsRenderConfig;
use crate::extract::{DebugState, PhysicsRenderExtract};
use crate::gpu::{depth_state, shader_module, view_layout};
use crate::status::{FeatureActivity, FeatureStatus, PhysicsRenderStatusHandle};
use anyhow::Result;
use engine_project::simulation::physics::DebugSegment;
use engine_render_pipeline::{
    DrawStats, FeatureContext, FeatureDescriptor, FeatureSetup, HDR_FORMAT, PipelineFeature, Stage,
    names,
};

pub const NAME: &str = "physics.debug_lines";
const SHADER: &str = include_str!("wgsl/lines.wgsl");

/// GPU layout of one line: `from.xyz`, pixel width, `to.xyz`, padding, RGBA.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineInstance {
    pub from: [f32; 3],
    pub width: f32,
    pub to: [f32; 3],
    pub pad: f32,
    pub color: [f32; 4],
}

/// Result of [`build_lines`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LineBatch {
    pub instances: Vec<LineInstance>,
    /// More segments were given than `cap`.
    pub truncated: bool,
    /// Segments dropped for non-finite coordinates or colours.
    pub rejected: usize,
}

/// Converts segments to instances: at most `cap`, finite only, with `width` pixels.
pub fn build_lines(segments: &[DebugSegment], width: f32, cap: usize) -> LineBatch {
    let mut batch = LineBatch {
        instances: Vec::with_capacity(segments.len().min(cap)),
        ..Default::default()
    };
    for segment in segments {
        let finite = segment.from.is_finite()
            && segment.to.is_finite()
            && segment.color.iter().all(|c| c.is_finite());
        if !finite {
            batch.rejected += 1;
            continue;
        }
        if batch.instances.len() >= cap {
            batch.truncated = true;
            break;
        }
        batch.instances.push(LineInstance {
            from: segment.from.to_array(),
            width,
            to: segment.to.to_array(),
            pad: 0.0,
            color: segment.color,
        });
    }
    batch
}

pub struct DebugLinesFeature {
    config: PhysicsRenderConfig,
    status: PhysicsRenderStatusHandle,
    pipeline: Option<wgpu::RenderPipeline>,
    instances: GrowBuffer,
    uploaded: Option<u64>,
    count: u32,
    last: FeatureStatus,
}

impl DebugLinesFeature {
    pub fn new(config: PhysicsRenderConfig, status: PhysicsRenderStatusHandle) -> Self {
        let cap = config.max_debug_segments as u64 * size_of::<LineInstance>() as u64;
        Self {
            config,
            status,
            pipeline: None,
            instances: GrowBuffer::new("physics debug lines", wgpu::BufferUsages::VERTEX, cap),
            uploaded: None,
            count: 0,
            last: FeatureStatus::default(),
        }
    }
}

impl PipelineFeature for DebugLinesFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        let mut descriptor =
            FeatureDescriptor::new(NAME, Stage::Transparent).after(names::TRANSPARENT);
        if !self.config.enabled.debug_lines {
            descriptor = descriptor.disabled_by_default();
        }
        descriptor
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let module = shader_module(setup, NAME, SHADER)?;
        let layout = view_layout(setup, NAME);
        let attributes = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4];
        self.pipeline = Some(setup.gpu.device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some(NAME),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<LineInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &attributes,
                    })],
                },
                primitive: Default::default(),
                depth_stencil: Some(depth_state(false, self.config.debug_depth_test)),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            },
        ));
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let cap = self.config.max_debug_segments;
        let mut status = FeatureStatus {
            cap,
            ..Default::default()
        };
        let extract = ctx
            .world
            .and_then(|w| w.get_resource::<PhysicsRenderExtract>());
        match extract {
            None => {
                status.activity = FeatureActivity::Skipped;
                status.reason = Some(
                    "no PhysicsRenderExtract in the render world; call install_physics_extraction"
                        .into(),
                );
                self.count = 0;
            }
            Some(extract) => {
                if self.uploaded != Some(extract.revisions.debug) {
                    let batch = build_lines(&extract.debug, self.config.debug_line_width, cap);
                    let whole = self.instances.write(
                        &ctx.gpu.device,
                        &ctx.gpu.queue,
                        bytemuck::cast_slice(&batch.instances),
                    );
                    self.count = (self.instances.len / size_of::<LineInstance>() as u64) as u32;
                    self.uploaded = Some(extract.revisions.debug);
                    self.last = FeatureStatus {
                        truncated: batch.truncated || extract.debug_truncated || !whole,
                        reason: line_reason(extract, &batch),
                        ..Default::default()
                    };
                }
                status.truncated = self.last.truncated;
                status.reason = self.last.reason.clone();
                status.primitives = self.count as usize;
                status.activity = if self.count > 0 {
                    FeatureActivity::Drew
                } else {
                    FeatureActivity::Idle
                };
            }
        }
        if self.count > 0
            && let (Some(pipeline), Some(buffer)) = (&self.pipeline, self.instances.buffer())
        {
            let mut pass = ctx.scene_pass(NAME)?;
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, ctx.view_bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..self.instances.len));
            pass.draw(0..6, 0..self.count);
            drop(pass);
            ctx.record_draws(DrawStats {
                draw_calls: 1,
                instances: self.count as usize,
            });
        }
        self.status.set(NAME, status);
        Ok(())
    }
}

fn line_reason(extract: &PhysicsRenderExtract, batch: &LineBatch) -> Option<String> {
    let mut reasons = Vec::new();
    match extract.debug_state {
        DebugState::On => {}
        DebugState::Off => reasons.push(format!(
            "debug view unavailable: {}",
            extract.debug_reason.as_deref().unwrap_or("unknown")
        )),
        DebugState::Disabled => reasons.push("debug lines disabled in the config".to_owned()),
    }
    if batch.rejected > 0 {
        reasons.push(format!("{} non-finite segments dropped", batch.rejected));
    }
    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn segment(x: f32) -> DebugSegment {
        DebugSegment {
            from: Vec3::new(x, 0.0, 0.0),
            to: Vec3::new(x, 1.0, 0.0),
            color: [1.0, 0.5, 0.25, 1.0],
        }
    }

    #[test]
    fn instances_carry_endpoints_width_and_colour() {
        let batch = build_lines(&[segment(2.0)], 3.0, 10);
        assert_eq!(batch.instances.len(), 1);
        let line = batch.instances[0];
        assert_eq!(line.from, [2.0, 0.0, 0.0]);
        assert_eq!(line.to, [2.0, 1.0, 0.0]);
        assert_eq!(line.width, 3.0);
        assert_eq!(line.color, [1.0, 0.5, 0.25, 1.0]);
        assert_eq!(size_of::<LineInstance>(), 48);
        assert!(!batch.truncated);
    }

    #[test]
    fn the_cap_truncates_and_non_finite_segments_are_dropped() {
        let mut segments: Vec<_> = (0..10).map(|i| segment(i as f32)).collect();
        segments.insert(3, segment(f32::NAN));
        let batch = build_lines(&segments, 1.0, 4);
        assert_eq!(batch.instances.len(), 4);
        assert!(batch.truncated);
        assert_eq!(batch.rejected, 1);
        let all = build_lines(&segments, 1.0, 100);
        assert_eq!(
            (all.instances.len(), all.truncated, all.rejected),
            (10, false, 1)
        );
    }

    #[test]
    fn the_shader_composes_and_validates() {
        let composer = engine_render_pipeline::ShaderComposer::new();
        let source = composer.compose(SHADER).unwrap();
        engine_render_pipeline::validate_wgsl(&source).unwrap();
    }
}
