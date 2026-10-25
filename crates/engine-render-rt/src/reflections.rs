//! RT reflections: GGX importance-sampled reflection rays with a roughness
//! cutoff to the raster fallback.

use crate::device::{Core, RtDevice};
use crate::error::RtError;
use crate::kernel::{self, GpuLighting, RtGBuffer, RtLighting};
use crate::material::RtMaterialTable;
use crate::scene::RtScene;
use crate::settings::RtReflectionSettings;
use crate::wgsl;
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;

/// Output alpha of a reflection ray that hit nothing (rgb = sky radiance);
/// the largest finite `f16`.
pub const REFLECTION_MISS_DISTANCE: f32 = 65504.0;
/// Output alpha of a pixel rougher than `roughness_cutoff` (rgb 0): not
/// traced, use the raster fallback.
pub const REFLECTION_ROUGH_FLAG: f32 = -1.0;
/// Output alpha of a pixel without geometry (rgb 0).
pub const REFLECTION_NO_GEOMETRY_FLAG: f32 = -2.0;

/// Textures, materials, lighting and camera of one reflection dispatch.
pub struct ReflectionInputs<'a> {
    pub gbuffer: RtGBuffer<'a>,
    /// Perceptual roughness in `.x`, `[0, 1]` (any float or unorm format,
    /// `width x height`).
    pub roughness: &'a wgpu::TextureView,
    /// `Rgba16Float` storage view, `width x height`.
    pub output: &'a wgpu::TextureView,
    pub materials: &'a RtMaterialTable,
    pub lighting: &'a RtLighting,
    /// Camera position (the view direction is `normalize(eye - position)`).
    pub eye: [f32; 3],
    /// Decorrelates the GGX samples between frames.
    pub frame_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuReflectionParams {
    lighting: GpuLighting,
    eye: [f32; 4],
    ray: [f32; 4],
    size: [u32; 4],
    frame: [u32; 4],
}

const REFLECTION_SEED: u32 = 0x4ef1_ec75;

/// Compute kernel `reflections` (`src/shaders/reflections.wgsl`), workgroup
/// 8 x 8, one ray per pixel.
///
/// Pixels with roughness `<= roughness_cutoff` trace one GGX VNDF-sampled
/// reflection ray (a mirror ray below GGX alpha 1e-3). The closest hit is
/// shaded with its material's emission plus its albedo lit by the sun (one
/// `rt_occluded` visibility ray); a miss returns the sky. Output `Rgba16Float`:
/// rgb = radiance arriving along the sampled direction (apply the specular
/// BRDF when compositing), a = hit distance, [`REFLECTION_MISS_DISTANCE`] on a
/// miss, [`REFLECTION_ROUGH_FLAG`] for rough pixels and
/// [`REFLECTION_NO_GEOMETRY_FLAG`] for background pixels.
///
/// Bindings: group 0 = the scene; group 1 = 0 world positions, 1 world
/// normals, 2 roughness (`texture_2d<f32>`, unfilterable), 3
/// `texture_storage_2d<rgba16float, write>`, 4 `array<RtMaterialRecord>`,
/// 5 uniform (128 bytes: lighting, `vec4(eye, cutoff)`, `vec4(normal_bias,
/// max_distance)`, `vec4<u32>(width, height, mask, material_count)`,
/// `vec4<u32>(frame, seed)`).
pub struct ReflectionKernel {
    core: Arc<Core>,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl ReflectionKernel {
    pub fn new(rt: &RtDevice) -> Result<Self, RtError> {
        let core = rt.core().clone();
        core.ensure_usable()?;
        let device = &core.device;
        let layout = kernel::bind_group_layout(
            device,
            "engine-render-rt reflections",
            &[
                kernel::texture_entry(0),
                kernel::texture_entry(1),
                kernel::texture_entry(2),
                kernel::storage_texture_entry(3, wgpu::TextureFormat::Rgba16Float),
                kernel::storage_buffer_entry(4, true),
                kernel::uniform_entry(5),
            ],
        );
        let pipeline = kernel::compute_pipeline(
            device,
            "engine-render-rt reflections",
            &[&core.scene_layout, &layout],
            wgsl::reflections_source(),
            "reflections",
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
        inputs: &ReflectionInputs<'_>,
        settings: &RtReflectionSettings,
    ) -> Result<(), RtError> {
        let scene_group = kernel::scene_group(&self.core, scene)?;
        if !inputs.materials.is_from(&self.core) {
            return Err(crate::error::invalid(
                "the material table belongs to another RtDevice",
            ));
        }
        let g = &inputs.gbuffer;
        kernel::check_size(&self.core, "reflections", g.width, g.height)?;
        settings.validate()?;
        kernel::finite_point("eye", inputs.eye)?;
        let e = inputs.eye;
        let uniform = GpuReflectionParams {
            lighting: inputs.lighting.to_gpu()?,
            eye: [e[0], e[1], e[2], settings.roughness_cutoff],
            ray: [settings.normal_bias, settings.max_distance, 0.0, 0.0],
            size: [g.width, g.height, 0xff, inputs.materials.material_count()],
            frame: [inputs.frame_index, REFLECTION_SEED, 0, 0],
        };
        let device = &self.core.device;
        let buffer = kernel::uniform_buffer(
            device,
            "engine-render-rt reflection params",
            bytemuck::bytes_of(&uniform),
        );
        let group = kernel::bind_group(
            device,
            "engine-render-rt reflections",
            &self.layout,
            &[
                wgpu::BindingResource::TextureView(g.world_position),
                wgpu::BindingResource::TextureView(g.world_normal),
                wgpu::BindingResource::TextureView(inputs.roughness),
                wgpu::BindingResource::TextureView(inputs.output),
                inputs.materials.buffer().as_entire_binding(),
                buffer.as_entire_binding(),
            ],
        );
        kernel::dispatch(
            encoder,
            "engine-render-rt reflections",
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
        // RtLightingRecord (64) + 4 x vec4 (64).
        assert_eq!(size_of::<GpuReflectionParams>(), 128);
        // The miss sentinel survives the f16 round trip exactly.
        assert_eq!(REFLECTION_MISS_DISTANCE, 65504.0);
    }
}
