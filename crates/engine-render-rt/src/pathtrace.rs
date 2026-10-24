//! Reference progressive path tracer (ground truth for later RT passes).

use crate::device::{Core, RtDevice};
use crate::error::{RtError, invalid};
use crate::scene::RtScene;
use crate::wgsl;
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Lambertian material. `material_index` of an instance selects one; indices
/// past the end use the last material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathTracerMaterial {
    /// Diffuse reflectance per channel, in [0, 1].
    pub albedo: [f32; 3],
    /// Emitted radiance per channel, >= 0 (both sides).
    pub emissive: [f32; 3],
}

/// Most materials one tracer holds.
pub const MAX_PATH_TRACER_MATERIALS: usize = 65_536;
/// Largest accepted `max_bounces`.
pub const MAX_PATH_TRACER_BOUNCES: u32 = 1024;

/// Per-frame parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathTracerParams {
    /// Column-major inverse view-projection; a pixel's ray goes from `eye`
    /// through the unprojected point at NDC depth 0.5 (works for standard and
    /// reversed depth).
    pub inv_view_proj: [[f32; 4]; 4],
    pub eye: [f32; 3],
    /// Sky radiance is `horizon + (zenith - horizon) * max(dir.y, 0)`; equal
    /// colours give a uniform sky.
    pub sky_zenith: [f32; 3],
    pub sky_horizon: [f32; 3],
    /// Path vertices after the camera ray (0 = direct sky and emission only).
    pub max_bounces: u32,
    /// Russian roulette applies to continuation number `>= rr_start`
    /// (survival probability = max throughput channel clamped to [0.05, 0.95]).
    pub rr_start: u32,
    pub seed: u32,
    pub mask: u8,
    pub t_max: f32,
    /// Random sub-pixel jitter (anti-aliasing); off samples pixel centres.
    pub jitter: bool,
    /// Ray-origin offset along the normal, scaled by `1 + max |position|`.
    pub ray_offset: f32,
}

