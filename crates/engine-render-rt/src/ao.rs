//! RT ambient occlusion.

use crate::device::{Core, RtDevice};
use crate::error::RtError;
use crate::kernel::{self, RtGBuffer, RtScalarFormat};
use crate::scene::RtScene;
use crate::settings::RtAoSettings;
use crate::wgsl;
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;

/// Textures and frame of one AO dispatch.
pub struct AoInputs<'a> {
    pub gbuffer: RtGBuffer<'a>,
    /// Storage view of the kernel's format, `width x height`.
    pub output: &'a wgpu::TextureView,
    /// Decorrelates the ray directions between frames.
    pub frame_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuAoParams {
    ray: [f32; 4],
    size: [u32; 4],
    frame: [u32; 4],
}

const AO_SEED: u32 = 0xa0a0_1337;

fn gpu_params(width: u32, height: u32, frame_index: u32, s: &RtAoSettings) -> GpuAoParams {
    GpuAoParams {
        ray: [s.radius, s.normal_bias, 0.0, 0.0],
        size: [width, height, s.rays_per_pixel, 0xff],
        frame: [frame_index, AO_SEED, 0, 0],
    }
}

/// Compute kernel `ambient_occlusion` (`src/shaders/ambient_occlusion.wgsl`),
/// workgroup 8 x 8, one invocation per pixel.
///
/// Per pixel, `rays_per_pixel` cosine-weighted rays of length `radius` start
/// at the G-buffer position offset by `normal_bias` along the normal; the
/// output is `1 - occluded / rays` (1 = open, 0 = fully occluded). Background
/// pixels are 1. Output `R16Float` (or `R8Unorm`/`R32Float`).
///
/// Bindings: group 0 = the scene; group 1 = 0 world positions, 1 world normals
/// (`texture_2d<f32>`, unfilterable), 2 `texture_storage_2d<format, write>`,
/// 3 uniform `{ ray: vec4<f32> (radius, normal_bias), size: vec4<u32> (width,
/// height, rays, mask), frame: vec4<u32> (frame, seed) }`.
pub struct AmbientOcclusionKernel {
    core: Arc<Core>,
    format: RtScalarFormat,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl AmbientOcclusionKernel {
    pub fn new(rt: &RtDevice, format: RtScalarFormat) -> Result<Self, RtError> {
        let core = rt.core().clone();
        core.ensure_usable()?;
        kernel::check_storage_format(rt.supports_storage_format(format.texture_format()), format)?;
        let device = &core.device;
        let layout = kernel::bind_group_layout(
            device,
            "engine-render-rt ambient occlusion",
            &[
                kernel::texture_entry(0),
                kernel::texture_entry(1),
                kernel::storage_texture_entry(2, format.texture_format()),
                kernel::uniform_entry(3),
            ],
        );
        let pipeline = kernel::compute_pipeline(
            device,
            "engine-render-rt ambient occlusion",
            &[&core.scene_layout, &layout],
            wgsl::ambient_occlusion_source(format.wgsl()),
            "ambient_occlusion",
        );
        Ok(Self {
            core,
            format,
            pipeline,
            layout,
        })
    }

    pub fn format(&self) -> RtScalarFormat {
        self.format
    }

    /// Records one dispatch. The scene must be committed; `settings` are
    /// validated (`enabled` and `denoise` are for the caller).
    pub fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &RtScene,
        inputs: &AoInputs<'_>,
        settings: &RtAoSettings,
    ) -> Result<(), RtError> {
        let scene_group = kernel::scene_group(&self.core, scene)?;
        let g = &inputs.gbuffer;
        kernel::check_size(&self.core, "ambient occlusion", g.width, g.height)?;
        settings.validate()?;
        let uniform = gpu_params(g.width, g.height, inputs.frame_index, settings);
        let device = &self.core.device;
        let buffer = kernel::uniform_buffer(
            device,
            "engine-render-rt ambient occlusion params",
            bytemuck::bytes_of(&uniform),
        );
        let group = kernel::bind_group(
            device,
            "engine-render-rt ambient occlusion",
            &self.layout,
            &[
                wgpu::BindingResource::TextureView(g.world_position),
                wgpu::BindingResource::TextureView(g.world_normal),
                wgpu::BindingResource::TextureView(inputs.output),
                buffer.as_entire_binding(),
            ],
        );
        kernel::dispatch(
            encoder,
            "engine-render-rt ambient occlusion",
            &self.pipeline,
            &[scene_group, &group],
            g.width,
            g.height,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_layout_matches_wgsl() {
        assert_eq!(size_of::<GpuAoParams>(), 48);
        let p = gpu_params(3, 5, 9, &RtAoSettings::default());
        assert_eq!(p.ray[0], 1.0);
        assert_eq!(p.size, [3, 5, 1, 0xff]);
        assert_eq!(p.frame[0], 9);
    }
}
