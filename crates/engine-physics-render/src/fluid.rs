//! `physics.fluid`: particle fluids as depth-correct sphere impostors (the particle rendering
//! hook). A screen-space fluid surface is documented as deferred in `docs/physics-rendering.md`.

use crate::buffers::GrowBuffer;
use crate::config::{FluidColorMode, FluidStyle, PhysicsRenderConfig};
use crate::extract::PhysicsRenderExtract;
use crate::gpu::{depth_state, shader_module, view_layout};
use crate::status::{FeatureActivity, FeatureStatus, PhysicsRenderStatusHandle, ShadingReport};
use anyhow::Result;
use engine_project::simulation::physics::FluidRender;
use engine_render_pipeline::{
    DrawStats, FeatureContext, FeatureDescriptor, FeatureSetup, HDR_FORMAT, PipelineFeature, Stage,
    names,
};

pub const NAME: &str = "physics.fluid";
const SHADER: &str = include_str!("wgsl/fluid.wgsl");

/// GPU layout of one particle: `center.xyz`, radius, RGBA.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ParticleInstance {
    pub center: [f32; 3],
    pub radius: f32,
    pub color: [f32; 4],
}

/// Result of [`build_particles`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParticleBatch {
    pub instances: Vec<ParticleInstance>,
    /// Particles beyond the cap.
    pub truncated: bool,
    /// Particles dropped for non-finite data or arrays of unequal length.
    pub rejected: usize,
}

fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// Colour of one particle under `style`. `density` and `speed` are physical values;
/// `surface` selects the emphasised colour.
pub fn particle_color(
    style: &FluidStyle,
    density: f32,
    rest_density: f32,
    speed: f32,
    surface: bool,
) -> [f32; 4] {
    let t = match style.mode {
        FluidColorMode::Density => {
            let ratio = if rest_density > 0.0 {
                density / rest_density
            } else {
                1.0
            };
            (ratio - 0.5).clamp(0.0, 1.0)
        }
        FluidColorMode::Speed => (speed / style.max_speed).clamp(0.0, 1.0),
        FluidColorMode::Flat => 0.0,
    };
    let base = mix(style.low, style.high, t);
    if surface {
        mix(base, style.surface_color, style.surface_emphasis)
    } else {
        base
    }
}

/// Builds instances from every fluid, at most `cap` in total.
pub fn build_particles<'a>(
    fluids: impl Iterator<Item = &'a FluidRender>,
    style: &FluidStyle,
    cap: usize,
) -> ParticleBatch {
    let mut batch = ParticleBatch::default();
    'fluids: for fluid in fluids {
        let count = fluid.positions.len();
        if fluid.velocities.len() != count
            || fluid.densities.len() != count
            || fluid.surface.len() != count
        {
            batch.rejected += count;
            continue;
        }
        let radius = fluid.particle_radius * style.radius_scale;
        for i in 0..count {
            let position = fluid.positions[i];
            if !position.is_finite() || !radius.is_finite() || radius <= 0.0 {
                batch.rejected += 1;
                continue;
            }
            if batch.instances.len() >= cap {
                batch.truncated = true;
                break 'fluids;
            }
            batch.instances.push(ParticleInstance {
                center: position.to_array(),
                radius,
                color: particle_color(
                    style,
                    fluid.densities[i],
                    fluid.rest_density,
                    fluid.velocities[i].length(),
                    fluid.surface[i],
                ),
            });
        }
    }
    batch
}

pub struct FluidFeature {
    config: PhysicsRenderConfig,
    status: PhysicsRenderStatusHandle,
    pipeline: Option<wgpu::RenderPipeline>,
    instances: GrowBuffer,
    uploaded: Option<u64>,
    count: u32,
    last: FeatureStatus,
}

impl FluidFeature {
    pub fn new(config: PhysicsRenderConfig, status: PhysicsRenderStatusHandle) -> Self {
        let cap = config.max_fluid_particles as u64 * size_of::<ParticleInstance>() as u64;
        Self {
            config,
            status,
            pipeline: None,
            instances: GrowBuffer::new("physics fluid particles", wgpu::BufferUsages::VERTEX, cap),
            uploaded: None,
            count: 0,
            last: FeatureStatus::default(),
        }
    }
}