impl Default for PathTracerParams {
    fn default() -> Self {
        Self {
            inv_view_proj: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            eye: [0.0; 3],
            sky_zenith: [1.0; 3],
            sky_horizon: [1.0; 3],
            max_bounces: 8,
            rr_start: 3,
            seed: 0,
            mask: 0xff,
            t_max: 1e6,
            jitter: true,
            ray_offset: 1e-4,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuMaterial {
    albedo: [f32; 4],
    emissive: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuParams {
    inv_view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    sky_zenith: [f32; 4],
    sky_horizon: [f32; 4],
    a: [u32; 4],
    b: [u32; 4],
    c: [f32; 4],
}

pub(crate) fn validate_materials(materials: &[PathTracerMaterial]) -> Result<(), RtError> {
    if materials.is_empty() || materials.len() > MAX_PATH_TRACER_MATERIALS {
        return Err(invalid(format!(
            "{} materials; between 1 and {MAX_PATH_TRACER_MATERIALS} are allowed",
            materials.len()
        )));
    }
    for (i, m) in materials.iter().enumerate() {
        if !m.albedo.iter().all(|a| (0.0..=1.0).contains(a)) {
            return Err(invalid(format!(
                "material {i} albedo {:?} must be within [0, 1]",
                m.albedo
            )));
        }
        if !m.emissive.iter().all(|e| e.is_finite() && *e >= 0.0) {
            return Err(invalid(format!(
                "material {i} emissive {:?} must be finite and >= 0",
                m.emissive
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_params(p: &PathTracerParams) -> Result<(), RtError> {
    let finite = p.inv_view_proj.iter().flatten().all(|v| v.is_finite())
        && p.eye.iter().all(|v| v.is_finite());
    if !finite {
        return Err(invalid("inv_view_proj and eye must be finite"));
    }
    if !p
        .sky_zenith
        .iter()
        .chain(&p.sky_horizon)
        .all(|v| v.is_finite() && *v >= 0.0)
    {
        return Err(invalid("sky radiance must be finite and >= 0"));
    }
    if p.max_bounces > MAX_PATH_TRACER_BOUNCES {
        return Err(invalid(format!(
            "max_bounces {} exceeds {MAX_PATH_TRACER_BOUNCES}",
            p.max_bounces
        )));
    }
    if !(p.t_max.is_finite() && p.t_max > 0.0) {
        return Err(invalid("t_max must be finite and > 0"));
    }
    if !(p.ray_offset.is_finite() && p.ray_offset >= 0.0) {
        return Err(invalid("ray_offset must be finite and >= 0"));
    }
    Ok(())
}

/// Compute kernel `path_trace` (`src/shaders/path_trace.wgsl`), workgroup 8 x 8,
/// one sample per pixel per frame, accumulated as a running mean in two
/// ping-pong `Rgba32Float` textures (alpha = samples so far).
///
/// Bindings: group 0 = the scene; group 1 = 0 previous accumulation
/// `texture_2d<f32>`, 1 `texture_storage_2d<rgba32float, write>` output,
/// 2 materials storage, 3 uniform parameters, 4 `array<atomic<u32>, 4>`
/// statistics (`[0]` = non-finite samples discarded).
pub struct PathTracer {
    core: Arc<Core>,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    textures: [wgpu::Texture; 2],
    views: [wgpu::TextureView; 2],
    materials: wgpu::Buffer,
    material_count: u32,
    stats: wgpu::Buffer,
    width: u32,
    height: u32,
    frames: u32,
}

impl PathTracer {
    pub fn new(
        rt: &RtDevice,
        width: u32,
        height: u32,
        materials: &[PathTracerMaterial],
    ) -> Result<Self, RtError> {
        let core = rt.core().clone();
        core.ensure_usable()?;
        let max = core.limits.max_texture_dimension_2d;
        if width == 0 || height == 0 || width > max || height > max {
            return Err(invalid(format!(
                "path tracer size {width}x{height} must be 1..={max} per side"
            )));
        }
        validate_materials(materials)?;
        let device = &core.device;
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let buffer = |ty| wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine-render-rt path tracer"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
                entry(
                    2,
                    buffer(wgpu::BufferBindingType::Storage { read_only: true }),
                ),
                entry(3, buffer(wgpu::BufferBindingType::Uniform)),
                entry(
                    4,
                    buffer(wgpu::BufferBindingType::Storage { read_only: false }),
                ),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("engine-render-rt path tracer"),
            bind_group_layouts: &[Some(&core.scene_layout), Some(&layout)],
            immediate_size: 0,
        });
        let module = wgsl::module(
            device,
            "engine-render-rt path tracer",
            wgsl::path_trace_source(),
        );
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("engine-render-rt path tracer"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("path_trace"),
            compilation_options: Default::default(),
            cache: None,
        });
        let texture = || {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("engine-render-rt path tracer accumulation"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let textures = [texture(), texture()];
        let views = [
            textures[0].create_view(&Default::default()),
            textures[1].create_view(&Default::default()),
        ];
        let stats = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("engine-render-rt path tracer stats"),
            contents: &[0u8; 16],
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        });
        let (materials_buffer, material_count) = upload_materials(device, materials);
        Ok(Self {
            core,
            pipeline,
            layout,
            textures,
            views,
            materials: materials_buffer,
            material_count,
            stats,
            width,
            height,
            frames: 0,
        })
    }

    /// Replaces the materials and restarts accumulation.
    pub fn set_materials(&mut self, materials: &[PathTracerMaterial]) -> Result<(), RtError> {
        validate_materials(materials)?;
        let (buffer, count) = upload_materials(&self.core.device, materials);
        self.materials = buffer;
        self.material_count = count;
        self.reset();
        Ok(())
    }

    /// Restarts accumulation (camera, scene or sky changed).
    pub fn reset(&mut self) {
        self.frames = 0;
    }

    /// Samples accumulated per pixel.
    pub fn frame_count(&self) -> u32 {
        self.frames
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The accumulation texture holding the current mean (rgb) and sample
    /// count (alpha), valid once a frame was recorded. Changes after every
    /// frame (ping-pong).
    pub fn output(&self) -> &wgpu::Texture {
        &self.textures[self.frames as usize % 2]
    }

    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.views[self.frames as usize % 2]
    }

    /// The statistics buffer (`[0]` = non-finite samples discarded since creation).
    pub fn stats_buffer(&self) -> &wgpu::Buffer {
        &self.stats
    }

    /// Reads the number of non-finite samples discarded so far (blocking).
    pub fn read_non_finite_samples(&self) -> Result<u32, RtError> {
        let bytes = self
            .core
            .read_buffer(&self.stats, 16, "path tracer stats")?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Records one sample per pixel into `encoder` and advances the frame count.
    pub fn record_frame(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &RtScene,
        params: &PathTracerParams,
    ) -> Result<(), RtError> {
        self.core.ensure_usable()?;
        if !scene.is_from(&self.core) {
            return Err(invalid("the scene belongs to another RtDevice"));
        }
        validate_params(params)?;
        let scene_group = scene.committed_bind_group()?;
        let [x, y, z] = params.eye;
        let v4 = |v: [f32; 3]| [v[0], v[1], v[2], 0.0];
        let uniform = GpuParams {
            inv_view_proj: params.inv_view_proj,
            eye: [x, y, z, 1.0],
            sky_zenith: v4(params.sky_zenith),
            sky_horizon: v4(params.sky_horizon),
            a: [self.width, self.height, self.frames, params.max_bounces],
            b: [
                params.rr_start,
                params.seed,
                u32::from(params.mask),
                self.material_count,
            ],
            c: [
                params.t_max,
                if params.jitter { 1.0 } else { 0.0 },
                params.ray_offset,
                0.0,
            ],
        };
        let device = &self.core.device;
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("engine-render-rt path tracer params"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let read = self.frames as usize % 2;
        let write = (read + 1) % 2;
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine-render-rt path tracer"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.views[read]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.views[write]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.materials.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.stats.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("engine-render-rt path tracer"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, scene_group, &[]);
            pass.set_bind_group(1, &group, &[]);
            pass.dispatch_workgroups(self.width.div_ceil(8), self.height.div_ceil(8), 1);
        }
        self.frames += 1;
        Ok(())
    }
}

fn upload_materials(
    device: &wgpu::Device,
    materials: &[PathTracerMaterial],
) -> (wgpu::Buffer, u32) {
    let records: Vec<GpuMaterial> = materials
        .iter()
        .map(|m| GpuMaterial {
            albedo: [m.albedo[0], m.albedo[1], m.albedo[2], 0.0],
            emissive: [m.emissive[0], m.emissive[1], m.emissive[2], 0.0],
        })
        .collect();
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("engine-render-rt path tracer materials"),
        contents: bytemuck::cast_slice(&records),
        usage: wgpu::BufferUsages::STORAGE,
    });
    (buffer, materials.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materials_and_parameters_are_validated() {
        let ok = PathTracerMaterial {
            albedo: [0.5; 3],
            emissive: [0.0; 3],
        };
        assert!(validate_materials(&[ok]).is_ok());
        assert!(validate_materials(&[]).is_err());
        let bright = PathTracerMaterial {
            albedo: [1.2, 0.5, 0.5],
            ..ok
        };
        assert!(
            validate_materials(&[bright])
                .unwrap_err()
                .to_string()
                .contains("[0, 1]")
        );
        let nan = PathTracerMaterial {
            emissive: [f32::NAN, 0.0, 0.0],
            ..ok
        };
        assert!(validate_materials(&[nan]).is_err());
        let p = PathTracerParams::default();
        assert!(validate_params(&p).is_ok());
        assert!(
            validate_params(&PathTracerParams {
                max_bounces: 5000,
                ..p
            })
            .is_err()
        );
        assert!(validate_params(&PathTracerParams { t_max: 0.0, ..p }).is_err());
        assert!(
            validate_params(&PathTracerParams {
                eye: [f32::NAN, 0.0, 0.0],
                ..p
            })
            .is_err()
        );
        assert!(
            validate_params(&PathTracerParams {
                sky_zenith: [-1.0, 0.0, 0.0],
                ..p
            })
            .is_err()
        );
    }

    #[test]
    fn uniform_layout_matches_wgsl() {
        // mat4x4 (64) + 3 x vec4<f32> (48) + 2 x vec4<u32> (32) + vec4<f32> (16).
        assert_eq!(size_of::<GpuParams>(), 160);
        assert_eq!(size_of::<GpuMaterial>(), 32);
    }
}
