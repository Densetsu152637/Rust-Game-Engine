//! The `engine.particles` GPU side: per-emitter pools, the tick entry point
//! that records the compute passes, the blocking readback, and the Transparent
//! stage billboard pass.
//!
//! Simulation and drawing share only the pools. [`ParticleShared::advance`]
//! encodes and submits its own commands and is driven by the host's scene
//! tick; the render feature never touches the simulation, so any number of
//! views or presentations draw the same state.

use super::{
    COUNTERS_BYTES, EMITTER_UNIFORM_BYTES, EmitterCursor, GpuParticle, PARTICLE_BYTES,
    ParticleBlend, ParticleCounters, ParticleEmitter, ParticleEmitterReport, ParticleReadback,
    ParticleReport, ParticleTick, ParticleTickReport, ParticleViewReport, WORKGROUP_SIZE,
    emitter_uniform, validate_tick,
};
use crate::context::{FeatureContext, FeatureSetup};
use crate::feature::{FeatureBody, FeatureDescriptor, FeatureRegistry, PipelineFeature, Stage};
use crate::instancing::DrawStats;
use crate::pipeline::names;
use crate::shader::{PARTICLES_DRAW_WGSL, PARTICLES_SIM_WGSL, validate_wgsl};
use crate::targets::{DEPTH_FORMAT, HDR_FORMAT};
use crate::view::ViewId;
use anyhow::{Context, Result, anyhow};
use engine_render_wgpu::GpuContext;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

/// One live emitter's buffers and CPU clock.
struct EmitterGpu {
    emitter: ParticleEmitter,
    cursor: EmitterCursor,
    ticks: u64,
    params: wgpu::Buffer,
    particles: wgpu::Buffer,
    counters: wgpu::Buffer,
    sim_bind: wgpu::BindGroup,
    draw_bind: wgpu::BindGroup,
}

#[derive(Default)]
struct State {
    emitters: BTreeMap<String, EmitterGpu>,
    last_tick: Option<u64>,
    simulated_ticks: u64,
    repeated_ticks: u64,
}

/// Pipelines, layouts and state shared by [`crate::ScenePipeline`] and the
/// render feature.
pub(crate) struct ParticleShared {
    gpu: GpuContext,
    sim_layout: wgpu::BindGroupLayout,
    draw_layout: wgpu::BindGroupLayout,
    integrate: wgpu::ComputePipeline,
    emit: wgpu::ComputePipeline,
    alpha: wgpu::RenderPipeline,
    additive: wgpu::RenderPipeline,
    state: Mutex<State>,
    views: Mutex<HashMap<ViewId, ParticleViewReport>>,
}

pub(crate) fn descriptor() -> FeatureDescriptor {
    FeatureDescriptor::new(names::PARTICLES, Stage::Transparent)
        .after(names::TRANSPARENT)
        .disabled_by_default()
}

fn storage_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(EMITTER_UNIFORM_BYTES),
        },
        count: None,
    }
}

fn blend_state(blend: ParticleBlend) -> wgpu::BlendState {
    match blend {
        ParticleBlend::Alpha => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        },
        ParticleBlend::Additive => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        },
    }
}

