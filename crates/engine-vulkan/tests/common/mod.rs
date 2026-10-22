//! Helpers shared by the GPU tests (each test file includes this module).
#![allow(dead_code)]

use engine_render::*;
use engine_vulkan::{ValidationLog, VulkanBackend, VulkanConfig};

/// The textured sprite shader the desktop renderer uses (a verbatim copy of
/// `crates/engine-desktop/src/textured.wgsl`, kept here so this crate's tests do
/// not depend on another crate's source layout).
pub const TEXTURED: &str = r#"
struct Camera { view_projection: mat4x4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}
@vertex fn vs_main(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>,
                  @location(2) color: vec4<f32>) -> VertexOutput {
    var output: VertexOutput;
    output.position = camera.view_projection * vec4<f32>(position, 1.0);
    output.uv = uv;
    output.color = color;
    return output;
}
@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(image, image_sampler, input.uv) * input.color;
}
"#;

pub type Device = RenderDevice<VulkanBackend>;

/// Open an offscreen device with validation requested (active only when the
/// layer is installed).
pub fn open(frames_in_flight: u32) -> (Device, ValidationLog) {
    let backend = VulkanBackend::offscreen(VulkanConfig {
        application_name: "engine-vulkan-tests".into(),
        enable_validation: true,
        frames_in_flight,
    });
    let log = backend.validation_log();
    let device = RenderDevice::new(backend, DeviceConfig { frames_in_flight })
        .expect("a Vulkan device is required for the GPU tests");
    let stats = device.backend().stats().expect("initialized");
    eprintln!(
        "adapter: {} ({}), Vulkan {}.{}.{}, validation layer {}",
        stats.adapter,
        stats.device_type,
        stats.api_version.0,
        stats.api_version.1,
        stats.api_version.2,
        if log.is_active() {
            "ACTIVE"
        } else {
            "not installed/inactive"
        }
    );
    (device, log)
}

/// Fail if the validation layer recorded any error.
pub fn assert_no_validation_errors(log: &ValidationLog) {
    let errors = log.errors();
    assert!(
        errors.is_empty(),
        "validation errors:\n{}",
        errors.join("\n")
    );
}

pub fn textured_layout() -> VertexLayout {
    VertexLayout::new(
        36,
        [
            VertexAttribute {
                location: 0,
                format: VertexFormat::Float32x3,
                offset: 0,
            },
            VertexAttribute {
                location: 1,
                format: VertexFormat::Float32x2,
                offset: 12,
            },
            VertexAttribute {
                location: 2,
                format: VertexFormat::Float32x4,
                offset: 20,
            },
        ],
    )
}

fn push_vertex(bytes: &mut Vec<u8>, position: [f32; 3], uv: [f32; 2], color: [f32; 4]) {
    for value in position.into_iter().chain(uv).chain(color) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

/// Indexed quad with corners `(x0,y0)` top-left to `(x1,y1)` bottom-right and uv
/// `(0,0)` at the top-left corner to `(1,1)` at the bottom-right.
pub fn quad(x0: f32, y0: f32, x1: f32, y1: f32, z: f32, color: [f32; 4]) -> MeshResource {
    let mut bytes = Vec::new();
    push_vertex(&mut bytes, [x0, y0, z], [0.0, 0.0], color);
    push_vertex(&mut bytes, [x1, y0, z], [1.0, 0.0], color);
    push_vertex(&mut bytes, [x0, y1, z], [0.0, 1.0], color);
    push_vertex(&mut bytes, [x1, y1, z], [1.0, 1.0], color);
    MeshResource::new(bytes, textured_layout(), vec![0, 1, 2, 2, 1, 3]).unwrap()
}

/// Non-indexed triangle.
pub fn triangle(points: [[f32; 2]; 3], z: f32, color: [f32; 4]) -> MeshResource {
    let mut bytes = Vec::new();
    for point in points {
        push_vertex(&mut bytes, [point[0], point[1], z], [0.5, 0.5], color);
    }
    MeshResource::new(bytes, textured_layout(), vec![]).unwrap()
}

pub const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

/// Column-major matrix mapping pixel coordinates (top-left origin, y down) to
/// clip space (y up), passing z through as depth.
pub fn pixel_camera(width: f32, height: f32) -> [f32; 16] {
    [
        2.0 / width,
        0.0,
        0.0,
        0.0, //
        0.0,
        -2.0 / height,
        0.0,
        0.0, //
        0.0,
        0.0,
        1.0,
        0.0, //
        -1.0,
        1.0,
        0.0,
        1.0,
    ]
}

pub fn matrix_bytes(matrix: &[f32; 16]) -> Vec<u8> {
    matrix.iter().flat_map(|v| v.to_le_bytes()).collect()
}

pub fn draw(
    shader: ShaderHandle,
    mesh: MeshHandle,
    texture: Option<TextureHandle>,
    camera: &[f32; 16],
    state: DrawState,
) -> DrawCommand {
    let mut command = DrawCommand::new(shader, mesh)
        .with_state(state)
        .with_uniform(DrawUniform::from_bytes(
            "camera",
            UniformSlot::new(0, 0),
            matrix_bytes(camera),
        ));
    command.texture = texture;
    command
}

pub fn state(blend: BlendMode, depth_test: bool, depth_write: bool) -> DrawState {
    DrawState {
        blend,
        depth_test,
        depth_write,
        cull: CullMode::None,
        scissor: None,
    }
}

pub fn submit(
    device: &mut Device,
    desc: FrameDesc,
    draws: Vec<DrawCommand>,
) -> Result<FrameOutcome> {
    let mut buffer = device.make_frame();
    for draw in draws {
        buffer.record(draw)?;
    }
    device.submit(desc, buffer)
}

/// Render `draws` to `target` with a linear `clear` color and return its pixels.
pub fn render_and_read(
    device: &mut Device,
    target: TargetHandle,
    clear: [f32; 4],
    draws: Vec<DrawCommand>,
) -> ReadbackImage {
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = clear;
    let outcome = submit(device, desc, draws).expect("frame submits");
    assert!(matches!(outcome, FrameOutcome::Rendered(_)), "{outcome:?}");
    device.read_target(target).expect("readback")
}

pub const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

pub fn pixel(image: &ReadbackImage, x: u32, y: u32) -> [u8; 4] {
    image.pixel(x, y).expect("pixel in bounds")
}

pub fn srgb_to_linear(byte: u8) -> f32 {
    let c = f32::from(byte) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn shader(device: &mut Device, code: &str) -> ShaderHandle {
    device
        .create_shader(ShaderResource::new("test", ShaderSource::wgsl(code)))
        .expect("shader compiles")
}

pub fn solid_texture(rgba: [u8; 4]) -> Texture2D {
    Texture2D::new(1, 1, rgba.to_vec()).unwrap()
}
