//! Per-view forward-Z maximum-depth pyramid and conservative visibility queries.
//! The caller owns frame ordering and must encode after scene depth is complete.

use anyhow::{Result, ensure};
use bytemuck::{Pod, Zeroable};
use engine_render_wgpu::GpuContext;
use wgpu::util::DeviceExt;

/// Maximum supported viewport edge. Keeps work and allocations predictable.
pub const MAX_EDGE: u32 = 4096;
/// Maximum number of bytes used by all pyramid mips in one view.
pub const MAX_PYRAMID_BYTES: u64 = 96 * 1024 * 1024;
/// Maximum queries encoded in one batch.
pub const MAX_QUERIES: u32 = 65_536;

const WORKGROUP: u32 = 8;
const QUERY_WORKGROUP: u32 = 64;

/// A conservative preprojected rectangle in viewport-local, half-open pixels.
///
/// The producer must enclose every pixel touched by the object's projected
/// bounds and provide the nearest possible forward-Z depth of the object.
/// Set `uncertain` for near-plane clipping or uncertain projection. This
/// kernel does not transform world-space bounds or choose an LOD.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct VisibilityQuery {
    pub rect: [i32; 4],
    pub nearest_depth: f32,
    pub uncertain: u32,
    pub padding: [u32; 2],
}

impl VisibilityQuery {
    pub fn new(rect: [i32; 4], nearest_depth: f32, uncertain: bool) -> Self {
        Self {
            rect,
            nearest_depth,
            uncertain: u32::from(uncertain),
            padding: [0; 2],
        }
    }
}

/// GPU output: one little-endian u32 per query, 1 visible or 0 occluded.
/// The caller can copy this buffer or bind it as a storage input to later GPU
/// work. Merely encoding a query does not suppress any renderer draw.
pub struct VisibilityOutput {
    pub buffer: wgpu::Buffer,
    pub count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params([u32; 4]);

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_texture_entry() -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: 1,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::R32Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn buffer_entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn compute_pipeline(
    device: &wgpu::Device,
    label: &'static str,
    layout: &wgpu::BindGroupLayout,
    shader: &'static str,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(shader.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    })
}

fn group(
    device: &wgpu::Device,
    label: &'static str,
    layout: &wgpu::BindGroupLayout,
    resources: &[wgpu::BindingResource<'_>],
) -> wgpu::BindGroup {
    let entries: Vec<_> = resources
        .iter()
        .enumerate()
        .map(|(i, resource)| wgpu::BindGroupEntry {
            binding: i as u32,
            resource: resource.clone(),
        })
        .collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &entries,
    })
}

/// Device-owned immutable compute pipelines. Reuse across independent views.
pub struct HiZKernel {
    base_layout: wgpu::BindGroupLayout,
    reduce_layout: wgpu::BindGroupLayout,
    query_layout: wgpu::BindGroupLayout,
    base: wgpu::ComputePipeline,
    reduce: wgpu::ComputePipeline,
    query: wgpu::ComputePipeline,
}

