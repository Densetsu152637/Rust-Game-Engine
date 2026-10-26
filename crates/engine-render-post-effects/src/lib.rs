//! Bounded HDR depth of field and motion blur passes.
//!
//! Register [`DepthOfFieldFeature`] and [`MotionBlurFeature`] on a
//! [`engine_render_pipeline::ScenePipeline`], then enable their names through
//! per-view feature overrides and set per-view parameters with [`PostEffects`].

use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};
use engine_render_pipeline::{
    FeatureContext, FeatureDescriptor, FeatureNeeds, FeatureSetup, PipelineFeature, Stage, ViewId,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

fn inverse_projection(view: &engine_render_pipeline::ViewData) -> Result<[[f32; 4]; 4]> {
    let inverse = view.clip_from_view.inverse();
    ensure!(
        inverse.is_finite(),
        "post effects require a finite inverse projection"
    );
    let mut previous = -1.0;
    for depth in [0.0, 1.0] {
        let point = inverse * glam::Vec4::new(0.0, 0.0, depth, 1.0);
        ensure!(
            point.w.is_finite() && point.w.abs() > 1e-6,
            "post effects require a finite near and far clip plane"
        );
        let distance = (point.z / point.w).abs();
        ensure!(
            distance.is_finite() && distance > previous,
            "post effects require increasing view-space depth"
        );
        previous = distance;
    }
    Ok(inverse.to_cols_array_2d())
}

pub const DOF_FEATURE: &str = "effects.dof";
pub const MOTION_BLUR_FEATURE: &str = "effects.motion_blur";
pub const MAX_BLUR_RADIUS_PIXELS: f32 = 24.0;
pub const MAX_SHUTTER_SCALE: f32 = 2.0;

/// Focus is a positive distance in view-space units. A pixel at the focus
/// distance is sharp; pixels `focus_range` or more away reach maximum radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthOfFieldSettings {
    pub focus_distance: f32,
    pub focus_range: f32,
    pub max_radius_pixels: f32,
    /// Maximum accepted depth separation between a sample and centre in
    /// view-space units. Prevents blur crossing a silhouette.
    pub edge_tolerance: f32,
}

impl Default for DepthOfFieldSettings {
    fn default() -> Self {
        Self {
            focus_distance: 5.0,
            focus_range: 3.0,
            max_radius_pixels: 8.0,
            edge_tolerance: 0.5,
        }
    }
}

impl DepthOfFieldSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.focus_distance.is_finite() && self.focus_distance > 0.0,
            "focus distance must be positive and finite"
        );
        ensure!(
            self.focus_range.is_finite() && self.focus_range > 0.0,
            "focus range must be positive and finite"
        );
        ensure!(
            self.max_radius_pixels.is_finite()
                && (0.0..=MAX_BLUR_RADIUS_PIXELS).contains(&self.max_radius_pixels),
            "DOF radius must be 0..={MAX_BLUR_RADIUS_PIXELS} pixels"
        );
        ensure!(
            self.edge_tolerance.is_finite() && self.edge_tolerance >= 0.0,
            "DOF edge tolerance must be nonnegative and finite"
        );
        Ok(())
    }
}

/// Symmetric camera shutter along the current-to-previous motion vector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionBlurSettings {
    pub shutter_scale: f32,
    pub max_radius_pixels: f32,
    /// Maximum accepted view-space depth separation along the blur path.
    pub edge_tolerance: f32,
}

impl Default for MotionBlurSettings {
    fn default() -> Self {
        Self {
            shutter_scale: 0.5,
            max_radius_pixels: 16.0,
            edge_tolerance: 0.5,
        }
    }
}

impl MotionBlurSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.shutter_scale.is_finite()
                && (0.0..=MAX_SHUTTER_SCALE).contains(&self.shutter_scale),
            "shutter scale must be 0..={MAX_SHUTTER_SCALE}"
        );
        ensure!(
            self.max_radius_pixels.is_finite()
                && (0.0..=MAX_BLUR_RADIUS_PIXELS).contains(&self.max_radius_pixels),
            "motion blur radius must be 0..={MAX_BLUR_RADIUS_PIXELS} pixels"
        );
        ensure!(
            self.edge_tolerance.is_finite() && self.edge_tolerance >= 0.0,
            "motion blur edge tolerance must be nonnegative and finite"
        );
        Ok(())
    }
}

