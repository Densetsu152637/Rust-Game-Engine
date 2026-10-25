//! RT diffuse global illumination.

use crate::device::{Core, RtDevice};
use crate::error::{RtError, invalid};
use crate::kernel::{self, GpuLighting, RtGBuffer, RtLighting};
use crate::material::RtMaterialTable;
use crate::scene::RtScene;
use crate::settings::RtGiSettings;
use crate::wgsl;
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;

/// Textures, materials and lighting of one diffuse GI dispatch.
pub struct GiInputs<'a> {
    pub gbuffer: RtGBuffer<'a>,
    /// `Rgba16Float` storage view, `width x height`.
    pub output: &'a wgpu::TextureView,
    pub materials: &'a RtMaterialTable,
    pub lighting: &'a RtLighting,
    /// Decorrelates the paths between frames.
    pub frame_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuGiParams {
    lighting: GpuLighting,
    ray: [f32; 4],
    size: [u32; 4],
    frame: [u32; 4],
}

const GI_SEED: u32 = 0x61b0_0c35;

/// Compute kernel `diffuse_gi` (`src/shaders/diffuse_gi.wgsl`), workgroup
/// 8 x 8.
///
/// Per pixel, `rays_per_pixel` cosine-weighted paths of up to `bounces`
/// segments. Every hit adds its emission and its albedo lit by the sun
/// (next-event estimation with one `rt_occluded` ray); a miss adds the sky;
/// later segments are cosine-sampled with throughput `*= albedo`. Output
/// `Rgba16Float`: rgb = cosine-weighted mean incident radiance (irradiance /
/// pi; multiply by the surface albedo for the diffuse indirect radiance),
/// a = 1 on geometry, 0 elsewhere. The direct sun on the G-buffer surface is
/// not included (the shadow-masked direct pass owns it).
///
/// Bindings: group 0 = the scene; group 1 = 0 world positions, 1 world normals
/// (`texture_2d<f32>`, unfilterable), 2 `texture_storage_2d<rgba16float,
/// write>`, 3 `array<RtMaterialRecord>`, 4 uniform (112 bytes: lighting,
/// `vec4(normal_bias, max_distance)`, `vec4<u32>(width, height, mask,
/// material_count)`, `vec4<u32>(frame, seed, rays, bounces)`).
pub struct DiffuseGiKernel {
    core: Arc<Core>,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl DiffuseGiKernel {
    pub fn new(rt: &RtDevice) -> Result<Self, RtError> {
        let core = rt.core().clone();
        core.ensure_usable()?;
        let device = &core.device;
        let layout = kernel::bind_group_layout(
            device,
            "engine-render-rt diffuse gi",
            &[
                kernel::texture_entry(0),
                kernel::texture_entry(1),
                kernel::storage_texture_entry(2, wgpu::TextureFormat::Rgba16Float),
                kernel::storage_buffer_entry(3, true),
                kernel::uniform_entry(4),
            ],
        );
        let pipeline = kernel::compute_pipeline(
            device,
            "engine-render-rt diffuse gi",
            &[&core.scene_layout, &layout],
            wgsl::diffuse_gi_source(),
            "diffuse_gi",
        );
        Ok(Self {
            core,
            pipeline,
            layout,
        })
    }

    /// Records one dispatch. The scene must be committed and the materials
    /// created on the same device.
    pub fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &RtScene,
        inputs: &GiInputs<'_>,
        settings: &RtGiSettings,
    ) -> Result<(), RtError> {
        let scene_group = kernel::scene_group(&self.core, scene)?;
        if !inputs.materials.is_from(&self.core) {
            return Err(invalid("the material table belongs to another RtDevice"));
        }
        let g = &inputs.gbuffer;
        kernel::check_size(&self.core, "diffuse gi", g.width, g.height)?;
        settings.validate()?;
        let uniform = GpuGiParams {
            lighting: inputs.lighting.to_gpu()?,
            ray: [settings.normal_bias, settings.max_distance, 0.0, 0.0],
            size: [g.width, g.height, 0xff, inputs.materials.material_count()],
            frame: [
                inputs.frame_index,
                GI_SEED,
                settings.rays_per_pixel,
                settings.bounces,
            ],
        };
        let device = &self.core.device;
        let buffer = kernel::uniform_buffer(
            device,
            "engine-render-rt diffuse gi params",
            bytemuck::bytes_of(&uniform),
        );
        let group = kernel::bind_group(
            device,
            "engine-render-rt diffuse gi",
            &self.layout,
            &[
                wgpu::BindingResource::TextureView(g.world_position),
                wgpu::BindingResource::TextureView(g.world_normal),
                wgpu::BindingResource::TextureView(inputs.output),
                inputs.materials.buffer().as_entire_binding(),
                buffer.as_entire_binding(),
            ],
        );
        kernel::dispatch(
            encoder,
            "engine-render-rt diffuse gi",
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
        // RtLightingRecord (64) + 3 x vec4 (48).
        assert_eq!(size_of::<GpuGiParams>(), 112);
    }
}
