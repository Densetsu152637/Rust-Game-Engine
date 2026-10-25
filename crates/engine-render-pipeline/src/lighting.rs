//! Per-view lighting: CPU culling of the light list, the uploaded GPU light
//! list, shadow view allocation in the shadow atlas, the light cluster grid
//! (built on the GPU by `engine.lights.clusters`), the `engine::view` bind
//! groups and the per-view [`LightingReport`].

use crate::context::PipelineLayouts;
use crate::lights::{
    GpuLight, GpuShadow, Light, LightKind, LightList, LightShape, MAX_LIGHTS, MAX_SHADOWS,
    ShadowSettings, ShadowView,
};
use crate::shader::{CLUSTERS_WGSL, ShaderComposer};
use crate::shadows::{
    ShadowAtlasConfig, cascade_splits, cascade_view, cube_views, pack_atlas,
    projection_depth_range, spot_view,
};
use crate::targets::DEPTH_FORMAT;
use crate::view::{VIEW_UNIFORM_BYTES, ViewData};
use anyhow::{Context, Result};
use bytemuck::Zeroable;
use engine_render_wgpu::GpuContext;
use glam::{Mat4, Vec3, Vec4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Froxel grid: clusters across, down and in (logarithmic) depth.
pub const CLUSTER_DIMS: [u32; 3] = [16, 9, 24];
/// Longest light list of one cluster; longer lists are truncated (counted
/// in [`ClusterStats::overflowed_clusters`]).
pub const MAX_LIGHTS_PER_CLUSTER: u32 = 256;
/// Most overflow reasons one report keeps.
pub const MAX_LIGHTING_REASONS: usize = 4;

const CLUSTER_COUNT: u32 = CLUSTER_DIMS[0] * CLUSTER_DIMS[1] * CLUSTER_DIMS[2];
const CLUSTER_HEADER_BYTES: u64 = 64;
const READBACK_BYTES: u64 = 32;

/// How local lights reach the lighting shader in a view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClusterMode {
    /// Per-fragment lists from the cluster grid.
    #[default]
    Clustered,
    /// Every uploaded local light per fragment (`engine.lights.clusters`
    /// disabled in the view).
    AllLights,
}

impl ClusterMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Clustered => "clustered",
            Self::AllLights => "all_lights",
        }
    }
}

/// GPU-side cluster statistics of a completed cluster pass (read back
/// without stalling, so they describe an earlier frame of the view).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClusterStats {
    /// Clusters whose light list exceeded [`MAX_LIGHTS_PER_CLUSTER`].
    pub overflowed_clusters: u32,
    /// Longest list before truncation.
    pub longest_list: u32,
    /// Light references stored in all lists.
    pub stored_entries: u32,
    /// Clusters with at least one light.
    pub nonempty_clusters: u32,
    /// GPU time of the cluster pass, when the device has timestamp queries.
    pub gpu_nanoseconds: Option<u64>,
}

/// What lighting did in one view's frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LightingReport {
    /// Lights in the frame's [`LightList`].
    pub submitted: usize,
    /// Local lights outside the view frustum or with zero intensity.
    pub culled: usize,
    /// Visible lights dropped by the [`MAX_LIGHTS`] budget (farthest first).
    pub over_budget: usize,
    /// Uploaded directional lights.
    pub directional: usize,
    /// Uploaded local lights (point, spot, area).
    pub local: usize,
    pub cluster_mode: ClusterMode,
    /// Local lights the cluster pass assigned to clusters (0 without it).
    pub clustered: usize,
    pub cluster_dims: [u32; 3],
    /// Latest completed GPU readback of the cluster pass.
    pub cluster_stats: Option<ClusterStats>,
    /// Whether `engine.shadows` runs in this view.
    pub shadows_enabled: bool,
    /// Lights with shadow views this frame.
    pub shadowed_lights: usize,
    pub shadow_views: usize,
    /// Lights that requested a shadow but did not get one (atlas or view
    /// budget; see `overflow`).
    pub shadow_dropped: usize,
    pub atlas_page_size: u32,
    /// Atlas pages holding this frame's shadow views.
    pub atlas_pages: u32,
    pub atlas_texels_used: u64,
    pub atlas_texels_capacity: u64,
    /// Why lights or shadows were dropped (at most [`MAX_LIGHTING_REASONS`]).
    pub overflow: Vec<String>,
}

impl LightingReport {
    fn reason(&mut self, reason: String) {
        if self.overflow.len() < MAX_LIGHTING_REASONS {
            self.overflow.push(reason);
        }
    }
}

/// One shadow view to render: atlas layer, texel rectangle and the
/// `EngineShadow` index the caster pass reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShadowDraw {
    pub layer: u32,
    pub x: u32,
    pub y: u32,
    pub size: u32,
    pub index: u32,
}

/// The CPU result of planning a view's lights.
#[derive(Clone, Debug)]
pub(crate) struct LightPlan {
    pub lights: Vec<GpuLight>,
    pub directional: u32,
    pub shadows: Vec<GpuShadow>,
    pub draws: Vec<ShadowDraw>,
    pub pages: u32,
    pub header: [u8; CLUSTER_HEADER_BYTES as usize],
    pub report: LightingReport,
}

