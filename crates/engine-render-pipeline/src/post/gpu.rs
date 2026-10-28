//! The GPU side: `engine.post` (PostProcess stage) and `engine.tonemap`
//! (Tonemap stage).

use super::lut::{Lut3d, f32_to_f16_bits};
use super::settings::{ExposureMode, MAX_BLOOM_MIPS, PostSettings, white_balance};
use crate::context::{FeatureContext, FeatureSetup};
use crate::feature::{FeatureBody, FeatureDescriptor, FeatureRegistry, PipelineFeature, Stage};
use crate::pipeline::names;
use crate::shader::{
    POST_BLOOM_WGSL, POST_EXPOSURE_WGSL, POST_MAIN_WGSL, POST_OUTPUT_WGSL, ShaderComposer,
};
use crate::targets::HDR_FORMAT;
use crate::view::ViewId;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Byte stride of one bloom pass's parameters in the shared uniform buffer.
const BLOOM_SLOT: u64 = 256;
const STATE_BYTES: u64 = 16;

/// Auto-exposure readbacks shared with [`crate::ScenePipeline`].
#[derive(Default)]
pub(crate) struct PostShared {
    pub readbacks: Mutex<HashMap<ViewId, wgpu::Buffer>>,
}

fn uniform(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn texture_entry(
    binding: u32,
    dimension: wgpu::TextureViewDimension,
    filterable: bool,
    stages: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: stages,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: dimension,
            multisampled: false,
        },
        count: None,
    }
}

fn buffer_entry(
    binding: u32,
    ty: wgpu::BufferBindingType,
    stages: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: stages,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn module(
    device: &wgpu::Device,
    composer: &ShaderComposer,
    label: &str,
    source: &str,
) -> Result<wgpu::ShaderModule> {
    let source = composer.compose(source)?;
    Ok(device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    }))
}

fn pipeline_layout(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> wgpu::PipelineLayout {
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    })
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    module: &wgpu::ShaderModule,
    fragment: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout(device, layout)),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn mip_size(base: [u32; 2], level: u32) -> [u32; 2] {
    [(base[0] >> level).max(1), (base[1] >> level).max(1)]
}

/// The number of bloom mips that fit a half-resolution base of `base`.
fn mip_count(base: [u32; 2], requested: u32) -> u32 {
    let fit = 32 - base[0].max(base[1]).max(1).leading_zeros();
    requested.clamp(1, MAX_BLOOM_MIPS).min(fit)
}

struct BloomChain {
    texture: wgpu::Texture,
    views: Vec<wgpu::TextureView>,
    base: [u32; 2],
    mips: u32,
}