#[derive(Default)]
struct ViewSettings {
    dof: HashMap<ViewId, DepthOfFieldSettings>,
    motion: HashMap<ViewId, MotionBlurSettings>,
}

/// Cloneable host handle. Absence of per-view settings makes the feature a
/// no-op even if its descriptor is enabled for that view.
#[derive(Clone, Default)]
pub struct PostEffects(Arc<RwLock<ViewSettings>>);

impl PostEffects {
    pub fn set_dof(&self, view: ViewId, settings: Option<DepthOfFieldSettings>) -> Result<()> {
        if let Some(settings) = settings {
            settings.validate()?;
        }
        let mut views = self
            .0
            .write()
            .map_err(|_| anyhow::anyhow!("post effects lock poisoned"))?;
        if let Some(settings) = settings {
            views.dof.insert(view, settings);
        } else {
            views.dof.remove(&view);
        }
        Ok(())
    }

    pub fn set_motion_blur(
        &self,
        view: ViewId,
        settings: Option<MotionBlurSettings>,
    ) -> Result<()> {
        if let Some(settings) = settings {
            settings.validate()?;
        }
        let mut views = self
            .0
            .write()
            .map_err(|_| anyhow::anyhow!("post effects lock poisoned"))?;
        if let Some(settings) = settings {
            views.motion.insert(view, settings);
        } else {
            views.motion.remove(&view);
        }
        Ok(())
    }

    fn dof(&self, view: ViewId) -> Result<Option<DepthOfFieldSettings>> {
        Ok(self
            .0
            .read()
            .map_err(|_| anyhow::anyhow!("post effects lock poisoned"))?
            .dof
            .get(&view)
            .copied())
    }