/// Inputs of [`plan_lights`].
pub(crate) struct LightingFrame<'a> {
    pub view: &'a ViewData,
    pub lights: &'a LightList,
    pub shadows: bool,
    pub clusters: bool,
    pub atlas: ShadowAtlasConfig,
}

/// Normalised frustum planes (`xyz . p + w >= 0` inside) of a WebGPU
/// clip-from-world matrix.
pub fn frustum_planes(clip_from_world: Mat4) -> [Vec4; 6] {
    let (r0, r1, r2, r3) = (
        clip_from_world.row(0),
        clip_from_world.row(1),
        clip_from_world.row(2),
        clip_from_world.row(3),
    );
    [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r2, r3 - r2]
        .map(|plane| plane / plane.truncate().length().max(1e-20))
}

/// Whether a sphere intersects the frustum (conservative).
pub fn sphere_in_frustum(planes: &[Vec4; 6], center: Vec3, radius: f32) -> bool {
    planes
        .iter()
        .all(|plane| plane.truncate().dot(center) + plane.w >= -radius)
}

fn shadow_view_count(light: &Light, settings: &ShadowSettings) -> u32 {
    match light.kind() {
        LightKind::Directional => settings.cascades.count,
        LightKind::Spot => 1,
        LightKind::Point | LightKind::Area => 6,
    }
}

fn shadow_views(view: &ViewData, light: &Light, settings: &ShadowSettings) -> Vec<ShadowView> {
    let resolution = settings.resolution;
    let margin = settings.kernel_texels();
    match light.shape {
        LightShape::Directional => {
            let (near, far) = projection_depth_range(view.clip_from_view);
            let c = &settings.cascades;
            let splits = cascade_splits(near, c.max_distance.min(far), c.count, c.split_lambda);
            splits
                .windows(2)
                .map(|split| {
                    cascade_view(
                        view.world_from_view,
                        view.clip_from_view,
                        [split[0], split[1]],
                        light.direction,
                        resolution,
                        c.caster_extension,
                    )
                })
                .collect()
        }
        LightShape::Spot { outer_angle, .. } => vec![spot_view(
            light.position,
            light.direction,
            outer_angle,
            settings.near,
            light.range,
            resolution,
            margin,
        )],
        LightShape::Point | LightShape::Area { .. } => cube_views(
            light.position,
            settings.near,
            light.range,
            resolution,
            margin,
        )
        .to_vec(),
    }
}

