//! The pre-HUD temporal upscaling stage of [`super::GpuRenderer`].
//!
//! With DLSS Super Resolution requested and a ready device, a frame is:
//! jittered HDR 3D color at the NGX render size, the engine depth/motion pass,
//! a split into the NGX depth/motion inputs, `Session::evaluate`, a composite of
//! the display-size output into the 3D viewport, then 2D sprites/UI at output
//! resolution over it. Any DLSS setup or evaluation failure renders the frame
//! natively and reports the reason; `Active` is reported only after a
//! successful evaluation.

use super::temporal::TemporalHistory;
use crate::dlss::UpscalerDevice;
use engine_project::rendering3d::{
    UpscalerQuality, UpscalerSettings, UpscalerState, UpscalerStatus, bounded_upscaler_reason,
};

/// Request key that a sticky failure applies to: retried only after the
/// quality or output size changes.
type FailureKey = (UpscalerQuality, [u32; 2]);

pub(crate) struct Upscaler {
    pub(super) device: UpscalerDevice,
    pub(super) history: TemporalHistory,
    status: UpscalerStatus,
    pub(super) evaluated: u64,
    pub(super) resets: u64,
    pub(super) failure: Option<(FailureKey, String)>,
    #[cfg(feature = "dlss")]
    pub(super) dlss: Option<dlss_stage::DlssStage>,
}

impl Upscaler {
    pub fn new(device: UpscalerDevice) -> Self {
        Self {
            device,
            history: TemporalHistory::default(),
            status: UpscalerStatus::default(),
            evaluated: 0,
            resets: 0,
            failure: None,
            #[cfg(feature = "dlss")]
            dlss: None,
        }
    }

    pub fn status(&self) -> &UpscalerStatus {
        &self.status
    }

    /// Replace the device capability; any DLSS state of the old device is released.
    pub fn set_device(&mut self, device: UpscalerDevice) {
        #[cfg(feature = "dlss")]
        {
            self.dlss = None;
        }
        self.device = device;
        self.failure = None;
        self.history.clear();
    }

    pub fn request_reset(&mut self) {
        self.history.request_reset();
    }

    /// A frame rendered at output resolution without a request.
    pub fn native(&mut self, requested: UpscalerSettings) {
        self.history.request_reset();
        self.status = UpscalerStatus {
            requested,
            state: UpscalerState::Native,
            evaluated_frames: self.evaluated,
            history_resets: self.resets,
            ..Default::default()
        };
    }

    /// A requested upscaler did not run; the frame rendered natively.
    pub fn fallback(&mut self, requested: UpscalerSettings, reason: &str) {
        self.history.request_reset();
        self.status = UpscalerStatus {
            requested,
            state: UpscalerState::Fallback,
            fallback_reason: Some(bounded_upscaler_reason(reason)),
            evaluated_frames: self.evaluated,
            history_resets: self.resets,
            ..Default::default()
        };
    }

    /// A frame whose DLSS evaluation succeeded.
    #[cfg_attr(not(feature = "dlss"), allow(dead_code))]
    pub fn active(
        &mut self,
        requested: UpscalerSettings,
        render_size: [u32; 2],
        output_size: [u32; 2],
        adapter: String,
    ) {
        self.status = UpscalerStatus {
            requested,
            state: UpscalerState::Active,
            fallback_reason: None,
            render_size: Some(render_size),
            output_size: Some(output_size),
            evaluated_frames: self.evaluated,
            history_resets: self.resets,
            adapter: Some(adapter),
        };
    }

    /// The reason a DLSS request cannot run before any GPU work, if known:
    /// device capability first, then a sticky failure for this exact request.
    pub fn precheck(&self, requested: UpscalerSettings, output: [u32; 2]) -> Option<String> {
        if let Some(reason) = self.device.unavailable_reason() {
            return Some(reason);
        }
        self.failure
            .as_ref()
            .filter(|(key, _)| *key == (requested.quality, output))
            .map(|(_, reason)| reason.clone())
    }