    fn motion(&self, view: ViewId) -> Result<Option<MotionBlurSettings>> {
        Ok(self
            .0
            .read()
            .map_err(|_| anyhow::anyhow!("post effects lock poisoned"))?
            .motion
            .get(&view)
            .copied())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DofUniform {
    inverse_projection: [[f32; 4]; 4],
    geometry: [f32; 4], // output width/height, render width/height
    focus: [f32; 4],    // distance, range, radius, edge tolerance
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MotionUniform {
    inverse_projection: [[f32; 4]; 4],
    geometry: [f32; 4],
    blur: [f32; 4], // shutter, radius, edge tolerance, reset
}

fn texture_entry(binding: u32, depth: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: if depth {
                wgpu::TextureSampleType::Depth
            } else {
                wgpu::TextureSampleType::Float { filterable: false }
            },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

struct Fullscreen {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

fn prepare_fullscreen(
    setup: &FeatureSetup<'_>,
    label: &str,
    shader: &str,
    motion: bool,
) -> Fullscreen {
    let device = &setup.gpu.device;
    let mut entries = vec![texture_entry(0, false), texture_entry(1, true)];
    if motion {
        entries.push(texture_entry(2, false));
    }
    entries.push(uniform_entry(if motion { 3 } else { 2 }));
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &entries,
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(shader.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: engine_render_pipeline::HDR_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    Fullscreen { layout, pipeline }
}

fn make_uniform(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    bytes: &[u8],
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, bytes);
    buffer
}

fn draw(
    ctx: &mut FeatureContext<'_>,
    name: &str,
    pipeline: &wgpu::RenderPipeline,
    bind: &wgpu::BindGroup,
    destination: &wgpu::TextureView,
) {
    let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(name),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: destination,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind, &[]);
    pass.draw(0..3, 0..1);
}

/// Output-size HDR gather against real scene depth.
pub struct DepthOfFieldFeature {
    controls: PostEffects,
    gpu: Option<Fullscreen>,
}

impl DepthOfFieldFeature {
    pub fn new(controls: PostEffects) -> Self {
        Self {
            controls,
            gpu: None,
        }
    }
}

impl PipelineFeature for DepthOfFieldFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(DOF_FEATURE, Stage::PostProcess)
            .before(MOTION_BLUR_FEATURE)
            .disabled_by_default()
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        self.gpu = Some(prepare_fullscreen(
            setup,
            "depth of field",
            include_str!("dof.wgsl"),
            false,
        ));
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let Some(settings) = self.controls.dof(ctx.view.id)? else {
            return Ok(());
        };
        settings.validate()?;
        if settings.max_radius_pixels == 0.0 {
            return Ok(());
        }
        let gpu = self.gpu.as_ref().context("depth of field not prepared")?;
        let device = &ctx.gpu.device;
        let params = DofUniform {
            inverse_projection: inverse_projection(ctx.view)?,
            geometry: [
                ctx.view.output_size[0] as f32,
                ctx.view.output_size[1] as f32,
                ctx.view.render_size[0] as f32,
                ctx.view.render_size[1] as f32,
            ],
            focus: [
                settings.focus_distance,
                settings.focus_range,
                settings.max_radius_pixels,
                settings.edge_tolerance,
            ],
        };
        let uniform = make_uniform(
            device,
            &ctx.gpu.queue,
            "depth of field params",
            bytemuck::bytes_of(&params),
        );
        let depth = ctx.depth()?.view.clone();
        let (source, destination) = ctx.post_process_io()?;
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("depth of field"),
            layout: &gpu.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&depth),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        draw(ctx, "depth of field", &gpu.pipeline, &bind, &destination);
        Ok(())
    }
}

/// Output-size shutter gather against real render-size motion and depth.
pub struct MotionBlurFeature {
    controls: PostEffects,
    gpu: Option<Fullscreen>,
}

impl MotionBlurFeature {
    pub fn new(controls: PostEffects) -> Self {
        Self {
            controls,
            gpu: None,
        }
    }
}

impl PipelineFeature for MotionBlurFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(MOTION_BLUR_FEATURE, Stage::PostProcess)
            .after(DOF_FEATURE)
            .needs(FeatureNeeds {
                motion: true,
                ..Default::default()
            })
            .disabled_by_default()
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        self.gpu = Some(prepare_fullscreen(
            setup,
            "motion blur",
            include_str!("motion_blur.wgsl"),
            true,
        ));
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let Some(settings) = self.controls.motion(ctx.view.id)? else {
            return Ok(());
        };
        settings.validate()?;
        if settings.shutter_scale == 0.0 || settings.max_radius_pixels == 0.0 {
            return Ok(());
        }
        let gpu = self.gpu.as_ref().context("motion blur not prepared")?;
        let device = &ctx.gpu.device;
        let params = MotionUniform {
            inverse_projection: inverse_projection(ctx.view)?,
            geometry: [
                ctx.view.output_size[0] as f32,
                ctx.view.output_size[1] as f32,
                ctx.view.render_size[0] as f32,
                ctx.view.render_size[1] as f32,
            ],
            blur: [
                settings.shutter_scale,
                settings.max_radius_pixels,
                settings.edge_tolerance,
                if ctx.view.reset_history { 1.0 } else { 0.0 },
            ],
        };
        let uniform = make_uniform(
            device,
            &ctx.gpu.queue,
            "motion blur params",
            bytemuck::bytes_of(&params),
        );
        let depth = ctx.depth()?.view.clone();
        let motion = ctx
            .motion()
            .context("motion blur requires motion vectors")?
            .view
            .clone();
        let (source, destination) = ctx.post_process_io()?;
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("motion blur"),
            layout: &gpu.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&depth),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&motion),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        draw(ctx, "motion blur", &gpu.pipeline, &bind, &destination);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_nonfinite_and_out_of_range_settings() {
        assert!(
            DepthOfFieldSettings {
                focus_distance: f32::NAN,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            DepthOfFieldSettings {
                max_radius_pixels: MAX_BLUR_RADIUS_PIXELS + 1.0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            MotionBlurSettings {
                shutter_scale: -1.0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            MotionBlurSettings {
                edge_tolerance: f32::INFINITY,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn controls_keep_views_independent_and_reject_without_mutation() {
        let controls = PostEffects::default();
        let a = ViewId(3);
        let b = ViewId(4);
        controls
            .set_dof(a, Some(DepthOfFieldSettings::default()))
            .unwrap();
        assert!(controls.dof(a).unwrap().is_some());
        assert!(controls.dof(b).unwrap().is_none());
        assert!(
            controls
                .set_dof(
                    a,
                    Some(DepthOfFieldSettings {
                        focus_range: 0.0,
                        ..Default::default()
                    })
                )
                .is_err()
        );
        assert!(controls.dof(a).unwrap().is_some());
        controls.set_dof(a, None).unwrap();
        assert!(controls.dof(a).unwrap().is_none());
    }
}