/// Cull, budget, order and shadow-plan one view's lights (no GPU work).
pub(crate) fn plan_lights(frame: &LightingFrame<'_>) -> Result<LightPlan> {
    let view = frame.view;
    let lights = &frame.lights.lights;
    frame.atlas.validate()?;
    let mut report = LightingReport {
        submitted: lights.len(),
        cluster_dims: CLUSTER_DIMS,
        shadows_enabled: frame.shadows,
        atlas_page_size: frame.atlas.page_size,
        atlas_texels_capacity: frame.atlas.capacity(),
        cluster_mode: if frame.clusters {
            ClusterMode::Clustered
        } else {
            ClusterMode::AllLights
        },
        ..Default::default()
    };
    let planes = frustum_planes(view.clip_from_world());
    let camera = view.camera_position();
    let mut directional = Vec::new();
    let mut local: Vec<(usize, f32)> = Vec::new();
    for (index, light) in lights.iter().enumerate() {
        if light.kind() == LightKind::Directional {
            directional.push(index);
        } else if light.intensity > 0.0
            && light.color.max_element() > 0.0
            && sphere_in_frustum(&planes, light.position, light.range)
        {
            let priority = (light.position.distance(camera) - light.range).max(0.0);
            local.push((index, priority));
        } else {
            report.culled += 1;
        }
    }
    if directional.len() > MAX_LIGHTS {
        report.over_budget += directional.len() - MAX_LIGHTS;
        report.reason(format!(
            "{} directional lights over the {MAX_LIGHTS}-light view budget were dropped",
            directional.len() - MAX_LIGHTS
        ));
        directional.truncate(MAX_LIGHTS);
    }
    let budget = MAX_LIGHTS - directional.len();
    if local.len() > budget {
        local.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        let dropped = local.len() - budget;
        report.over_budget += dropped;
        report.reason(format!(
            "{dropped} visible local lights over the {MAX_LIGHTS}-light view budget were \
             dropped (farthest first)"
        ));
        local.truncate(budget);
    }
    let priorities: std::collections::HashMap<usize, f32> = local.iter().copied().collect();
    local.sort_by_key(|entry| entry.0);
    let order: Vec<usize> = directional
        .iter()
        .copied()
        .chain(local.iter().map(|entry| entry.0))
        .collect();
    let mut gpu: Vec<GpuLight> = order.iter().map(|&i| lights[i].gpu()).collect();
    report.directional = directional.len();
    report.local = local.len();
    report.clustered = if frame.clusters { local.len() } else { 0 };

    // Shadows: directional lights first, then local lights nearest first.
    let mut shadows = Vec::new();
    let mut draws = Vec::new();
    let mut pages = 0;
    if frame.shadows {
        let mut requests: Vec<usize> = (0..order.len())
            .filter(|&slot| lights[order[slot]].shadow.is_some())
            .collect();
        requests.sort_by(|&a, &b| {
            let priority = |slot: usize| {
                if slot < directional.len() {
                    -1.0
                } else {
                    priorities[&order[slot]]
                }
            };
            priority(a).total_cmp(&priority(b)).then(a.cmp(&b))
        });
        let capacity = frame.atlas.capacity();
        let mut used = 0u64;
        let mut admitted: Vec<(usize, Vec<ShadowView>)> = Vec::new();
        let mut view_count = 0;
        let (mut atlas_full, mut view_limit) = (0, 0);
        for slot in requests {
            let light = &lights[order[slot]];
            let settings = light.shadow.expect("filtered above");
            let count = shadow_view_count(light, &settings) as usize;
            let size = settings.resolution.min(frame.atlas.page_size);
            let area = u64::from(size).pow(2) * count as u64;
            if view_count + count > MAX_SHADOWS {
                view_limit += 1;
                continue;
            }
            if used + area > capacity {
                atlas_full += 1;
                continue;
            }
            let mut views = shadow_views(view, light, &settings);
            for view in &mut views {
                view.resolution = size;
            }
            used += area;
            view_count += count;
            admitted.push((slot, views));
        }
        if atlas_full > 0 {
            report.reason(format!(
                "{atlas_full} shadowed lights did not fit the shadow atlas ({} pages of {}^2 \
                 texels); lower their resolution or raise ShadowAtlasConfig",
                frame.atlas.max_pages, frame.atlas.page_size
            ));
        }
        if view_limit > 0 {
            report.reason(format!(
                "{view_limit} shadowed lights exceeded the {MAX_SHADOWS} shadow view limit"
            ));
        }
        report.shadow_dropped = atlas_full + view_limit;
        let sizes: Vec<u32> = admitted
            .iter()
            .flat_map(|(_, views)| views.iter().map(|v| v.resolution))
            .collect();
        let slots = pack_atlas(&sizes, &frame.atlas)
            .context("shadow views admitted by area always pack")?;
        let page = frame.atlas.page_size as f32;
        let mut next = 0usize;
        for (slot, views) in &admitted {
            let light = &lights[order[*slot]];
            let settings = light.shadow.expect("admitted lights have shadows");
            gpu[*slot].shadow_index = shadows.len() as i32;
            for view in views {
                let atlas_slot = slots[next];
                next += 1;
                pages = pages.max(atlas_slot.page + 1);
                draws.push(ShadowDraw {
                    layer: atlas_slot.page,
                    x: atlas_slot.x,
                    y: atlas_slot.y,
                    size: atlas_slot.size,
                    index: shadows.len() as u32,
                });
                shadows.push(view.gpu(
                    &settings,
                    [
                        atlas_slot.x as f32 / page,
                        atlas_slot.y as f32 / page,
                        atlas_slot.size as f32 / page,
                        atlas_slot.page as f32,
                    ],
                ));
            }
        }
        report.shadowed_lights = admitted.len();
        report.shadow_views = shadows.len();
        report.atlas_pages = pages;
        report.atlas_texels_used = used;
    }

    let header = cluster_header(view, &gpu[directional.len()..], &report, gpu.len());
    Ok(LightPlan {
        lights: gpu,
        directional: directional.len() as u32,
        shadows,
        draws,
        pages,
        header,
        report,
    })
}

/// `EngineClusterGrid` header: dims, logarithmic depth slicing between the
/// camera near plane and the farthest local light extent, tile scale and
/// mode.
fn cluster_header(
    view: &ViewData,
    local: &[GpuLight],
    report: &LightingReport,
    light_count: usize,
) -> [u8; CLUSTER_HEADER_BYTES as usize] {
    let (near, far) = projection_depth_range(view.clip_from_view);
    let near = near.max(1e-3);
    let view_from_world = view.view_from_world();
    let extent = local
        .iter()
        .map(|light| {
            -view_from_world
                .transform_point3(Vec3::from(light.position))
                .z
                + light.range
        })
        .fold(f32::NEG_INFINITY, f32::max);
    let cluster_far = if extent.is_finite() {
        extent.clamp(near * 1.01, far.max(near * 1.01))
    } else {
        far.max(near * 1.01)
    };
    let slices = CLUSTER_DIMS[2] as f32;
    let log_ratio = (cluster_far / near).ln();
    let depth = [
        near,
        cluster_far,
        slices / log_ratio,
        -slices * near.ln() / log_ratio,
    ];
    let tile = [
        CLUSTER_DIMS[0] as f32 / view.render_size[0] as f32,
        CLUSTER_DIMS[1] as f32 / view.render_size[1] as f32,
        0.0,
        0.0,
    ];
    let mode = match report.cluster_mode {
        ClusterMode::Clustered => 0u32,
        ClusterMode::AllLights => 1,
    };
    let dims = [
        CLUSTER_DIMS[0],
        CLUSTER_DIMS[1],
        CLUSTER_DIMS[2],
        MAX_LIGHTS_PER_CLUSTER,
    ];
    let params = [mode, report.directional as u32, light_count as u32, 0];
    let mut header = [0u8; CLUSTER_HEADER_BYTES as usize];
    header[0..16].copy_from_slice(bytemuck::cast_slice(&dims));
    header[16..32].copy_from_slice(bytemuck::cast_slice(&depth));
    header[32..48].copy_from_slice(bytemuck::cast_slice(&tile));
    header[48..64].copy_from_slice(bytemuck::cast_slice(&params));
    header
}