impl ParticleShared {
    pub fn new(setup: &FeatureSetup<'_>) -> Result<Self> {
        let gpu = setup.gpu.clone();
        let device = &gpu.device;
        let sim_source = setup.composer.compose(PARTICLES_SIM_WGSL)?;
        validate_wgsl(&sim_source)?;
        let draw_source = setup.composer.compose(PARTICLES_DRAW_WGSL)?;
        validate_wgsl(&draw_source)?;
        let sim_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("engine particles simulation"),
            source: wgpu::ShaderSource::Wgsl(sim_source.into()),
        });
        let draw_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("engine particles draw"),
            source: wgpu::ShaderSource::Wgsl(draw_source.into()),
        });
        let compute = wgpu::ShaderStages::COMPUTE;
        let sim_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine particles simulation"),
            entries: &[
                uniform_entry(0, compute),
                storage_entry(1, compute, false),
                storage_entry(2, compute, false),
            ],
        });
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine particles draw"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX),
                storage_entry(1, wgpu::ShaderStages::VERTEX, true),
            ],
        });
        let sim_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("engine particles simulation"),
            bind_group_layouts: &[Some(&sim_layout)],
            immediate_size: 0,
        });
        let compute_pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&sim_pipeline_layout),
                module: &sim_module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let integrate = compute_pipeline("integrate");
        let emit = compute_pipeline("emit");
        let draw_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("engine particles draw"),
            bind_group_layouts: &[Some(&setup.layouts.view), Some(&draw_layout)],
            immediate_size: 0,
        });
        let render_pipeline = |blend: ParticleBlend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("engine particles draw"),
                layout: Some(&draw_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &draw_module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &draw_module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend: Some(blend_state(blend)),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let alpha = render_pipeline(ParticleBlend::Alpha);
        let additive = render_pipeline(ParticleBlend::Additive);
        Ok(Self {
            gpu,
            sim_layout,
            draw_layout,
            integrate,
            emit,
            alpha,
            additive,
            state: Mutex::new(State::default()),
            views: Mutex::new(HashMap::new()),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow!("the particle state lock was poisoned"))
    }

    /// Whether any emitter has spawned (or may have spawned) a particle.
    pub fn has_particles(&self) -> bool {
        self.state.lock().is_ok_and(|state| {
            state
                .emitters
                .values()
                .any(|e| e.cursor.high_water(e.emitter.capacity) > 0)
        })
    }

    pub fn view_report(&self, view: ViewId) -> Option<ParticleViewReport> {
        self.views.lock().ok()?.get(&view).copied()
    }

    pub fn forget_view(&self, view: ViewId) {
        if let Ok(mut views) = self.views.lock() {
            views.remove(&view);
        }
    }

    pub fn report(&self) -> ParticleReport {
        let Ok(state) = self.lock() else {
            return ParticleReport::default();
        };
        ParticleReport {
            simulated_ticks: state.simulated_ticks,
            repeated_ticks: state.repeated_ticks,
            last_tick: state.last_tick,
            emitters: state
                .emitters
                .iter()
                .map(|(id, e)| ParticleEmitterReport {
                    id: id.clone(),
                    capacity: e.emitter.capacity,
                    ticks: e.ticks,
                    spawn_attempts: e.cursor.attempts(),
                    rate_limited: e.cursor.rate_limited(),
                    high_water: e.cursor.high_water(e.emitter.capacity),
                })
                .collect(),
        }
    }

    fn create_emitter(&self, emitter: &ParticleEmitter) -> EmitterGpu {
        let device = &self.gpu.device;
        let buffer = |label: &str, size: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let params = buffer(
            "engine particles emitter",
            EMITTER_UNIFORM_BYTES,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let particles = buffer(
            "engine particles pool",
            PARTICLE_BYTES * u64::from(emitter.capacity),
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let counters = buffer(
            "engine particles counters",
            COUNTERS_BYTES,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        );
        let sim_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine particles simulation"),
            layout: &self.sim_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: particles.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: counters.as_entire_binding(),
                },
            ],
        });
        let draw_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine particles draw"),
            layout: &self.draw_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: particles.as_entire_binding(),
                },
            ],
        });
        EmitterGpu {
            emitter: emitter.clone(),
            cursor: EmitterCursor::default(),
            ticks: 0,
            params,
            particles,
            counters,
            sim_bind,
            draw_bind,
        }
    }

    /// Advance the simulation by one scene tick; see
    /// [`crate::ScenePipeline::advance_particles`].
    pub fn advance(
        &self,
        tick: &ParticleTick,
        emitters: &[ParticleEmitter],
    ) -> Result<ParticleTickReport> {
        // Everything is checked before any state or GPU change.
        validate_tick(tick, emitters)?;
        let mut state = self.lock()?;
        if state.last_tick == Some(tick.id) {
            state.repeated_ticks += 1;
            return Ok(ParticleTickReport {
                tick: tick.id,
                advanced: false,
                emitters: state.emitters.len(),
                ..Default::default()
            });
        }
        // Emitters that left the set free their buffers; a new id or a
        // changed capacity starts an empty pool.
        state
            .emitters
            .retain(|id, _| emitters.iter().any(|e| &e.id == id));
        for emitter in emitters {
            let stale = state
                .emitters
                .get(&emitter.id)
                .is_none_or(|existing| existing.emitter.capacity != emitter.capacity);
            if stale {
                let created = self.create_emitter(emitter);
                state.emitters.insert(emitter.id.clone(), created);
            }
        }
        let mut report = ParticleTickReport {
            tick: tick.id,
            advanced: true,
            emitters: emitters.len(),
            ..Default::default()
        };
        if !emitters.is_empty() {
            let mut encoder =
                self.gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("engine particles tick"),
                    });
            for emitter in emitters {
                let live = state
                    .emitters
                    .get_mut(&emitter.id)
                    .context("particle emitter pool vanished")?;
                let plan = live.cursor.plan(emitter, tick.dt);
                report.queued_spawns += plan.count;
                report.rate_limited += plan.excess;
                let uniform = emitter_uniform(emitter, &plan, tick.dt);
                self.gpu
                    .queue
                    .write_buffer(&live.params, 0, bytemuck::bytes_of(&uniform));
                // `alive` is recounted every tick; the other counters accumulate.
                encoder.clear_buffer(&live.counters, 0, Some(4));
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("engine particles"),
                        timestamp_writes: None,
                    });
                    pass.set_bind_group(0, &live.sim_bind, &[]);
                    pass.set_pipeline(&self.integrate);
                    pass.dispatch_workgroups(emitter.capacity.div_ceil(WORKGROUP_SIZE), 1, 1);
                    if plan.count > 0 {
                        pass.set_pipeline(&self.emit);
                        pass.dispatch_workgroups(plan.count.div_ceil(WORKGROUP_SIZE), 1, 1);
                    }
                }
                live.emitter = emitter.clone();
                live.ticks += 1;
            }
            self.gpu.queue.submit([encoder.finish()]);
        }
        state.last_tick = Some(tick.id);
        state.simulated_ticks += 1;
        Ok(report)
    }

    /// Wait for the GPU and read one emitter's pool and counters.
    pub fn read_back(&self, id: &str) -> Result<Option<ParticleReadback>> {
        let state = self.lock()?;
        let Some(live) = state.emitters.get(id) else {
            return Ok(None);
        };
        let pool_bytes = PARTICLE_BYTES * u64::from(live.emitter.capacity);
        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine particles readback"),
            size: pool_bytes + COUNTERS_BYTES,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("engine particles readback"),
            });
        encoder.copy_buffer_to_buffer(&live.particles, 0, &staging, 0, pool_bytes);
        encoder.copy_buffer_to_buffer(&live.counters, 0, &staging, pool_bytes, COUNTERS_BYTES);
        self.gpu.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.gpu.wait_idle()?;
        receiver
            .recv()
            .context("the particle readback was not mapped")?
            .context("mapping the particle readback")?;
        let mapped = slice
            .get_mapped_range()
            .map_err(|error| anyhow!("reading the particle readback: {error:?}"))?;
        let split = pool_bytes as usize;
        let particles = bytemuck::pod_collect_to_vec::<u8, GpuParticle>(&mapped[..split]);
        let counters = bytemuck::pod_collect_to_vec::<u8, ParticleCounters>(&mapped[split..]);
        drop(mapped);
        staging.unmap();
        Ok(Some(ParticleReadback {
            particles,
            counters: counters.first().copied().unwrap_or_default(),
            spawn_attempts: live.cursor.attempts(),
            rate_limited: live.cursor.rate_limited(),
        }))
    }
}