    #[cfg_attr(not(feature = "dlss"), allow(dead_code))]
    pub fn remember_failure(
        &mut self,
        requested: UpscalerSettings,
        output: [u32; 2],
        reason: &str,
    ) {
        self.failure = Some(((requested.quality, output), reason.to_owned()));
    }
}

/// Native split/composite passes and the NGX session (SDK builds only).
#[cfg(feature = "dlss")]
pub(super) mod dlss_stage {
    use crate::dlss::DlssPaths;
    use engine_project::rendering3d::UpscalerQuality;
    use std::collections::HashMap;

    const SPLIT_WGSL: &str = r"
@group(0) @binding(0) var source: texture_2d<f32>;
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}
struct Split { @location(0) depth: vec4<f32>, @location(1) motion: vec4<f32> }
@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> Split {
    let value = textureLoad(source, vec2<i32>(position.xy), 0);
    return Split(vec4<f32>(value.z, 0.0, 0.0, 1.0), vec4<f32>(value.xy, 0.0, 1.0));
}
";
    const COMPOSITE_WGSL: &str = r"
@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var<uniform> origin: vec4<f32>;
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}
// Linear HDR to the target: values above the target range clip, exactly like
// the native 3D path (which has no tone mapper either).
@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let color = textureLoad(scene, vec2<i32>(position.xy - origin.xy), 0).rgb;
    return vec4<f32>(max(color, vec3<f32>(0.0)), 1.0);
}
";

    pub(crate) fn quality(quality: UpscalerQuality) -> engine_dlss::Quality {
        match quality {
            UpscalerQuality::UltraPerformance => engine_dlss::Quality::UltraPerformance,
            UpscalerQuality::Performance => engine_dlss::Quality::Performance,
            UpscalerQuality::Balanced => engine_dlss::Quality::Balanced,
            UpscalerQuality::Quality => engine_dlss::Quality::Quality,
            UpscalerQuality::Dlaa => engine_dlss::Quality::Dlaa,
        }
    }

    /// Engine-owned GPU objects of the DLSS path plus the NGX session.
    pub(crate) struct DlssStage {
        paths: DlssPaths,
        context: Option<(engine_dlss::Context, engine_dlss::SessionOptions)>,
        pub session: Option<((UpscalerQuality, [u32; 2]), engine_dlss::Session)>,
        motion: Option<([u32; 2], wgpu::TextureView)>,
        split_layout: wgpu::BindGroupLayout,
        split: wgpu::RenderPipeline,
        composite_layout: wgpu::BindGroupLayout,
        composite_module: wgpu::ShaderModule,
        composite: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
        origin: wgpu::Buffer,
        pub last_evaluation: Option<std::time::Instant>,
    }

    fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }
    }

    fn pipeline(
        device: &wgpu::Device,
        label: &str,
        layout: &wgpu::BindGroupLayout,
        module: &wgpu::ShaderModule,
        targets: &[Option<wgpu::ColorTargetState>],
    ) -> wgpu::RenderPipeline {
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets,
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn color_target(format: wgpu::TextureFormat) -> Option<wgpu::ColorTargetState> {
        Some(wgpu::ColorTargetState {
            format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })
    }

    impl DlssStage {
        pub fn new(device: &wgpu::Device, paths: DlssPaths) -> Self {
            let split_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("DLSS input split"),
                entries: &[texture_entry(0)],
            });
            let split_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("DLSS input split"),
                source: wgpu::ShaderSource::Wgsl(SPLIT_WGSL.into()),
            });
            let split = pipeline(
                device,
                "DLSS input split",
                &split_layout,
                &split_module,
                &[
                    color_target(wgpu::TextureFormat::R32Float),
                    color_target(wgpu::TextureFormat::Rg16Float),
                ],
            );
            let composite_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("DLSS composite"),
                    entries: &[
                        texture_entry(0),
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: wgpu::BufferSize::new(16),
                            },
                            count: None,
                        },
                    ],
                });
            let composite_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("DLSS composite"),
                source: wgpu::ShaderSource::Wgsl(COMPOSITE_WGSL.into()),
            });
            let origin = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("DLSS composite origin"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            Self {
                paths,
                context: None,
                session: None,
                motion: None,
                split_layout,
                split,
                composite_layout,
                composite_module,
                composite: HashMap::new(),
                origin,
                last_evaluation: None,
            }
        }

        /// The NGX session for this request, creating the shared context on
        /// first use and replacing the SR feature when quality or output size
        /// change. `Ok(true)` means a new session (temporal history restarts).
        #[allow(unreachable_code, unused_variables)] // uninhabited without the SDK
        pub fn ensure_session(
            &mut self,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            requested: UpscalerQuality,
            output: [u32; 2],
        ) -> Result<bool, String> {
            let key = (requested, output);
            if self.session.as_ref().is_some_and(|(k, _)| *k == key) {
                return Ok(false);
            }
            // Release the previous SR feature before creating its replacement.
            self.session = None;
            let context = self.ensure_context(device, queue, output)?.0;
            let session =
                engine_dlss::Session::with_context(context, output, quality(requested))
                    .map_err(|e| format!("DLSS Super Resolution feature creation failed: {e}"))?;
            self.session = Some((key, session));
            self.last_evaluation = None;
            Ok(true)
        }

        /// The shared NGX context (created on first use, for SR and frame
        /// generation) and the session options it was created with.
        #[allow(unreachable_code, unused_variables)] // uninhabited without the SDK
        pub fn ensure_context(
            &mut self,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            output: [u32; 2],
        ) -> Result<(engine_dlss::Context, engine_dlss::SessionOptions), String> {
            if let Some(context) = &self.context {
                return Ok(context.clone());
            }
            let options = engine_dlss::SessionOptions {
                project_id: self.paths.project_id.clone(),
                runtime_dir: self.paths.runtime_dir.clone(),
                log_dir: self.paths.log_dir.clone(),
                output_size: output,
                quality: engine_dlss::Quality::Quality,
            };
            let context = engine_dlss::Context::new(device, queue, &options)
                .map_err(|e| format!("DLSS initialization failed: {e}"))?;
            self.context = Some((context.clone(), options.clone()));
            Ok((context, options))
        }

        /// The `Rgba32Float` motion/depth intermediate at the render size.
        pub fn motion_view(&mut self, device: &wgpu::Device, size: [u32; 2]) -> wgpu::TextureView {
            if let Some((existing, view)) = &self.motion
                && *existing == size
            {
                return view.clone();
            }
            let view = device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("DLSS motion and depth"),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba32Float,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default());
            self.motion = Some((size, view.clone()));
            view
        }

        /// Write NGX depth (`R32Float`) and motion (`Rg16Float`) from the intermediate.
        pub fn encode_split(
            &self,
            device: &wgpu::Device,
            encoder: &mut wgpu::CommandEncoder,
            source: &wgpu::TextureView,
            depth: &wgpu::TextureView,
            motion: &wgpu::TextureView,
        ) {
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("DLSS input split"),
                layout: &self.split_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                }],
            });
            let attachment = |view| {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("DLSS input split"),
                color_attachments: &[attachment(depth), attachment(motion)],
                ..Default::default()
            });
            pass.set_pipeline(&self.split);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }

        /// Clear the whole target, then draw the upscaled scene into `viewport`.
        #[allow(clippy::too_many_arguments)]
        pub fn encode_composite(
            &mut self,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            encoder: &mut wgpu::CommandEncoder,
            scene: &wgpu::TextureView,
            target: &wgpu::TextureView,
            format: wgpu::TextureFormat,
            viewport: [u32; 4],
        ) {
            let pipeline = self
                .composite
                .entry(format)
                .or_insert_with(|| {
                    pipeline(
                        device,
                        "DLSS composite",
                        &self.composite_layout,
                        &self.composite_module,
                        &[color_target(format)],
                    )
                })
                .clone();
            queue.write_buffer(
                &self.origin,
                0,
                bytemuck::cast_slice(&[viewport[0] as f32, viewport[1] as f32, 0.0, 0.0]),
            );
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("DLSS composite"),
                layout: &self.composite_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(scene),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.origin.as_entire_binding(),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("DLSS composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_viewport(
                viewport[0] as f32,
                viewport[1] as f32,
                viewport[2] as f32,
                viewport[3] as f32,
                0.0,
                1.0,
            );
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dlss::DeviceSupport;
    use engine_project::rendering3d::UpscalerMode;

    fn dlss(quality: UpscalerQuality) -> UpscalerSettings {
        UpscalerSettings {
            mode: UpscalerMode::DlssSr,
            quality,
        }
    }

    #[test]
    fn unsupported_requests_fall_back_with_the_device_reason() {
        let mut upscaler = Upscaler::new(UpscalerDevice(DeviceSupport::Unavailable(
            "DLSS requires an NVIDIA Vulkan adapter".into(),
        )));
        assert_eq!(upscaler.status().state, UpscalerState::NotRendered);
        let request = dlss(UpscalerQuality::Quality);
        let reason = upscaler.precheck(request, [640, 360]).unwrap();
        upscaler.fallback(request, &reason);
        let status = upscaler.status();
        assert_eq!(status.state, UpscalerState::Fallback);
        assert_eq!(status.requested, request);
        assert_eq!(
            status.fallback_reason.as_deref(),
            Some("DLSS requires an NVIDIA Vulkan adapter")
        );
        assert_eq!(status.evaluated_frames, 0);
        assert!(status.render_size.is_none(), "no render size without DLSS");
    }

    #[test]
    fn sticky_failures_apply_only_to_the_failed_request() {
        let mut upscaler = Upscaler::new(UpscalerDevice::default());
        // Without a prepared device every request has the device reason.
        assert!(
            upscaler
                .precheck(dlss(UpscalerQuality::Dlaa), [8, 8])
                .is_some()
        );
        #[cfg(feature = "dlss")]
        {
            upscaler.device = UpscalerDevice(DeviceSupport::Ready(crate::dlss::DlssPaths {
                project_id: "p".into(),
                runtime_dir: ".".into(),
                log_dir: ".".into(),
                frame_generation: None,
            }));
            assert!(
                upscaler
                    .precheck(dlss(UpscalerQuality::Dlaa), [8, 8])
                    .is_none()
            );
            upscaler.remember_failure(dlss(UpscalerQuality::Dlaa), [8, 8], "evaluation failed");
            assert_eq!(
                upscaler
                    .precheck(dlss(UpscalerQuality::Dlaa), [8, 8])
                    .as_deref(),
                Some("evaluation failed")
            );
            assert!(
                upscaler
                    .precheck(dlss(UpscalerQuality::Quality), [8, 8])
                    .is_none()
            );
            assert!(
                upscaler
                    .precheck(dlss(UpscalerQuality::Dlaa), [16, 8])
                    .is_none()
            );
        }
        upscaler.native(UpscalerSettings::default());
        assert_eq!(upscaler.status().state, UpscalerState::Native);
        assert!(upscaler.status().fallback_reason.is_none());
    }

    #[test]
    fn fallback_reasons_are_bounded() {
        let mut upscaler = Upscaler::new(UpscalerDevice::default());
        upscaler.fallback(dlss(UpscalerQuality::Quality), &"é".repeat(1000));
        let reason = upscaler.status().fallback_reason.clone().unwrap();
        assert!(reason.len() <= engine_project::rendering3d::MAX_UPSCALER_REASON_BYTES);
        assert!(reason.chars().all(|c| c == 'é'));
    }

    #[test]
    fn active_is_reported_only_with_render_and_output_sizes() {
        let mut upscaler = Upscaler::new(UpscalerDevice::default());
        upscaler.evaluated = 3;
        upscaler.active(
            dlss(UpscalerQuality::Quality),
            [427, 240],
            [640, 360],
            "GPU".into(),
        );
        let status = upscaler.status();
        assert_eq!(status.state, UpscalerState::Active);
        assert_eq!(status.render_size, Some([427, 240]));
        assert_eq!(status.output_size, Some([640, 360]));
        assert_eq!(status.evaluated_frames, 3);
    }
}