// ----------------------------------------------------------------- GPU

/// Shared GPU objects of lighting: the cluster pass and the shadow
/// comparison sampler.
pub(crate) struct LightingRenderer {
    cluster_pipeline: wgpu::ComputePipeline,
    compute_view_layout: wgpu::BindGroupLayout,
    compute_lists_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    timestamps: bool,
    /// 1x1 screen-lighting placeholders holding
    /// [`crate::targets::SCREEN_LIGHTING_DEFAULTS`].
    screen_defaults: [wgpu::TextureView; 4],
}

impl LightingRenderer {
    pub fn new(gpu: &GpuContext) -> Result<Self> {
        let device = &gpu.device;
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let storage = |read_only| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let compute_view_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("engine cluster view"),
                entries: &[
                    entry(
                        0,
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(VIEW_UNIFORM_BYTES),
                        },
                    ),
                    entry(1, storage(true)),
                ],
            });
        let compute_lists_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("engine cluster lists"),
                entries: &[
                    entry(0, storage(false)),
                    entry(1, storage(false)),
                    entry(2, storage(false)),
                ],
            });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("engine cluster pass"),
            bind_group_layouts: &[Some(&compute_view_layout), Some(&compute_lists_layout)],
            immediate_size: 0,
        });
        let source = ShaderComposer::new().compose(CLUSTERS_WGSL)?;
        let cluster_pipeline = crate::scoped(device, || {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("engine clusters"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            Ok(
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("engine clusters"),
                    layout: Some(&layout),
                    module: &module,
                    entry_point: Some("build"),
                    compilation_options: Default::default(),
                    cache: None,
                }),
            )
        })
        .context("creating the light cluster pass")?;
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("engine shadow comparison"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let screen_defaults = crate::targets::SCREEN_LIGHTING_DEFAULTS.map(|value| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("engine screen lighting default"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: crate::targets::SCREEN_LIGHTING_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let half: Vec<u8> = value
                .iter()
                .flat_map(|v| f32_to_f16(*v).to_le_bytes())
                .collect();
            gpu.queue.write_texture(
                texture.as_image_copy(),
                &half,
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
        });
        Ok(Self {
            cluster_pipeline,
            compute_view_layout,
            compute_lists_layout,
            sampler,
            timestamps: device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
            screen_defaults,
        })
    }

    /// Build `state`'s cluster lists (the `engine.lights.clusters` node).
    pub fn encode_clusters(&self, encoder: &mut wgpu::CommandEncoder, state: &mut ViewLighting) {
        let readback = state.readback.state.load(Ordering::Acquire) == READBACK_IDLE;
        let timestamps = state.timestamps.as_ref().filter(|_| readback);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("engine.lights.clusters"),
                timestamp_writes: timestamps.map(|t| wgpu::ComputePassTimestampWrites {
                    query_set: &t.0,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                }),
            });
            pass.set_pipeline(&self.cluster_pipeline);
            pass.set_bind_group(0, &state.compute_view, &[]);
            pass.set_bind_group(1, &state.compute_lists, &[]);
            pass.dispatch_workgroups(CLUSTER_COUNT.div_ceil(64), 1, 1);
        }
        if readback {
            encoder.copy_buffer_to_buffer(&state.stats, 0, &state.readback.buffer, 0, 16);
            if let Some((queries, resolve)) = timestamps {
                encoder.resolve_query_set(queries, 0..2, resolve, 0);
                encoder.copy_buffer_to_buffer(resolve, 0, &state.readback.buffer, 16, 16);
            }
            state.readback.encoded = true;
            state.readback.timed = timestamps.is_some();
        }
    }
}

pub(crate) struct Atlas {
    pub page_size: u32,
    pub layers: u32,
    array_view: wgpu::TextureView,
    pub layer_views: Vec<wgpu::TextureView>,
}