impl HiZKernel {
    pub fn new(gpu: &GpuContext) -> Result<Self> {
        let features = gpu
            .adapter
            .get_texture_format_features(wgpu::TextureFormat::R32Float);
        ensure!(
            features.allowed_usages.contains(
                wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
            ),
            "R32Float storage and sampled texture usage unavailable"
        );
        let limits = gpu.device.limits();
        ensure!(
            limits.max_compute_workgroup_size_x >= 64
                && limits.max_compute_workgroup_size_y >= 8
                && limits.max_compute_workgroup_size_z >= 1
                && limits.max_compute_invocations_per_workgroup >= 64,
            "Hi-Z compute kernels require workgroups up to 64x1x1 and 8x8x1"
        );
        ensure!(
            limits.max_bind_groups >= 1 && limits.max_bindings_per_bind_group >= 4,
            "one bind group with four bindings required"
        );
        ensure!(
            limits.max_uniform_buffer_binding_size >= 16,
            "16-byte uniform buffer binding required"
        );
        ensure!(
            limits.max_storage_textures_per_shader_stage >= 1,
            "one storage texture binding required"
        );
        ensure!(
            limits.max_sampled_textures_per_shader_stage >= 1,
            "one sampled texture binding required"
        );
        ensure!(
            limits.max_storage_buffers_per_shader_stage >= 2,
            "two storage buffer bindings required"
        );
        ensure!(
            limits.max_uniform_buffers_per_shader_stage >= 1,
            "one uniform binding required"
        );

        let depth = texture_entry(0, wgpu::TextureSampleType::Depth);
        let float = texture_entry(0, wgpu::TextureSampleType::Float { filterable: false });
        let storage = storage_texture_entry();
        let uniform = buffer_entry(2, wgpu::BufferBindingType::Uniform);
        let base_layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("hiz base"),
                entries: &[depth, storage, uniform],
            });
        let reduce_layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("hiz reduce"),
                entries: &[float, storage, uniform],
            });
        let query_layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("hiz query"),
                entries: &[
                    float,
                    buffer_entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
                    buffer_entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
                    buffer_entry(3, wgpu::BufferBindingType::Uniform),
                ],
            });
        let base = compute_pipeline(
            &gpu.device,
            "hiz base",
            &base_layout,
            include_str!("base.wgsl"),
        );
        let reduce = compute_pipeline(
            &gpu.device,
            "hiz reduce",
            &reduce_layout,
            include_str!("reduce.wgsl"),
        );
        let query = compute_pipeline(
            &gpu.device,
            "hiz query",
            &query_layout,
            include_str!("query.wgsl"),
        );
        Ok(Self {
            base_layout,
            reduce_layout,
            query_layout,
            base,
            reduce,
            query,
        })
    }

    pub fn create_view(&self, gpu: &GpuContext, size: [u32; 2], capacity: u32) -> Result<HiZView> {
        HiZView::new(gpu, size, capacity)
    }

    /// Fully overwrite the pyramid and optionally write a new visibility
    /// buffer. Submit this encoder after all depth writes for this view.
    /// The source must be a single-sample Depth32Float 2D texture with
    /// TEXTURE_BINDING usage. The viewport is [x, y, width, height] within it.
    pub fn encode(
        &self,
        gpu: &GpuContext,
        encoder: &mut wgpu::CommandEncoder,
        view: &HiZView,
        depth: &wgpu::Texture,
        viewport: [u32; 4],
        queries: &[VisibilityQuery],
    ) -> Result<VisibilityOutput> {
        ensure!(
            viewport[2] == view.size[0] && viewport[3] == view.size[1],
            "viewport must match Hi-Z view size"
        );
        ensure!(
            depth.format() == wgpu::TextureFormat::Depth32Float,
            "source must be Depth32Float"
        );
        ensure!(
            depth.dimension() == wgpu::TextureDimension::D2
                && depth.sample_count() == 1
                && depth.size().depth_or_array_layers == 1,
            "source must be single-layer, single-sample 2D"
        );
        ensure!(
            depth.usage().contains(wgpu::TextureUsages::TEXTURE_BINDING),
            "source depth needs TEXTURE_BINDING usage"
        );
        let end_x = viewport[0].checked_add(viewport[2]);
        let end_y = viewport[1].checked_add(viewport[3]);
        ensure!(
            end_x.is_some_and(|x| x <= depth.width()) && end_y.is_some_and(|y| y <= depth.height()),
            "viewport exceeds source depth"
        );
        ensure!(
            queries.len() <= view.capacity as usize,
            "query count exceeds view capacity"
        );

        let source = depth.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            ..Default::default()
        });
        let params = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("hiz viewport"),
                contents: bytemuck::bytes_of(&Params(viewport)),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let base_group = group(
            &gpu.device,
            "hiz base",
            &self.base_layout,
            &[
                wgpu::BindingResource::TextureView(&source),
                wgpu::BindingResource::TextureView(&view.mips[0]),
                params.as_entire_binding(),
            ],
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("hiz base"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.base);
            pass.set_bind_group(0, &base_group, &[]);
            pass.dispatch_workgroups(
                view.size[0].div_ceil(WORKGROUP),
                view.size[1].div_ceil(WORKGROUP),
                1,
            );
        }
        for level in 1..view.dimensions.len() {
            let src = view.dimensions[level - 1];
            let dst = view.dimensions[level];
            let dimensions = Params([src[0], src[1], dst[0], dst[1]]);
            let params = gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("hiz mip dimensions"),
                    contents: bytemuck::bytes_of(&dimensions),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let bind = group(
                &gpu.device,
                "hiz reduce",
                &self.reduce_layout,
                &[
                    wgpu::BindingResource::TextureView(&view.mips[level - 1]),
                    wgpu::BindingResource::TextureView(&view.mips[level]),
                    params.as_entire_binding(),
                ],
            );
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("hiz reduce"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.reduce);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(dst[0].div_ceil(WORKGROUP), dst[1].div_ceil(WORKGROUP), 1);
        }

        let bytes = (queries.len() as u64).max(1) * 4;
        let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hiz visibility"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        if !queries.is_empty() {
            let input = gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("hiz queries"),
                    contents: bytemuck::cast_slice(queries),
                    usage: wgpu::BufferUsages::STORAGE,
                });
            let params = gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("hiz query params"),
                    contents: bytemuck::bytes_of(&Params([
                        view.size[0],
                        view.size[1],
                        queries.len() as u32,
                        view.mips.len() as u32,
                    ])),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let bind = group(
                &gpu.device,
                "hiz query",
                &self.query_layout,
                &[
                    wgpu::BindingResource::TextureView(&view.all),
                    input.as_entire_binding(),
                    output.as_entire_binding(),
                    params.as_entire_binding(),
                ],
            );
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("hiz query"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.query);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups((queries.len() as u32).div_ceil(QUERY_WORKGROUP), 1, 1);
        }
        Ok(VisibilityOutput {
            buffer: output,
            count: queries.len() as u32,
        })
    }
}

