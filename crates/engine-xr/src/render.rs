//! Rendering adapter: the renderer sees per-eye `wgpu` targets plus view data
//! and nothing of OpenXR. [`TestPattern`] is a self-contained renderer for
//! bring-up and the headset test; [`MirrorBlit`] copies an eye to a desktop
//! texture on the same device.
use crate::view::XrView;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Eye {
    Left,
    Right,
}
impl Eye {
    pub const BOTH: [Self; 2] = [Self::Left, Self::Right];
    pub fn index(self) -> usize {
        self as usize
    }
}

/// One eye's colour target for this frame. The texture is an OpenXR swapchain
/// image (single layer, single mip) in `RENDER_ATTACHMENT | TEXTURE_BINDING |
/// COPY_SRC` usage; renderers create their own depth buffers.
pub struct XrRenderTarget<'a> {
    pub view: &'a XrView,
    pub texture: &'a wgpu::Texture,
    pub color: &'a wgpu::TextureView,
    pub format: wgpu::TextureFormat,
}

/// Everything a renderer receives for one XR frame.
pub struct XrFrameContext<'a> {
    /// The device created through `XR_KHR_vulkan_enable2`; every resource used
    /// with the targets must come from it.
    pub device: &'a wgpu::Device,
    /// The queue the XR runtime was given; submit all eye work here before
    /// returning.
    pub queue: &'a wgpu::Queue,
    /// Predicted display time of the frame (OpenXR `XrTime`, nanoseconds).
    pub predicted_display_time_ns: i64,
    /// Left then right eye.
    pub targets: &'a [XrRenderTarget<'a>],
}

/// Implemented by renderers that draw the scene into XR eye targets. Called
/// once per rendered XR frame between swapchain acquire/wait and release.
pub trait XrViewRenderer {
    /// Record and submit all work for `frame.targets` on `frame.queue`.
    fn render_views(&mut self, frame: &XrFrameContext<'_>) -> Result<(), String>;
}

/// Vulkan formats the engine accepts for XR swapchains, in preference order
/// (sRGB first so shaders write linear colour).
const SWAPCHAIN_FORMATS: [(u32, wgpu::TextureFormat); 4] = [
    (43, wgpu::TextureFormat::Rgba8UnormSrgb), // VK_FORMAT_R8G8B8A8_SRGB
    (50, wgpu::TextureFormat::Bgra8UnormSrgb), // VK_FORMAT_B8G8R8A8_SRGB
    (37, wgpu::TextureFormat::Rgba8Unorm),     // VK_FORMAT_R8G8B8A8_UNORM
    (44, wgpu::TextureFormat::Bgra8Unorm),     // VK_FORMAT_B8G8R8A8_UNORM
];
/// Pick the preferred swapchain format among the runtime's (Vulkan) formats.
pub fn choose_swapchain_format(runtime_formats: &[u32]) -> Option<(u32, wgpu::TextureFormat)> {
    SWAPCHAIN_FORMATS
        .into_iter()
        .find(|(vk, _)| runtime_formats.contains(vk))
}

fn fullscreen_vertex() -> &'static str {
    "struct Out { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> };
     @vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
         let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
         var out: Out;
         out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
         out.uv = uv;
         return out;
     }"
}

/// Checkerboard of 64-pixel cells: dark grey and an eye tint (left orange,
/// right cyan), so each eye is identifiable in the headset and in readback.
pub struct TestPattern {
    module: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    tints: [wgpu::BindGroup; 2],
    frames: u64,
}
impl TestPattern {
    /// Cell size in pixels.
    pub const CELL: u32 = 64;
    /// Linear RGB of the dark cells.
    pub const DARK: [f32; 3] = [0.05, 0.05, 0.05];
    /// Linear RGB tints of the left and right eye cells.
    pub const TINTS: [[f32; 3]; 2] = [[1.0, 0.5, 0.0], [0.0, 0.8, 1.0]];