impl Atlas {
    fn new(device: &wgpu::Device, page_size: u32, layers: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("engine shadow atlas"),
            size: wgpu::Extent3d {
                width: page_size,
                height: page_size,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let array_view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layer_views = (0..layers)
            .map(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        Self {
            page_size,
            layers,
            array_view,
            layer_views,
        }
    }
}

struct GrowBuffer {
    buffer: wgpu::Buffer,
    capacity: u64,
}

impl GrowBuffer {
    fn new(device: &wgpu::Device, label: &str, capacity: u64) -> Self {
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: capacity,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            capacity,
        }
    }

    /// Write `bytes`, growing first; `true` when the buffer was replaced.
    fn write(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        bytes: &[u8],
    ) -> bool {
        let grown = self.capacity < bytes.len() as u64;
        if grown {
            *self = Self::new(device, label, (bytes.len() as u64).next_power_of_two());
        }
        queue.write_buffer(&self.buffer, 0, bytes);
        grown
    }
}

const READBACK_IDLE: u8 = 0;
const READBACK_PENDING: u8 = 1;
const READBACK_READY: u8 = 2;
const READBACK_FAILED: u8 = 3;

struct Readback {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// A copy into `buffer` was encoded this frame.
    encoded: bool,
    timed: bool,
    pending_timed: bool,
}

/// Lighting state of one view.
pub(crate) struct ViewLighting {
    lights: GrowBuffer,
    shadows: GrowBuffer,
    grid: wgpu::Buffer,
    lists: wgpu::Buffer,
    stats: wgpu::Buffer,
    readback: Readback,
    timestamps: Option<(wgpu::QuerySet, wgpu::Buffer)>,
    atlas: Atlas,
    no_atlas: Atlas,
    bind_group: wgpu::BindGroup,
    shadow_group: wgpu::BindGroup,
    compute_view: wgpu::BindGroup,
    compute_lists: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    /// Views bound at `engine::view` 7..=10 and the texture they belong to
    /// (`None` for the placeholders).
    screen: [wgpu::TextureView; 4],
    screen_texture: Option<wgpu::Texture>,
    /// Shadow views of this frame.
    pub draws: Vec<ShadowDraw>,
    /// Whether the cluster pass runs this frame.
    pub dispatch: bool,
    stats_seen: Option<ClusterStats>,
    pub report: LightingReport,
}

impl ViewLighting {
    pub fn new(
        gpu: &GpuContext,
        layouts: &PipelineLayouts,
        renderer: &LightingRenderer,
        uniform: &wgpu::Buffer,
    ) -> Self {
        let device = &gpu.device;
        let lights = GrowBuffer::new(device, "engine lights", 256);
        let shadows = GrowBuffer::new(device, "engine shadows", 256);
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let grid = buffer(
            "engine cluster grid",
            CLUSTER_HEADER_BYTES + 4 * u64::from(CLUSTER_COUNT),
            storage,
        );
        let lists = buffer(
            "engine cluster lights",
            4 * u64::from(CLUSTER_COUNT * MAX_LIGHTS_PER_CLUSTER),
            storage,
        );
        let stats = buffer(
            "engine cluster stats",
            16,
            storage | wgpu::BufferUsages::COPY_SRC,
        );
        let readback = Readback {
            buffer: buffer(
                "engine cluster readback",
                READBACK_BYTES,
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            ),
            state: Arc::new(AtomicU8::new(READBACK_IDLE)),
            encoded: false,
            timed: false,
            pending_timed: false,
        };
        let timestamps = renderer.timestamps.then(|| {
            (
                device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("engine cluster timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                }),
                buffer(
                    "engine cluster timestamp resolve",
                    16,
                    wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                ),
            )
        });
        // Zeroed defaults: no lights, all-lights mode, empty clusters.
        let mut header = [0u8; CLUSTER_HEADER_BYTES as usize];
        header[0..16].copy_from_slice(bytemuck::cast_slice(&[
            CLUSTER_DIMS[0],
            CLUSTER_DIMS[1],
            CLUSTER_DIMS[2],
            MAX_LIGHTS_PER_CLUSTER,
        ]));
        header[16..32].copy_from_slice(bytemuck::cast_slice(&[0.1f32, 1.0, 1.0, 0.0]));
        header[48..52].copy_from_slice(&1u32.to_ne_bytes());
        gpu.queue.write_buffer(&grid, 0, &header);
        gpu.queue.write_buffer(&lights.buffer, 0, &[0; 16]);
        let atlas = Atlas::new(device, 64, 1);
        let no_atlas = Atlas::new(device, 1, 1);
        let parts = Parts {
            uniform,
            lights: &lights.buffer,
            shadows: &shadows.buffer,
            grid: &grid,
            lists: &lists,
            sampler: &renderer.sampler,
            screen: &renderer.screen_defaults,
        };
        let bind_group = view_group(device, layouts, &parts, &atlas);
        let shadow_group = view_group(device, layouts, &parts, &no_atlas);
        let (compute_view, compute_lists) = compute_groups(device, renderer, &parts, &stats);
        Self {
            lights,
            shadows,
            grid,
            lists,
            stats,
            readback,
            timestamps,
            atlas,
            no_atlas,
            bind_group,
            shadow_group,
            compute_view,
            compute_lists,
            uniform: uniform.clone(),
            screen: renderer.screen_defaults.clone(),
            screen_texture: None,
            draws: Vec::new(),
            dispatch: false,
            stats_seen: None,
            report: LightingReport::default(),
        }
    }

    /// `engine::view` group with the shadow atlas.
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Bind `screen` (or the placeholders) at `engine::view` 7..=10,
    /// rebuilding the view groups when the textures changed.
    pub fn set_screen(
        &mut self,
        device: &wgpu::Device,
        layouts: &PipelineLayouts,
        renderer: &LightingRenderer,
        screen: Option<&crate::targets::ScreenLighting>,
    ) {
        let texture = screen.map(|s| s.shadow.texture.clone());
        if texture == self.screen_texture {
            return;
        }
        self.screen = match screen {
            Some(s) => s.targets().map(|t| t.view.clone()),
            None => renderer.screen_defaults.clone(),
        };
        self.screen_texture = texture;
        let parts = Parts {
            uniform: &self.uniform,
            lights: &self.lights.buffer,
            shadows: &self.shadows.buffer,
            grid: &self.grid,
            lists: &self.lists,
            sampler: &renderer.sampler,
            screen: &self.screen,
        };
        self.bind_group = view_group(device, layouts, &parts, &self.atlas);
        self.shadow_group = view_group(device, layouts, &parts, &self.no_atlas);
    }

    /// `engine::view` group with a placeholder atlas (Shadow stage).
    pub fn shadow_group(&self) -> &wgpu::BindGroup {
        &self.shadow_group
    }

    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// Plan and upload this frame's lights; returns the uploaded light count.
    pub fn prepare(
        &mut self,
        gpu: &GpuContext,
        layouts: &PipelineLayouts,
        renderer: &LightingRenderer,
        uniform: &wgpu::Buffer,
        frame: &LightingFrame<'_>,
    ) -> Result<u32> {
        // A copy encoded by a frame that failed before submission never ran.
        self.readback.encoded = false;
        self.collect_readback(gpu);
        let plan = plan_lights(frame)?;
        let device = &gpu.device;
        let queue = &gpu.queue;
        let mut light_bytes = Vec::with_capacity(16 + 80 * plan.lights.len().max(1));
        light_bytes.extend_from_slice(bytemuck::cast_slice(&[
            plan.lights.len() as u32,
            plan.directional,
            0,
            0,
        ]));
        if plan.lights.is_empty() {
            light_bytes.extend_from_slice(bytemuck::bytes_of(&GpuLight::zeroed()));
        } else {
            light_bytes.extend_from_slice(bytemuck::cast_slice(&plan.lights));
        }
        let shadow_bytes: Vec<u8> = if plan.shadows.is_empty() {
            bytemuck::bytes_of(&GpuShadow::zeroed()).to_vec()
        } else {
            bytemuck::cast_slice(&plan.shadows).to_vec()
        };
        let mut rebind = self
            .lights
            .write(device, queue, "engine lights", &light_bytes);
        rebind |= self
            .shadows
            .write(device, queue, "engine shadows", &shadow_bytes);
        queue.write_buffer(&self.grid, 0, &plan.header);
        if plan.pages > 0
            && (self.atlas.page_size != frame.atlas.page_size || self.atlas.layers < plan.pages)
        {
            let layers = if self.atlas.page_size == frame.atlas.page_size {
                plan.pages.max(self.atlas.layers)
            } else {
                plan.pages
            };
            self.atlas = Atlas::new(device, frame.atlas.page_size, layers);
            rebind = true;
        }
        if rebind {
            let parts = Parts {
                uniform,
                lights: &self.lights.buffer,
                shadows: &self.shadows.buffer,
                grid: &self.grid,
                lists: &self.lists,
                sampler: &renderer.sampler,
                screen: &self.screen,
            };
            self.bind_group = view_group(device, layouts, &parts, &self.atlas);
            self.shadow_group = view_group(device, layouts, &parts, &self.no_atlas);
            (self.compute_view, self.compute_lists) =
                compute_groups(device, renderer, &parts, &self.stats);
        }
        self.dispatch = frame.clusters;
        if self.dispatch {
            queue.write_buffer(&self.stats, 0, &[0; 16]);
        }
        self.draws = plan.draws;
        self.report = plan.report;
        self.report.cluster_stats = self.stats_seen;
        Ok(plan.lights.len() as u32)
    }

    /// Start mapping the cluster readback encoded this frame (call after the
    /// submission that contains the cluster pass).
    pub fn after_submit(&mut self) {
        if !self.readback.encoded {
            return;
        }
        self.readback.encoded = false;
        self.readback.pending_timed = self.readback.timed;
        self.readback
            .state
            .store(READBACK_PENDING, Ordering::Release);
        let state = self.readback.state.clone();
        self.readback
            .buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                state.store(
                    if result.is_ok() {
                        READBACK_READY
                    } else {
                        READBACK_FAILED
                    },
                    Ordering::Release,
                );
            });
    }