/// Resources for one view. Recreate on resize, or call `resize`. Each encode
/// overwrites every mip before any query, so there is no previous-frame input.
pub struct HiZView {
    texture: wgpu::Texture,
    all: wgpu::TextureView,
    mips: Vec<wgpu::TextureView>,
    dimensions: Vec<[u32; 2]>,
    size: [u32; 2],
    capacity: u32,
}

impl HiZView {
    fn new(gpu: &GpuContext, size: [u32; 2], capacity: u32) -> Result<Self> {
        let dimensions = mip_dimensions(size)?;
        ensure!(
            capacity <= MAX_QUERIES,
            "query capacity exceeds {MAX_QUERIES}"
        );
        let limits = gpu.device.limits();
        ensure!(
            size[0] <= limits.max_texture_dimension_2d
                && size[1] <= limits.max_texture_dimension_2d,
            "view exceeds device 2D texture limit"
        );
        ensure!(
            dimensions.iter().all(|d| d[0].div_ceil(WORKGROUP)
                <= limits.max_compute_workgroups_per_dimension
                && d[1].div_ceil(WORKGROUP) <= limits.max_compute_workgroups_per_dimension),
            "view exceeds compute dispatch limit"
        );
        ensure!(
            capacity.div_ceil(QUERY_WORKGROUP) <= limits.max_compute_workgroups_per_dimension,
            "query count exceeds compute dispatch limit"
        );
        let input_bytes = u64::from(capacity) * std::mem::size_of::<VisibilityQuery>() as u64;
        let output_bytes = u64::from(capacity).max(1) * 4;
        ensure!(
            input_bytes <= limits.max_buffer_size
                && input_bytes <= u64::from(limits.max_storage_buffer_binding_size),
            "query input exceeds device storage buffer limit"
        );
        ensure!(
            output_bytes <= limits.max_buffer_size
                && output_bytes <= u64::from(limits.max_storage_buffer_binding_size),
            "query output exceeds device storage buffer limit"
        );
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hiz pyramid"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: dimensions.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let all = texture.create_view(&Default::default());
        let mips = (0..dimensions.len() as u32)
            .map(|i| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: i,
                    mip_level_count: Some(1),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    ..Default::default()
                })
            })
            .collect();
        Ok(Self {
            texture,
            all,
            mips,
            dimensions,
            size,
            capacity,
        })
    }

    pub fn resize(&mut self, gpu: &GpuContext, size: [u32; 2]) -> Result<()> {
        if self.size != size {
            *self = Self::new(gpu, size, self.capacity)?;
        }
        Ok(())
    }

    pub fn size(&self) -> [u32; 2] {
        self.size
    }
    pub fn capacity(&self) -> u32 {
        self.capacity
    }
    pub fn mip_dimensions(&self) -> &[[u32; 2]] {
        &self.dimensions
    }
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