    pub fn new(device: &wgpu::Device) -> Self {
        let source = format!(
            "{}
             @group(0) @binding(0) var<uniform> tint: vec4<f32>;
             @fragment fn fs(in: Out) -> @location(0) vec4<f32> {{
                 let cell = (u32(in.position.x) / {cell}u + u32(in.position.y) / {cell}u) & 1u;
                 let dark = vec4<f32>({d0}, {d1}, {d2}, 1.0);
                 return select(dark, tint, cell == 1u);
             }}",
            fullscreen_vertex(),
            cell = Self::CELL,
            d0 = Self::DARK[0],
            d1 = Self::DARK[1],
            d2 = Self::DARK[2],
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("xr test pattern"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("xr test pattern tint"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            }],
        });
        let tints = Self::TINTS.map(|tint| {
            use wgpu::util::DeviceExt;
            let contents: Vec<u8> = tint
                .into_iter()
                .chain([1.0_f32])
                .flat_map(f32::to_le_bytes)
                .collect();
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("xr test pattern tint"),
                contents: &contents,
                usage: wgpu::BufferUsages::UNIFORM,
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("xr test pattern tint"),
                layout: &layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            })
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("xr test pattern"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        Self {
            module,
            pipeline_layout,
            pipelines: HashMap::new(),
            tints,
            frames: 0,
        }
    }
    /// Frames rendered so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }
    fn pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> &wgpu::RenderPipeline {
        let (module, layout) = (&self.module, &self.pipeline_layout);
        self.pipelines.entry(format).or_insert_with(|| {
            fullscreen_pipeline(device, module, layout, format, "xr test pattern")
        })
    }
    /// Draw the pattern for `eye` into `target`.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        eye: Eye,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        self.pipeline(device, format);
        let pipeline = &self.pipelines[&format];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("xr test pattern"),
            color_attachments: &[Some(color_attachment(
                target,
                wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            ))],
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.tints[eye.index()], &[]);
        pass.draw(0..3, 0..1);
    }
}
impl XrViewRenderer for TestPattern {
    fn render_views(&mut self, frame: &XrFrameContext<'_>) -> Result<(), String> {
        let mut encoder = frame
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("xr test pattern"),
            });
        for target in frame.targets {
            self.encode(
                frame.device,
                &mut encoder,
                target.view.eye,
                target.color,
                target.format,
            );
        }
        frame.queue.submit([encoder.finish()]);
        self.frames += 1;
        Ok(())
    }
}

fn color_attachment(
    view: &wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load,
            store: wgpu::StoreOp::Store,
        },
    }
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Scales one eye texture into a desktop texture (bilinear, full target).
/// Source and target must belong to the same device; colour is sampled and
/// written through each view's format, so sRGB/linear mixes are handled.
pub struct MirrorBlit {
    module: wgpu::ShaderModule,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
}
impl MirrorBlit {
    pub fn new(device: &wgpu::Device) -> Self {
        let source = format!(
            "{}
             @group(0) @binding(0) var source: texture_2d<f32>;
             @group(0) @binding(1) var source_sampler: sampler;
             @fragment fn fs(in: Out) -> @location(0) vec4<f32> {{
                 return textureSample(source, source_sampler, in.uv);
             }}",
            fullscreen_vertex()
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("xr mirror"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("xr mirror"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("xr mirror"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("xr mirror"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            module,
            layout,
            pipeline_layout,
            sampler,
            pipelines: HashMap::new(),
        }
    }
    /// Record a blit of `source` (a 2D view) over all of `target`.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        target_format: wgpu::TextureFormat,
    ) {
        let (module, layout) = (&self.module, &self.pipeline_layout);
        let pipeline = self.pipelines.entry(target_format).or_insert_with(|| {
            fullscreen_pipeline(device, module, layout, target_format, "xr mirror")
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("xr mirror"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("xr mirror"),
            color_attachments: &[Some(color_attachment(
                target,
                wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            ))],
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Leave each eye image in colour-attachment layout before release, as
/// `XR_KHR_vulkan_enable2` requires, whatever the renderer or mirror did last.
pub(crate) fn finish_eye_layouts(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    views: &[&wgpu::TextureView],
) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("xr release layout"),
    });
    for view in views {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("xr release layout"),
            color_attachments: &[Some(color_attachment(view, wgpu::LoadOp::Load))],
            ..Default::default()
        });
    }
    queue.submit([encoder.finish()]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swapchain_format_prefers_srgb_rgba() {
        assert_eq!(
            choose_swapchain_format(&[37, 50, 43]),
            Some((43, wgpu::TextureFormat::Rgba8UnormSrgb))
        );
        assert_eq!(
            choose_swapchain_format(&[44, 50]),
            Some((50, wgpu::TextureFormat::Bgra8UnormSrgb))
        );
        assert_eq!(
            choose_swapchain_format(&[44]),
            Some((44, wgpu::TextureFormat::Bgra8Unorm))
        );
        assert_eq!(choose_swapchain_format(&[97, 109]), None);
    }
}