    /// Take a completed cluster readback without waiting.
    fn collect_readback(&mut self, gpu: &GpuContext) {
        if self.readback.state.load(Ordering::Acquire) == READBACK_PENDING {
            let _ = gpu.device.poll(wgpu::PollType::Poll);
        }
        match self.readback.state.load(Ordering::Acquire) {
            READBACK_READY => {
                let stats = self
                    .readback
                    .buffer
                    .slice(..)
                    .get_mapped_range()
                    .ok()
                    .map(|bytes| {
                        let words: &[u32] = bytemuck::cast_slice(&bytes[..16]);
                        let ticks: &[u64] = bytemuck::cast_slice(&bytes[16..32]);
                        let period = f64::from(gpu.queue.get_timestamp_period());
                        ClusterStats {
                            overflowed_clusters: words[0],
                            longest_list: words[1],
                            stored_entries: words[2],
                            nonempty_clusters: words[3],
                            gpu_nanoseconds: (self.readback.pending_timed && ticks[1] >= ticks[0])
                                .then(|| ((ticks[1] - ticks[0]) as f64 * period) as u64),
                        }
                    });
                self.readback.buffer.unmap();
                if stats.is_some() {
                    self.stats_seen = stats;
                }
                self.readback.state.store(READBACK_IDLE, Ordering::Release);
            }
            READBACK_FAILED => self.readback.state.store(READBACK_IDLE, Ordering::Release),
            _ => {}
        }
    }
}