/// Standard wgpu mip dimensions, with checked memory accounting.
pub fn mip_dimensions(size: [u32; 2]) -> Result<Vec<[u32; 2]>> {
    ensure!(
        size[0] > 0 && size[1] > 0 && size[0] <= MAX_EDGE && size[1] <= MAX_EDGE,
        "view size must be 1..={MAX_EDGE} on each axis"
    );
    let mut dimensions = Vec::new();
    let mut current = size;
    let mut bytes = 0u64;
    loop {
        bytes = bytes
            .checked_add(u64::from(current[0]) * u64::from(current[1]) * 4)
            .ok_or_else(|| anyhow::anyhow!("pyramid byte count overflow"))?;
        ensure!(
            bytes <= MAX_PYRAMID_BYTES,
            "pyramid exceeds {MAX_PYRAMID_BYTES} bytes"
        );
        dimensions.push(current);
        if current == [1, 1] {
            break;
        }
        current = [(current[0] / 2).max(1), (current[1] / 2).max(1)];
    }
    Ok(dimensions)
}

/// CPU reference used for exact footprint and visibility tests.
pub fn reference_pyramid(size: [u32; 2], depth: &[f32]) -> Result<Vec<Vec<f32>>> {
    let dimensions = mip_dimensions(size)?;
    ensure!(
        depth.len() == size[0] as usize * size[1] as usize,
        "depth length differs from size"
    );
    let mut levels: Vec<Vec<f32>> = Vec::with_capacity(dimensions.len());
    levels.push(
        depth
            .iter()
            .map(|&d| if (0.0..=1.0).contains(&d) { d } else { 1.0 })
            .collect(),
    );
    for pair in dimensions.windows(2) {
        let src = pair[0];
        let dst = pair[1];
        let previous = levels
            .last()
            .ok_or_else(|| anyhow::anyhow!("missing base mip"))?;
        let mut next = vec![0.0f32; dst[0] as usize * dst[1] as usize];
        for y in 0..dst[1] {
            for x in 0..dst[0] {
                let x0 = x * src[0] / dst[0];
                let x1 = (x + 1) * src[0] / dst[0];
                let y0 = y * src[1] / dst[1];
                let y1 = (y + 1) * src[1] / dst[1];
                let mut greatest = 0.0f32;
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        greatest = greatest.max(previous[(sy * src[0] + sx) as usize]);
                    }
                }
                next[(y * dst[0] + x) as usize] = greatest;
            }
        }
        levels.push(next);
    }
    Ok(levels)
}

