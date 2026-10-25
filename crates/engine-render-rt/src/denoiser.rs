//! Spatio-temporal (SVGF-style) denoiser for the RT signals.

use crate::device::{Core, RtDevice};
use crate::error::{RtError, invalid};
use crate::kernel::{self, RtGBuffer};
use crate::settings::DenoiserSettings;
use crate::wgsl::DenoisePass;
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;

/// Which channels of the input signal are filtered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DenoiserSignal {
    /// `.x` (AO, soft shadow visibility); output `(v, 0, 0, 1)`.
    Scalar,
    /// `.rgb` (diffuse GI); output `(rgb, 1)`.
    Rgb,
    /// `.rgb` with `.a` the reflection hit distance (RT reflections). Pixels
    /// whose `.a` is negative (the rough-pixel and no-geometry flags) are not
    /// filtered, contribute to no neighbour and pass through unchanged; the
    /// output keeps the input `.a`.
    RgbHitDistance,
}

impl DenoiserSignal {
    fn code(self) -> u32 {
        match self {
            Self::Scalar => 0,
            Self::Rgb => 1,
            Self::RgbHitDistance => 2,
        }
    }
}

/// Per-frame inputs. Every texture is the denoiser's `width x height`.
pub struct DenoiserInputs<'a> {
    pub gbuffer: RtGBuffer<'a>,
    /// The noisy signal (any float format readable by `textureLoad`, for
    /// example the `R16Float` AO or `Rgba16Float` GI/reflection output).
    pub signal: &'a wgpu::TextureView,
    /// `.xy` = current-to-previous motion in pixels (y down), the engine's
    /// `Rg16Float` motion convention: a surface at pixel `p` this frame was at
    /// `p + motion` in the previous frame.
    pub motion: &'a wgpu::TextureView,
    /// Camera position, for view distances.
    pub eye: [f32; 3],
    /// Discard all history this frame (camera cut or teleport).
    pub camera_cut: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuDenoiseParams {
    eye: [f32; 4],
    temporal: [f32; 4],
    edge: [f32; 4],
    size: [u32; 4],
    pass_info: [u32; 4],
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Target {
    fn new(
        device: &wgpu::Device,
        label: &str,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Self { texture, view }
    }
}

/// SVGF-style spatio-temporal denoiser (Schied et al. 2017) for one signal of
/// one view. Plain compute (no ray query), workgroups of 8 x 8.
///
/// Each [`Denoiser::record`] runs:
/// 1. **temporal**: bilinear reprojection of the previous history through the
///    motion vectors; each tap is rejected when its stored view distance
///    differs by more than `reprojection_depth_tolerance` (relative) or its
///    normal by more than `reprojection_normal_threshold` (cosine); the signal
///    and the first two luminance moments are blended with
///    `alpha = 1 / history length`, the length capped at `max_history_length`.
/// 2. **variance**: the moment variance `E[l^2] - E[l]^2`, replaced for
///    histories shorter than `min_history_for_temporal_variance` by an
///    edge-aware 7 x 7 spatial estimate.
/// 3. **a-trous** x `atrous_iterations`: 5 x 5 B3-spline wavelet taps at step
///    widths 1, 2, 4, ... px with depth (plane distance), normal and luminance
///    (`phi_luminance` x the local standard deviation) edge stops; the first
///    iteration's output becomes next frame's history.
/// 4. **resolve** into the `Rgba16Float` [`Denoiser::output`].
///
/// History (signal, moments + length, normal + view distance) lives in owned
/// ping-pong `Rgba32Float` textures; the first frame, [`Denoiser::reset`] and
/// `camera_cut` discard it.
pub struct Denoiser {
    core: Arc<Core>,
    signal: DenoiserSignal,
    settings: DenoiserSettings,
    width: u32,
    height: u32,
    temporal: wgpu::ComputePipeline,
    variance: wgpu::ComputePipeline,
    atrous: wgpu::ComputePipeline,
    resolve: wgpu::ComputePipeline,
    temporal_layout: wgpu::BindGroupLayout,
    filter_layout: wgpu::BindGroupLayout,
    resolve_layout: wgpu::BindGroupLayout,
    integrated: Target,
    ping: [Target; 2],
    history: [Target; 2],
    moments: [Target; 2],
    geometry: [Target; 2],
    output: Target,
    frames: u64,
    has_history: bool,
}

impl Denoiser {
    pub fn new(
        rt: &RtDevice,
        width: u32,
        height: u32,
        signal: DenoiserSignal,
        settings: &DenoiserSettings,
    ) -> Result<Self, RtError> {
        let core = rt.core().clone();
        core.ensure_usable()?;
        kernel::check_size(&core, "denoiser", width, height)?;
        settings.validate()?;
        let device = &core.device;
        let rgba32 = wgpu::TextureFormat::Rgba32Float;
        let base = [
            kernel::uniform_entry(0),
            kernel::texture_entry(1),
            kernel::texture_entry(2),
        ];
        let layout = |label: &str, extra: &[wgpu::BindGroupLayoutEntry]| {
            let entries: Vec<_> = base.iter().chain(extra).copied().collect();
            kernel::bind_group_layout(device, label, &entries)
        };
        let temporal_layout = layout(
            "engine-render-rt denoiser temporal",
            &[
                kernel::texture_entry(3),
                kernel::texture_entry(4),
                kernel::texture_entry(5),
                kernel::texture_entry(6),
                kernel::texture_entry(7),
                kernel::storage_texture_entry(8, rgba32),
                kernel::storage_texture_entry(9, rgba32),
                kernel::storage_texture_entry(10, rgba32),
            ],
        );
        let filter_layout = layout(
            "engine-render-rt denoiser filter",
            &[
                kernel::texture_entry(3),
                kernel::texture_entry(4),
                kernel::storage_texture_entry(5, rgba32),
            ],
        );
        let resolve_layout = layout(
            "engine-render-rt denoiser resolve",
            &[
                kernel::texture_entry(3),
                kernel::texture_entry(4),
                kernel::texture_entry(5),
                kernel::storage_texture_entry(6, wgpu::TextureFormat::Rgba16Float),
            ],
        );
        let pipeline = |pass: DenoisePass, layout: &wgpu::BindGroupLayout| {
            kernel::compute_pipeline(
                device,
                &format!("engine-render-rt denoiser {}", pass.entry_point()),
                &[layout],
                pass.source(),
                pass.entry_point(),
            )
        };
        let temporal = pipeline(DenoisePass::Temporal, &temporal_layout);
        let variance = pipeline(DenoisePass::Variance, &filter_layout);
        let atrous = pipeline(DenoisePass::Atrous, &filter_layout);
        let resolve = pipeline(DenoisePass::Resolve, &resolve_layout);
        let storage = wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING;
        let target =
            |label: &str, format, usage| Target::new(device, label, width, height, format, usage);
        let pair =
            |label: &str, usage| [target(label, rgba32, usage), target(label, rgba32, usage)];
        Ok(Self {
            integrated: target("engine-render-rt denoiser integrated", rgba32, storage),
            ping: pair(
                "engine-render-rt denoiser filter",
                storage | wgpu::TextureUsages::COPY_SRC,
            ),
            history: pair(
                "engine-render-rt denoiser history",
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            ),
            moments: pair("engine-render-rt denoiser moments", storage),
            geometry: pair("engine-render-rt denoiser geometry", storage),
            output: target(
                "engine-render-rt denoiser output",
                wgpu::TextureFormat::Rgba16Float,
                storage | wgpu::TextureUsages::COPY_SRC,
            ),
            core,
            signal,
            settings: *settings,
            width,
            height,
            temporal,
            variance,
            atrous,
            resolve,
            temporal_layout,
            filter_layout,
            resolve_layout,
            frames: 0,
            has_history: false,
        })
    }

    pub fn signal(&self) -> DenoiserSignal {
        self.signal
    }

    pub fn settings(&self) -> &DenoiserSettings {
        &self.settings
    }

    /// Validates and applies new settings; the history is kept.
    pub fn set_settings(&mut self, settings: &DenoiserSettings) -> Result<(), RtError> {
        settings.validate()?;
        self.settings = *settings;
        Ok(())
    }

    /// Discards the history: the next frame starts accumulation afresh.
    pub fn reset(&mut self) {
        self.has_history = false;
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Frames recorded since creation.
    pub fn frame_count(&self) -> u64 {
        self.frames
    }

    /// The denoised signal (`Rgba16Float`, `STORAGE_BINDING | TEXTURE_BINDING
    /// | COPY_SRC`), valid after a recorded frame; the same texture every frame.
    pub fn output(&self) -> &wgpu::Texture {
        &self.output.texture
    }

    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.output.view
    }

    fn params(&self, inputs: &DenoiserInputs<'_>, no_history: bool, step: u32) -> GpuDenoiseParams {
        let s = &self.settings;
        let e = inputs.eye;
        GpuDenoiseParams {
            eye: [e[0], e[1], e[2], 0.0],
            temporal: [
                s.reprojection_depth_tolerance,
                s.reprojection_normal_threshold,
                s.max_history_length as f32,
                0.0,
            ],
            edge: [s.phi_depth, s.phi_normal, s.phi_luminance, 0.0],
            size: [
                self.width,
                self.height,
                self.signal.code(),
                u32::from(no_history),
            ],
            pass_info: [step, s.min_history_for_temporal_variance, 0, 0],
        }
    }

    /// Records one denoised frame into `encoder`.
    pub fn record(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &DenoiserInputs<'_>,
    ) -> Result<(), RtError> {
        self.core.ensure_usable()?;
        let g = &inputs.gbuffer;
        if (g.width, g.height) != (self.width, self.height) {
            return Err(invalid(format!(
                "denoiser inputs are {}x{} but the denoiser is {}x{}",
                g.width, g.height, self.width, self.height
            )));
        }
        kernel::finite_point("eye", inputs.eye)?;
        let no_history = inputs.camera_cut || !self.has_history;
        let current = (self.frames % 2) as usize;
        let previous = 1 - current;
        let (w, h) = (self.width, self.height);
        let base = self.uniform(inputs, no_history, 1);
        let temporal_group = self.group(
            "engine-render-rt denoiser temporal",
            &self.temporal_layout,
            &base,
            g,
            &[
                inputs.signal,
                inputs.motion,
                &self.history[previous].view,
                &self.moments[previous].view,
                &self.geometry[previous].view,
                &self.integrated.view,
                &self.moments[current].view,
                &self.geometry[current].view,
            ],
        );
        kernel::dispatch(
            encoder,
            "engine-render-rt denoiser temporal",
            &self.temporal,
            &[&temporal_group],
            w,
            h,
        );
        let moments = &self.moments[current].view;
        let variance_group = self.group(
            "engine-render-rt denoiser variance",
            &self.filter_layout,
            &base,
            g,
            &[&self.integrated.view, moments, &self.ping[0].view],
        );
        kernel::dispatch(
            encoder,
            "engine-render-rt denoiser variance",
            &self.variance,
            &[&variance_group],
            w,
            h,
        );
        let iterations = self.settings.atrous_iterations as usize;
        for i in 0..iterations {
            let buffer = self.uniform(inputs, no_history, 1 << i);
            let group = self.group(
                "engine-render-rt denoiser a-trous",
                &self.filter_layout,
                &buffer,
                g,
                &[
                    &self.ping[i % 2].view,
                    moments,
                    &self.ping[(i + 1) % 2].view,
                ],
            );
            kernel::dispatch(
                encoder,
                "engine-render-rt denoiser a-trous",
                &self.atrous,
                &[&group],
                w,
                h,
            );
            if i == 0 {
                self.copy_to_history(encoder, 1, current);
            }
        }
        if iterations == 0 {
            self.copy_to_history(encoder, 0, current);
        }
        let resolve_group = self.group(
            "engine-render-rt denoiser resolve",
            &self.resolve_layout,
            &base,
            g,
            &[
                &self.ping[iterations % 2].view,
                moments,
                inputs.signal,
                &self.output.view,
            ],
        );
        kernel::dispatch(
            encoder,
            "engine-render-rt denoiser resolve",
            &self.resolve,
            &[&resolve_group],
            w,
            h,
        );
        self.frames += 1;
        self.has_history = true;
        Ok(())
    }

    fn uniform(&self, inputs: &DenoiserInputs<'_>, no_history: bool, step: u32) -> wgpu::Buffer {
        kernel::uniform_buffer(
            &self.core.device,
            "engine-render-rt denoiser params",
            bytemuck::bytes_of(&self.params(inputs, no_history, step)),
        )
    }

    /// A pass bind group: parameters, the G-buffer, then `views` at bindings 3...
    fn group(
        &self,
        label: &str,
        layout: &wgpu::BindGroupLayout,
        params: &wgpu::Buffer,
        gbuffer: &RtGBuffer<'_>,
        views: &[&wgpu::TextureView],
    ) -> wgpu::BindGroup {
        let resources: Vec<wgpu::BindingResource<'_>> = [
            params.as_entire_binding(),
            wgpu::BindingResource::TextureView(gbuffer.world_position),
            wgpu::BindingResource::TextureView(gbuffer.world_normal),
        ]
        .into_iter()
        .chain(views.iter().map(|v| wgpu::BindingResource::TextureView(v)))
        .collect();
        kernel::bind_group(&self.core.device, label, layout, &resources)
    }

    fn copy_to_history(&self, encoder: &mut wgpu::CommandEncoder, ping: usize, history: usize) {
        encoder.copy_texture_to_texture(
            self.ping[ping].texture.as_image_copy(),
            self.history[history].texture.as_image_copy(),
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_layout_matches_wgsl() {
        assert_eq!(size_of::<GpuDenoiseParams>(), 80);
        assert_eq!(DenoiserSignal::Scalar.code(), 0);
        assert_eq!(DenoiserSignal::Rgb.code(), 1);
        assert_eq!(DenoiserSignal::RgbHitDistance.code(), 2);
    }
}