struct Parts<'a> {
    uniform: &'a wgpu::Buffer,
    lights: &'a wgpu::Buffer,
    shadows: &'a wgpu::Buffer,
    grid: &'a wgpu::Buffer,
    lists: &'a wgpu::Buffer,
    sampler: &'a wgpu::Sampler,
    screen: &'a [wgpu::TextureView; 4],
}

fn view_group(
    device: &wgpu::Device,
    layouts: &PipelineLayouts,
    parts: &Parts<'_>,
    atlas: &Atlas,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("engine view"),
        layout: &layouts.view,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: parts.uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: parts.lights.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: parts.shadows.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&atlas.array_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: parts.grid.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: parts.lists.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(parts.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: wgpu::BindingResource::TextureView(&parts.screen[0]),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: wgpu::BindingResource::TextureView(&parts.screen[1]),
            },
            wgpu::BindGroupEntry {
                binding: 9,
                resource: wgpu::BindingResource::TextureView(&parts.screen[2]),
            },
            wgpu::BindGroupEntry {
                binding: 10,
                resource: wgpu::BindingResource::TextureView(&parts.screen[3]),
            },
        ],
    })
}

fn compute_groups(
    device: &wgpu::Device,
    renderer: &LightingRenderer,
    parts: &Parts<'_>,
    stats: &wgpu::Buffer,
) -> (wgpu::BindGroup, wgpu::BindGroup) {
    let view = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("engine cluster view"),
        layout: &renderer.compute_view_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: parts.uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: parts.lights.as_entire_binding(),
            },
        ],
    });
    let lists = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("engine cluster lists"),
        layout: &renderer.compute_lists_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: parts.grid.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: parts.lists.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: stats.as_entire_binding(),
            },
        ],
    });
    (view, lists)
}

/// The `engine::view` layout entries 4..=10 (cluster grid, cluster lists,
/// shadow comparison sampler, the four screen-lighting textures) appended to
/// the view layout.
pub(crate) fn view_layout_entries(
    visibility: wgpu::ShaderStages,
) -> [wgpu::BindGroupLayoutEntry; 7] {
    let storage = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let screen = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    [
        storage(4),
        storage(5),
        wgpu::BindGroupLayoutEntry {
            binding: 6,
            visibility,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
            count: None,
        },
        screen(7),
        screen(8),
        screen(9),
        screen(10),
    ]
}