impl BloomChain {
    fn new(device: &wgpu::Device, output: [u32; 2], mips: u32) -> Self {
        let base = mip_size(output, 1);
        let mips = mip_count(base, mips);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("engine bloom chain"),
            size: wgpu::Extent3d {
                width: base[0],
                height: base[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let views = (0..mips)
            .map(|level| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("engine bloom mip"),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        Self {
            texture,
            views,
            base,
            mips,
        }
    }
}

struct ViewPost {
    output: [u32; 2],
    bloom: Option<BloomChain>,
    state: wgpu::Buffer,
    histogram: wgpu::Buffer,
    expo_params: wgpu::Buffer,
    bloom_params: wgpu::Buffer,
    main_params: wgpu::Buffer,
    lut: Option<(Arc<Lut3d>, wgpu::TextureView)>,
}

impl ViewPost {
    fn new(device: &wgpu::Device, shared: &PostShared, view: ViewId) -> Self {
        let state = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine auto exposure state"),
            size: STATE_BYTES,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let histogram = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine auto exposure histogram"),
            size: 256,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine auto exposure readback"),
            size: STATE_BYTES,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        shared
            .readbacks
            .lock()
            .expect("post readbacks lock")
            .insert(view, readback);
        Self {
            output: [0, 0],
            bloom: None,
            state,
            histogram,
            expo_params: uniform(device, "engine auto exposure params", 48),
            bloom_params: uniform(
                device,
                "engine bloom params",
                BLOOM_SLOT * u64::from(MAX_BLOOM_MIPS) * 2,
            ),
            main_params: uniform(device, "engine post params", 16 * 9),
            lut: None,
        }
    }
}

/// `engine.post`: auto-exposure, bloom, exposure and HDR colour grading.
pub(crate) struct PostFeature {
    shared: Arc<PostShared>,
    views: HashMap<ViewId, ViewPost>,
    histogram_pipeline: Option<wgpu::ComputePipeline>,
    reduce_pipeline: Option<wgpu::ComputePipeline>,
    expo_layout: Option<wgpu::BindGroupLayout>,
    bloom_layout: Option<wgpu::BindGroupLayout>,
    prefilter: Option<wgpu::RenderPipeline>,
    down: Option<wgpu::RenderPipeline>,
    up: Option<wgpu::RenderPipeline>,
    main_layout: Option<wgpu::BindGroupLayout>,
    main: Option<wgpu::RenderPipeline>,
    sampler: Option<wgpu::Sampler>,
    black: Option<wgpu::TextureView>,
    identity_lut: Option<wgpu::TextureView>,
}

impl PostFeature {
    pub fn new(shared: Arc<PostShared>) -> Self {
        Self {
            shared,
            views: HashMap::new(),
            histogram_pipeline: None,
            reduce_pipeline: None,
            expo_layout: None,
            bloom_layout: None,
            prefilter: None,
            down: None,
            up: None,
            main_layout: None,
            main: None,
            sampler: None,
            black: None,
            identity_lut: None,
        }
    }
}

fn dummy_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    dimension: wgpu::TextureDimension,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("engine post placeholder"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension,
        format: HDR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &[0u8; 8],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&Default::default())
}

fn upload_lut(device: &wgpu::Device, queue: &wgpu::Queue, lut: &Lut3d) -> wgpu::TextureView {
    let n = lut.size;
    let extent = wgpu::Extent3d {
        width: n,
        height: n,
        depth_or_array_layers: n,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("engine colour grading LUT"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: HDR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut bytes = Vec::with_capacity(lut.data.len() * 8);
    for rgb in &lut.data {
        for value in [rgb[0], rgb[1], rgb[2], 1.0] {
            bytes.extend_from_slice(&f32_to_f16_bits(value).to_le_bytes());
        }
    }
    queue.write_texture(
        texture.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(n * 8),
            rows_per_image: Some(n),
        },
        extent,
    );
    texture.create_view(&Default::default())
}

fn f32_bytes(values: &[[f32; 4]]) -> Vec<u8> {
    bytemuck::cast_slice(values).to_vec()
}

fn main_params(settings: &PostSettings) -> Vec<[f32; 4]> {
    let g = &settings.grading;
    let wb = white_balance(g.temperature, g.tint);
    let (lut_on, lut_size, dmin, dmax) = match (&g.lut, g.enabled) {
        (Some(lut), true) => (1.0, lut.size as f32, lut.domain_min, lut.domain_max),
        _ => (0.0, 2.0, [0.0; 3], [1.0; 3]),
    };
    let mode = match settings.exposure.mode {
        ExposureMode::Off => 0.0,
        ExposureMode::Manual => 1.0,
        ExposureMode::Auto => 2.0,
    };
    let b = &settings.bloom;
    let bloom_on = if b.enabled && b.intensity > 0.0 {
        1.0
    } else {
        0.0
    };
    let grading_on = if g.enabled { 1.0 } else { 0.0 };
    vec![
        [mode, settings.manual_scale(), bloom_on, b.intensity],
        [b.tint[0], b.tint[1], b.tint[2], grading_on],
        [wb[0], wb[1], wb[2], g.exposure_offset.exp2()],
        [
            g.contrast,
            g.saturation,
            g.hue_shift_degrees.to_radians(),
            lut_on,
        ],
        [g.lift[0], g.lift[1], g.lift[2], g.lut_intensity],
        [
            1.0 / g.gamma[0],
            1.0 / g.gamma[1],
            1.0 / g.gamma[2],
            lut_size,
        ],
        [g.gain[0], g.gain[1], g.gain[2], 0.0],
        [dmin[0], dmin[1], dmin[2], 0.0],
        [dmax[0], dmax[1], dmax[2], 0.0],
    ]
}

impl PipelineFeature for PostFeature {
    fn forget_view(&mut self, view: ViewId) {
        self.views.remove(&view);
        self.shared
            .readbacks
            .lock()
            .expect("post readbacks lock")
            .remove(&view);
    }
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::POST, Stage::PostProcess).disabled_by_default()
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let composer = setup.composer;
        let compute = wgpu::ShaderStages::COMPUTE;
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let sampled = wgpu::TextureViewDimension::D2;

        let expo_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine auto exposure"),
            entries: &[
                texture_entry(0, sampled, false, compute),
                buffer_entry(
                    1,
                    wgpu::BufferBindingType::Storage { read_only: false },
                    compute,
                ),
                buffer_entry(2, wgpu::BufferBindingType::Uniform, compute),
                buffer_entry(
                    3,
                    wgpu::BufferBindingType::Storage { read_only: false },
                    compute,
                ),
            ],
        });
        let expo_module = module(device, composer, "engine auto exposure", POST_EXPOSURE_WGSL)?;
        let compute_pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout(device, &expo_layout)),
                module: &expo_module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        self.histogram_pipeline = Some(compute_pipeline("cs_histogram"));
        self.reduce_pipeline = Some(compute_pipeline("cs_reduce"));
        self.expo_layout = Some(expo_layout);

        let bloom_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine bloom"),
            entries: &[
                texture_entry(0, sampled, true, fragment),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                buffer_entry(2, wgpu::BufferBindingType::Uniform, fragment),
                buffer_entry(
                    3,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    fragment,
                ),
            ],
        });
        let bloom_module = module(device, composer, "engine bloom", POST_BLOOM_WGSL)?;
        let additive = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Constant,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let bloom_pipeline = |entry: &str, blend| {
            fullscreen_pipeline(
                device,
                entry,
                &bloom_layout,
                &bloom_module,
                entry,
                HDR_FORMAT,
                blend,
            )
        };
        self.prefilter = Some(bloom_pipeline("fs_prefilter", None));
        self.down = Some(bloom_pipeline("fs_down", None));
        self.up = Some(bloom_pipeline("fs_up", Some(additive)));
        self.bloom_layout = Some(bloom_layout);

        let main_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine post"),
            entries: &[
                texture_entry(0, sampled, false, fragment),
                texture_entry(1, sampled, true, fragment),
                texture_entry(2, wgpu::TextureViewDimension::D3, true, fragment),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                buffer_entry(4, wgpu::BufferBindingType::Uniform, fragment),
                buffer_entry(
                    5,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    fragment,
                ),
            ],
        });
        let main_module = module(device, composer, "engine post", POST_MAIN_WGSL)?;
        self.main = Some(fullscreen_pipeline(
            device,
            "engine post",
            &main_layout,
            &main_module,
            "fs_main",
            HDR_FORMAT,
            None,
        ));
        self.main_layout = Some(main_layout);
        self.sampler = Some(device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("engine post sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        }));
        self.black = Some(dummy_texture(
            device,
            &setup.gpu.queue,
            wgpu::TextureDimension::D2,
        ));
        self.identity_lut = Some(dummy_texture(
            device,
            &setup.gpu.queue,
            wgpu::TextureDimension::D3,
        ));
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let Some(settings) = ctx.view.post.as_ref() else {
            return Ok(());
        };
        if !settings.needs_post_pass() {
            return Ok(());
        }
        let settings = settings.clone();
        settings.validate()?;
        let device = ctx.gpu.device.clone();
        let queue = ctx.gpu.queue.clone();
        let view_id = ctx.view.id;
        let output = ctx.view.output_size;
        let (source, destination) = ctx.post_process_io()?;
        let shared = self.shared.clone();
        let state = self
            .views
            .entry(view_id)
            .or_insert_with(|| ViewPost::new(&device, &shared, view_id));
        if state.output != output {
            state.output = output;
            state.bloom = None;
        }
        let want_bloom = settings.bloom.enabled && settings.bloom.intensity > 0.0;
        if want_bloom {
            let rebuild = state
                .bloom
                .as_ref()
                .is_none_or(|chain| chain.mips != mip_count(chain.base, settings.bloom.mips));
            if rebuild {
                state.bloom = Some(BloomChain::new(&device, output, settings.bloom.mips));
            }
        }
        // Wanted mips can shrink below an existing chain; use what exists.
        let sampler = self.sampler.as_ref().context("post sampler")?;
        let auto = settings.exposure.mode == ExposureMode::Auto;

        if auto {
            let e = &settings.exposure;
            let params = [
                [output[0] as f32, output[1] as f32, e.min_ev, e.max_ev],
                [
                    e.low_percentile,
                    e.high_percentile,
                    e.compensation,
                    if ctx.view.reset_history { 1.0 } else { 0.0 },
                ],
                [e.speed_up, e.speed_down, ctx.view.delta_time.max(0.0), 0.0],
            ];
            queue.write_buffer(&state.expo_params, 0, &f32_bytes(&params));
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("engine auto exposure"),
                layout: self.expo_layout.as_ref().context("exposure layout")?,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: state.histogram.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: state.expo_params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: state.state.as_entire_binding(),
                    },
                ],
            });
            let mut pass = ctx
                .encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("engine auto exposure"),
                    timestamp_writes: None,
                });
            pass.set_bind_group(0, &bind, &[]);
            pass.set_pipeline(self.histogram_pipeline.as_ref().context("histogram")?);
            pass.dispatch_workgroups(output[0].div_ceil(16), output[1].div_ceil(16), 1);
            pass.set_pipeline(self.reduce_pipeline.as_ref().context("reduce")?);
            pass.dispatch_workgroups(1, 1, 1);
            drop(pass);
            if let Some(readback) = shared
                .readbacks
                .lock()
                .expect("post readbacks lock")
                .get(&view_id)
            {
                ctx.encoder
                    .copy_buffer_to_buffer(&state.state, 0, readback, 0, STATE_BYTES);
            }
        }

        let mode = match settings.exposure.mode {
            ExposureMode::Off => 0.0,
            ExposureMode::Manual => 1.0,
            ExposureMode::Auto => 2.0,
        };
        let bloom_view = if want_bloom {
            let chain = state.bloom.as_ref().context("bloom chain")?;
            let b = &settings.bloom;
            let mips = chain.mips;
            let mut slots = vec![0u8; (BLOOM_SLOT * u64::from(mips) * 2) as usize];
            let mut put = |slot: u64, texel: [f32; 2], extra: [f32; 2], dest: [u32; 2]| {
                let data = [
                    [texel[0], texel[1], extra[0], extra[1]],
                    [
                        mode,
                        settings.manual_scale(),
                        dest[0] as f32,
                        dest[1] as f32,
                    ],
                ];
                let at = (slot * BLOOM_SLOT) as usize;
                slots[at..at + 32].copy_from_slice(&f32_bytes(&data));
            };
            let knee = (b.threshold * b.knee).max(1e-5);
            put(
                0,
                [1.0 / output[0] as f32, 1.0 / output[1] as f32],
                [b.threshold, knee],
                chain.base,
            );
            for level in 1..mips {
                let src = mip_size(chain.base, level - 1);
                put(
                    u64::from(level),
                    [1.0 / src[0] as f32, 1.0 / src[1] as f32],
                    [0.0, 0.0],
                    mip_size(chain.base, level),
                );
            }
            for (index, level) in (0..mips.saturating_sub(1)).rev().enumerate() {
                let src = mip_size(chain.base, level + 1);
                put(
                    u64::from(mips) + index as u64,
                    [1.0 / src[0] as f32, 1.0 / src[1] as f32],
                    [0.0, 0.0],
                    mip_size(chain.base, level),
                );
            }
            queue.write_buffer(&state.bloom_params, 0, &slots);
            let layout = self.bloom_layout.as_ref().context("bloom layout")?;
            let bind = |src: &wgpu::TextureView, slot: u64| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("engine bloom"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(src),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &state.bloom_params,
                                offset: slot * BLOOM_SLOT,
                                size: wgpu::BufferSize::new(32),
                            }),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: state.state.as_entire_binding(),
                        },
                    ],
                })
            };
            let run = |encoder: &mut wgpu::CommandEncoder,
                       pipeline: &wgpu::RenderPipeline,
                       src: &wgpu::TextureView,
                       slot: u64,
                       target: &wgpu::TextureView,
                       load: wgpu::LoadOp<wgpu::Color>,
                       constant: Option<f64>| {
                let group = bind(src, slot);
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("engine bloom"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                if let Some(c) = constant {
                    pass.set_blend_constant(wgpu::Color {
                        r: c,
                        g: c,
                        b: c,
                        a: c,
                    });
                }
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.draw(0..3, 0..1);
            };
            let clear = wgpu::LoadOp::Clear(wgpu::Color::BLACK);
            run(
                ctx.encoder,
                self.prefilter.as_ref().context("prefilter")?,
                &source,
                0,
                &chain.views[0],
                clear,
                None,
            );
            for level in 1..mips {
                run(
                    ctx.encoder,
                    self.down.as_ref().context("down")?,
                    &chain.views[level as usize - 1],
                    u64::from(level),
                    &chain.views[level as usize],
                    clear,
                    None,
                );
            }
            for (index, level) in (0..mips.saturating_sub(1)).rev().enumerate() {
                run(
                    ctx.encoder,
                    self.up.as_ref().context("up")?,
                    &chain.views[level as usize + 1],
                    u64::from(mips) + index as u64,
                    &chain.views[level as usize],
                    wgpu::LoadOp::Load,
                    Some(f64::from(b.scatter)),
                );
            }
            let _ = &chain.texture;
            chain.views[0].clone()
        } else {
            self.black.as_ref().context("placeholder")?.clone()
        };

        let lut_view = match (&settings.grading.lut, settings.grading.enabled) {
            (Some(lut), true) => {
                let current = state
                    .lut
                    .as_ref()
                    .is_some_and(|(held, _)| Arc::ptr_eq(held, lut) || **held == **lut);
                if !current {
                    state.lut = Some((lut.clone(), upload_lut(&device, &queue, lut)));
                }
                state.lut.as_ref().map(|(_, view)| view.clone())
            }
            _ => None,
        }
        .unwrap_or_else(|| self.identity_lut.clone().expect("placeholder"));

        queue.write_buffer(&state.main_params, 0, &f32_bytes(&main_params(&settings)));
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine post"),
            layout: self.main_layout.as_ref().context("post layout")?,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&bloom_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&lut_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: state.main_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: state.state.as_entire_binding(),
                },
            ],
        });
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("engine post"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &destination,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(self.main.as_ref().context("post pipeline")?);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

/// `engine.tonemap`: tonemap, vignette, grain and the sRGB encode.
pub(crate) struct TonemapFeature {
    layout: Option<wgpu::BindGroupLayout>,
    module: Option<wgpu::ShaderModule>,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    views: HashMap<ViewId, (wgpu::Buffer, u32)>,
    pending: HashSet<ViewId>,
}

impl TonemapFeature {
    pub fn new() -> Self {
        Self {
            layout: None,
            module: None,
            pipelines: HashMap::new(),
            views: HashMap::new(),
            pending: HashSet::new(),
        }
    }
}

impl PipelineFeature for TonemapFeature {
    fn forget_view(&mut self, view: ViewId) {
        self.views.remove(&view);
        self.pending.remove(&view);
    }
    fn reset_history(&mut self, view: ViewId) {
        self.pending.remove(&view);
    }
    fn finish_scene(&mut self, view: ViewId, _evaluation_id: u64) {
        if self.pending.remove(&view)
            && let Some((_, frame)) = self.views.get_mut(&view)
        {
            *frame = frame.wrapping_add(1) % 65_536;
        }
    }
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::TONEMAP, Stage::Tonemap)
            .after(names::OUTPUT)
            .disabled_by_default()
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        self.layout = Some(
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("engine tonemap"),
                entries: &[
                    texture_entry(
                        0,
                        wgpu::TextureViewDimension::D2,
                        false,
                        wgpu::ShaderStages::FRAGMENT,
                    ),
                    buffer_entry(
                        1,
                        wgpu::BufferBindingType::Uniform,
                        wgpu::ShaderStages::FRAGMENT,
                    ),
                ],
            }),
        );
        self.module = Some(module(
            device,
            setup.composer,
            "engine tonemap",
            POST_OUTPUT_WGSL,
        )?);
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let settings = ctx.view.post.clone().unwrap_or_default();
        settings.validate()?;
        let device = ctx.gpu.device.clone();
        let hdr = ctx.hdr_input()?;
        let output = ctx.output()?.clone();
        let layout = self.layout.as_ref().context("tonemap layout")?;
        let module = self.module.as_ref().context("tonemap shader")?;
        let pipeline = match self.pipelines.get(&output.format) {
            Some(pipeline) => pipeline.clone(),
            None => {
                let pipeline = crate::scoped(&device, || {
                    Ok(fullscreen_pipeline(
                        &device,
                        "engine tonemap",
                        layout,
                        module,
                        "fs_main",
                        output.format,
                        None,
                    ))
                })?;
                self.pipelines.insert(output.format, pipeline.clone());
                pipeline
            }
        };
        let entry = self
            .views
            .entry(ctx.view.id)
            .or_insert_with(|| (uniform(&device, "engine tonemap params", 16 * 5), 0));
        let frame = entry.1.wrapping_add(1) % 65_536;
        self.pending.insert(ctx.view.id);
        let v = &settings.vignette;
        let n = &settings.grain;
        let flag = |on: bool| if on { 1.0 } else { 0.0 };
        let params = [
            [
                output.viewport.x as f32,
                output.viewport.y as f32,
                output.viewport.width as f32,
                output.viewport.height as f32,
            ],
            [
                settings.tonemap.code(),
                flag(!output.format.is_srgb()),
                flag(v.enabled && v.intensity > 0.0),
                flag(n.enabled && n.intensity > 0.0),
            ],
            [v.intensity, v.smoothness, v.center[0], v.center[1]],
            [v.color[0], v.color[1], v.color[2], frame as f32],
            [n.intensity, n.response, 0.0, 0.0],
        ];
        ctx.gpu.queue.write_buffer(&entry.0, 0, &f32_bytes(&params));
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine tonemap"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&hdr),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: entry.0.as_entire_binding(),
                },
            ],
        });
        let mut pass = ctx.output_pass("engine tonemap")?;
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

/// Replace the descriptor-only `engine.post` / `engine.tonemap` of the
/// built-in registry with their implementations and prepare them.
pub(crate) fn install(
    registry: &mut FeatureRegistry,
    setup: &FeatureSetup<'_>,
    shared: Arc<PostShared>,
) -> Result<()> {
    let features: [(&str, Box<dyn PipelineFeature>); 2] = [
        (names::POST, Box::new(PostFeature::new(shared))),
        (names::TONEMAP, Box::new(TonemapFeature::new())),
    ];
    for (name, mut feature) in features {
        crate::scoped(&setup.gpu.device, || feature.prepare(setup))
            .with_context(|| format!("preparing render feature {name}"))?;
        let registered = registry
            .features
            .iter_mut()
            .find(|r| r.descriptor.name == name)
            .with_context(|| format!("built-in feature {name} is not registered"))?;
        registered.body = FeatureBody::Owned(feature);
        registered.prepared = true;
    }
    Ok(())
}