/// CPU query reference. 1 means visible, 0 means safely occluded.
pub fn reference_visibility(
    size: [u32; 2],
    levels: &[Vec<f32>],
    query: VisibilityQuery,
) -> Result<u32> {
    let dimensions = mip_dimensions(size)?;
    ensure!(
        levels.len() == dimensions.len(),
        "level count differs from dimensions"
    );
    let [rx0, ry0, rx1, ry1] = query.rect;
    if query.uncertain != 0
        || !(0.0..=1.0).contains(&query.nearest_depth)
        || rx0 < 0
        || ry0 < 0
        || rx0 >= rx1
        || ry0 >= ry1
        || rx1 as u32 > size[0]
        || ry1 as u32 > size[1]
    {
        return Ok(1);
    }
    let [mut x0, mut y0, mut x1, mut y1] = [rx0 as u32, ry0 as u32, rx1 as u32, ry1 as u32];
    let mut level = 0usize;
    while (x1 - x0 > 4 || y1 - y0 > 4) && level + 1 < dimensions.len() {
        let src = dimensions[level];
        let dst = dimensions[level + 1];
        x0 = ((x0 + 1) * dst[0]).div_ceil(src[0]) - 1;
        y0 = ((y0 + 1) * dst[1]).div_ceil(src[1]) - 1;
        x1 = (x1 * dst[0]).div_ceil(src[0]);
        y1 = (y1 * dst[1]).div_ceil(src[1]);
        level += 1;
    }
    let width = dimensions[level][0];
    let mut greatest = 0.0f32;
    for y in y0..y1 {
        for x in x0..x1 {
            greatest = greatest.max(levels[level][(y * width + x) as usize]);
        }
    }
    Ok(u32::from(query.nearest_depth <= greatest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_mips_cover_every_edge_and_match_reference() {
        for size in [[3, 5], [9, 7], [1, 17], [17, 1]] {
            let mut depth = vec![0.0; size[0] as usize * size[1] as usize];
            *depth.last_mut().unwrap() = 0.93;
            let levels = reference_pyramid(size, &depth).unwrap();
            assert_eq!(*levels.last().unwrap(), vec![0.93]);
            assert_eq!(levels.len(), mip_dimensions(size).unwrap().len());
        }
    }

    #[test]
    fn invalid_depth_and_uncertain_queries_remain_visible() {
        let levels = reference_pyramid([3, 5], &[f32::NAN; 15]).unwrap();
        assert!(levels.iter().all(|l| l.iter().all(|&x| x == 1.0)));
        for query in [
            VisibilityQuery::new([0, 0, 3, 5], 0.9, false),
            VisibilityQuery::new([0, 0, 3, 5], 0.9, true),
            VisibilityQuery::new([0, 0, 4, 5], 0.9, false),
            VisibilityQuery::new([1, 1, 1, 2], 0.9, false),
            VisibilityQuery::new([-1, 0, 1, 2], 0.9, false),
            VisibilityQuery::new([0, 0, 3, 5], f32::NAN, false),
        ] {
            assert_eq!(reference_visibility([3, 5], &levels, query).unwrap(), 1);
        }
    }

    #[test]
    fn query_never_culls_a_directly_visible_depth_sample() {
        for size in [[3, 5], [9, 7], [1, 17], [17, 1]] {
            for variant in 0..3 {
                let mut depth: Vec<f32> = (0..size[0] * size[1])
                    .map(|i| match variant {
                        0 => 0.2,
                        1 => (i * 37 % 97) as f32 / 100.0,
                        _ => 0.1,
                    })
                    .collect();
                if variant == 2 {
                    *depth.last_mut().unwrap() = 0.8;
                }
                let levels = reference_pyramid(size, &depth).unwrap();
                for y0 in 0..size[1] {
                    for x0 in 0..size[0] {
                        for y1 in y0 + 1..=size[1] {
                            for x1 in x0 + 1..=size[0] {
                                // This reads the original base depth directly,
                                // independent of the mip/query footprint mapping.
                                let mut exact_max = 0.0f32;
                                for y in y0..y1 {
                                    for x in x0..x1 {
                                        exact_max =
                                            exact_max.max(depth[(y * size[0] + x) as usize]);
                                    }
                                }
                                for nearest in [0.0, 0.2, 0.5, exact_max, 1.0] {
                                    let query = VisibilityQuery::new(
                                        [x0 as i32, y0 as i32, x1 as i32, y1 as i32],
                                        nearest,
                                        false,
                                    );
                                    if nearest <= exact_max {
                                        assert_eq!(
                                            reference_visibility(size, &levels, query).unwrap(),
                                            1,
                                            "size={size:?} variant={variant} rect={:?} nearest={nearest} exact_max={exact_max}",
                                            query.rect
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn allocation_limits_reject_invalid_extents() {
        assert!(mip_dimensions([0, 1]).is_err());
        assert!(mip_dimensions([MAX_EDGE + 1, 1]).is_err());
        assert!(mip_dimensions([MAX_EDGE, MAX_EDGE]).is_ok());
        assert!(reference_pyramid([3, 5], &[0.0; 14]).is_err());
    }
}