/// IEEE half of a finite `value` (mantissa truncated; exact for the
/// screen-lighting defaults).
fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x007f_ffff;
    if value == 0.0 || exponent <= 0 {
        return sign;
    }
    if exponent >= 31 {
        return sign | 0x7c00;
    }
    sign | ((exponent as u16) << 10) | ((mantissa >> 13) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lights::{CascadeSettings, ShadowFilter};
    use crate::view::ViewId;

    fn view() -> ViewData {
        let projection = glam::camera::rh::proj::directx::perspective(1.2, 16.0 / 9.0, 0.1, 500.0);
        ViewData::new(
            ViewId::MAIN,
            Mat4::from_translation(Vec3::new(0.0, 2.0, 0.0)),
            projection,
            [320, 180],
            [320, 180],
        )
        .unwrap()
    }

    fn frame<'a>(view: &'a ViewData, lights: &'a LightList) -> LightingFrame<'a> {
        LightingFrame {
            view,
            lights,
            shadows: true,
            clusters: true,
            atlas: ShadowAtlasConfig::default(),
        }
    }

    #[test]
    fn frustum_culling_drops_only_invisible_local_lights() {
        let view = view();
        let lights = LightList {
            lights: vec![
                Light::point(Vec3::new(0.0, 2.0, -10.0), Vec3::ONE, 10.0, 2.0),
                Light::point(Vec3::new(0.0, 2.0, 10.0), Vec3::ONE, 10.0, 2.0),
                Light::point(Vec3::new(0.0, 2.0, 3.0), Vec3::ONE, 10.0, 5.0),
                Light::point(Vec3::new(0.0, 2.0, -10.0), Vec3::ONE, 0.0, 2.0),
                Light::directional(Vec3::NEG_Y, Vec3::ONE, 3.0),
            ],
        };
        let plan = plan_lights(&frame(&view, &lights)).unwrap();
        let r = &plan.report;
        assert_eq!(
            (r.submitted, r.culled, r.directional, r.local),
            (5, 2, 1, 2)
        );
        assert_eq!(r.clustered, 2);
        assert_eq!(plan.directional, 1);
        assert_eq!(plan.lights[0].kind, LightKind::Directional as u32);
        assert_eq!(plan.lights.len(), 3);
    }

    #[test]
    fn the_light_budget_keeps_the_nearest_lights() {
        let view = view();
        let lights = LightList {
            lights: (0..MAX_LIGHTS + 10)
                .map(|i| {
                    Light::point(
                        Vec3::new(0.0, 2.0, -1.0 - i as f32 * 0.01),
                        Vec3::ONE,
                        1.0,
                        0.5,
                    )
                })
                .collect(),
        };
        let plan = plan_lights(&frame(&view, &lights)).unwrap();
        assert_eq!(plan.lights.len(), MAX_LIGHTS);
        assert_eq!(plan.report.over_budget, 10);
        assert!(plan.report.overflow[0].contains("budget"));
        // The farthest were dropped.
        let farthest = plan
            .lights
            .iter()
            .map(|l| -l.position[2])
            .fold(0.0, f32::max);
        assert!(farthest < 1.0 + MAX_LIGHTS as f32 * 0.01);
    }

    #[test]
    fn shadows_are_packed_and_overflow_is_reported() {
        let view = view();
        let shadow = ShadowSettings {
            resolution: 2048,
            ..Default::default()
        };
        let lights = LightList {
            lights: vec![
                Light::directional(Vec3::new(0.2, -1.0, 0.1), Vec3::ONE, 3.0).with_shadow(
                    ShadowSettings {
                        resolution: 2048,
                        cascades: CascadeSettings {
                            count: 4,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                ),
                Light::point(Vec3::new(0.0, 2.0, -5.0), Vec3::ONE, 10.0, 4.0).with_shadow(shadow),
                Light::spot(
                    Vec3::new(1.0, 3.0, -4.0),
                    Vec3::NEG_Y,
                    Vec3::ONE,
                    10.0,
                    6.0,
                    0.3,
                    0.5,
                )
                .with_shadow(ShadowSettings {
                    resolution: 512,
                    filter: ShadowFilter::Pcss {
                        source_size: 0.1,
                        max_radius: 16,
                    },
                    ..Default::default()
                }),
            ],
        };
        let mut f = frame(&view, &lights);
        f.atlas = ShadowAtlasConfig {
            page_size: 4096,
            max_pages: 2,
        };
        let plan = plan_lights(&f).unwrap();
        let r = &plan.report;
        // 4 cascades of 2048^2 fill one page; 6 faces of 2048^2 need 1.5
        // pages, so the point light is dropped and the spot fits.
        assert_eq!(r.shadowed_lights, 2, "{r:?}");
        assert_eq!(r.shadow_views, 5);
        assert_eq!(r.shadow_dropped, 1);
        assert!(r.overflow[0].contains("atlas"), "{:?}", r.overflow);
        assert_eq!(r.atlas_pages, 2);
        assert_eq!(plan.lights[0].shadow_index, 0);
        let point = plan.lights.iter().find(|l| l.kind == 1).unwrap();
        assert_eq!(point.shadow_index, -1);
        let spot = plan.lights.iter().find(|l| l.kind == 2).unwrap();
        assert_eq!(spot.shadow_index, 4);
        assert_eq!(plan.shadows[4].sampling[0], 2.0, "pcss");
        // Cascade splits are increasing and reach the max distance.
        let splits: Vec<f32> = plan.shadows[..4].iter().map(|s| s.range[3]).collect();
        assert!(splits.windows(2).all(|w| w[0] < w[1]));
        assert!((splits[3] - 100.0).abs() < 1e-3);
        assert_eq!(plan.draws.len(), 5);
        let mut disabled = frame(&view, &lights);
        disabled.shadows = false;
        let plan = plan_lights(&disabled).unwrap();
        assert!(plan.lights.iter().all(|l| l.shadow_index == -1));
        assert!(!plan.report.shadows_enabled);
    }

    #[test]
    fn the_cluster_header_slices_up_to_the_farthest_light() {
        let view = view();
        let lights = LightList {
            lights: vec![Light::point(
                Vec3::new(0.0, 2.0, -20.0),
                Vec3::ONE,
                1.0,
                5.0,
            )],
        };
        let plan = plan_lights(&frame(&view, &lights)).unwrap();
        let dims: &[u32] = bytemuck::cast_slice(&plan.header[0..16]);
        let depth: &[f32] = bytemuck::cast_slice(&plan.header[16..32]);
        let params: &[u32] = bytemuck::cast_slice(&plan.header[48..64]);
        assert_eq!(dims, [16, 9, 24, MAX_LIGHTS_PER_CLUSTER]);
        assert!((depth[0] - 0.1).abs() < 1e-4 && (depth[1] - 25.0).abs() < 1e-3);
        // Slice of a depth: log(d) * scale + bias spans 0..24 over near..far.
        let slice = |d: f32| d.ln() * depth[2] + depth[3];
        assert!(slice(0.1).abs() < 1e-3 && (slice(25.0) - 24.0).abs() < 1e-3);
        assert_eq!(params, [0, 0, 1, 0]);
        let mut all = frame(&view, &lights);
        all.clusters = false;
        let plan = plan_lights(&all).unwrap();
        assert_eq!(bytemuck::cast_slice::<u8, u32>(&plan.header[48..52])[0], 1);
        assert_eq!(plan.report.cluster_mode, ClusterMode::AllLights);
        assert_eq!(plan.report.clustered, 0);
    }
}