struct ParticleFeature {
    shared: Arc<ParticleShared>,
}

impl PipelineFeature for ParticleFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        descriptor()
    }

    fn forget_view(&mut self, view: ViewId) {
        self.shared.forget_view(view);
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let shared = self.shared.clone();
        let state = shared.lock()?;
        let draws: Vec<(&EmitterGpu, u32)> = state
            .emitters
            .values()
            .filter_map(|live| {
                let instances = live.cursor.high_water(live.emitter.capacity);
                (instances > 0).then_some((live, instances))
            })
            .collect();
        let report = ParticleViewReport {
            emitters_drawn: draws.len(),
            instances: draws.iter().map(|(_, n)| *n).sum(),
        };
        if !draws.is_empty() {
            let mut pass = ctx.scene_pass("engine particles")?;
            pass.set_bind_group(0, ctx.view_bind_group, &[]);
            for (live, instances) in &draws {
                pass.set_pipeline(match live.emitter.blend {
                    ParticleBlend::Alpha => &shared.alpha,
                    ParticleBlend::Additive => &shared.additive,
                });
                pass.set_bind_group(1, &live.draw_bind, &[]);
                pass.draw(0..6, 0..*instances);
            }
            drop(pass);
            ctx.record_draws(DrawStats {
                draw_calls: draws.len(),
                instances: report.instances as usize,
            });
        }
        if let Ok(mut views) = shared.views.lock() {
            views.insert(ctx.view.id, report);
        }
        Ok(())
    }
}

/// Replace the descriptor-only `engine.particles` of the built-in registry
/// with its implementation.
pub(crate) fn install(registry: &mut FeatureRegistry, shared: Arc<ParticleShared>) -> Result<()> {
    let registered = registry
        .features
        .iter_mut()
        .find(|r| r.descriptor.name == names::PARTICLES)
        .context("built-in feature engine.particles is not registered")?;
    registered.body = FeatureBody::Owned(Box::new(ParticleFeature { shared }));
    registered.prepared = true;
    Ok(())
}