impl PipelineFeature for FluidFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        let mut descriptor = FeatureDescriptor::new(NAME, Stage::Opaque).after(names::OPAQUE);
        if !self.config.enabled.fluid {
            descriptor = descriptor.disabled_by_default();
        }
        descriptor
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let module = shader_module(setup, NAME, SHADER)?;
        let layout = view_layout(setup, NAME);
        let attributes = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
        self.pipeline = Some(setup.gpu.device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some(NAME),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<ParticleInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &attributes,
                    })],
                },
                primitive: Default::default(),
                depth_stencil: Some(depth_state(true, true)),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(HDR_FORMAT.into())],
                }),
                multiview_mask: None,
                cache: None,
            },
        ));
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let cap = self.config.max_fluid_particles;
        let mut status = FeatureStatus {
            cap,
            shading: ShadingReport::Lit,
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
                if self.uploaded != Some(extract.revisions.fluid) {
                    let batch =
                        build_particles(extract.data.fluids.values(), &self.config.fluid, cap);
                    self.instances.write(
                        &ctx.gpu.device,
                        &ctx.gpu.queue,
                        bytemuck::cast_slice(&batch.instances),
                    );
                    self.count = batch.instances.len() as u32;
                    self.uploaded = Some(extract.revisions.fluid);
                    self.last = FeatureStatus {
                        truncated: batch.truncated,
                        reason: (batch.truncated || batch.rejected > 0).then(|| {
                            format!(
                                "{}{}",
                                if batch.truncated {
                                    format!("particles capped at {cap}; ")
                                } else {
                                    String::new()
                                },
                                if batch.rejected > 0 {
                                    format!("{} invalid particles dropped", batch.rejected)
                                } else {
                                    String::new()
                                }
                            )
                            .trim_end_matches("; ")
                            .to_owned()
                        }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn fluid(n: usize, density: f32, speed: f32, surface: bool) -> FluidRender {
        FluidRender {
            positions: (0..n).map(|i| Vec3::new(i as f32, 0.0, 0.0)).collect(),
            velocities: vec![Vec3::new(speed, 0.0, 0.0); n],
            densities: vec![density; n],
            surface: vec![surface; n],
            particle_radius: 0.1,
            rest_density: 1000.0,
            capacity: n,
            domain_min: Vec3::splat(-10.0),
            domain_max: Vec3::splat(10.0),
        }
    }

    #[test]
    fn colour_follows_density_and_speed_between_the_ramp_ends() {
        let style = FluidStyle {
            surface_emphasis: 0.0,
            ..FluidStyle::default()
        };
        assert_eq!(particle_color(&style, 500.0, 1000.0, 0.0, false), style.low);
        assert_eq!(
            particle_color(&style, 1500.0, 1000.0, 0.0, false),
            style.high
        );
        let mid = particle_color(&style, 1000.0, 1000.0, 0.0, false);
        for (i, channel) in mid.iter().enumerate().take(3) {
            assert!((channel - (style.low[i] + style.high[i]) * 0.5).abs() < 1e-6);
        }
        let speed = FluidStyle {
            mode: FluidColorMode::Speed,
            max_speed: 4.0,
            ..style
        };
        assert_eq!(particle_color(&speed, 1.0, 1000.0, 4.0, false), speed.high);
        assert_eq!(particle_color(&speed, 1.0, 1000.0, 0.0, false), speed.low);
        assert_eq!(
            particle_color(&speed, 1.0, 1000.0, 99.0, false),
            speed.high,
            "speed clamps"
        );
    }

    #[test]
    fn surface_particles_move_toward_the_surface_colour_by_the_emphasis() {
        let style = FluidStyle {
            mode: FluidColorMode::Flat,
            surface_emphasis: 0.5,
            ..FluidStyle::default()
        };
        let inner = particle_color(&style, 1000.0, 1000.0, 0.0, false);
        let surface = particle_color(&style, 1000.0, 1000.0, 0.0, true);
        assert_eq!(inner, style.low);
        for (i, channel) in surface.iter().enumerate().take(3) {
            assert!((channel - (style.low[i] + style.surface_color[i]) * 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn particles_carry_radius_scale_and_respect_the_cap_across_fluids() {
        let style = FluidStyle {
            radius_scale: 2.0,
            ..FluidStyle::default()
        };
        let a = fluid(3, 1000.0, 0.0, false);
        let b = fluid(5, 1000.0, 0.0, true);
        let batch = build_particles([&a, &b].into_iter(), &style, 6);
        assert_eq!(batch.instances.len(), 6);
        assert!(batch.truncated);
        assert!((batch.instances[0].radius - 0.2).abs() < 1e-6);
        assert_eq!(batch.instances[2].center, [2.0, 0.0, 0.0]);
        assert_ne!(
            batch.instances[2].color, batch.instances[3].color,
            "surface differs"
        );
        assert_eq!(size_of::<ParticleInstance>(), 32);
        let all = build_particles([&a, &b].into_iter(), &style, 100);
        assert_eq!((all.instances.len(), all.truncated), (8, false));
    }

    #[test]
    fn malformed_fluids_and_non_finite_particles_are_rejected_and_counted() {
        let mut a = fluid(4, 1000.0, 0.0, false);
        a.positions[1] = Vec3::NAN;
        let mut b = fluid(3, 1000.0, 0.0, false);
        b.densities.pop();
        let batch = build_particles([&a, &b].into_iter(), &FluidStyle::default(), 100);
        assert_eq!(batch.instances.len(), 3);
        assert_eq!(batch.rejected, 1 + 3);
    }

    #[test]
    fn the_shader_composes_and_validates() {
        let composer = engine_render_pipeline::ShaderComposer::new();
        let source = composer.compose(SHADER).unwrap();
        engine_render_pipeline::validate_wgsl(&source).unwrap();
    }
}
