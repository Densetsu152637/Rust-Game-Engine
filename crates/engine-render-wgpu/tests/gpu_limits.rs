//! GPU tests of the error model: a rejected resource fails its own call only.
//! They fail (not skip) without a wgpu adapter and are ignored in ordinary runs:
//! `cargo test -p engine-render-wgpu -- --ignored --nocapture`.
//!
//! Each bad input is exercised twice. Through `RenderDevice` the limit
//! pre-validation rejects it with `LimitExceeded` before the GPU sees it. Through
//! the raw backend (bypassing pre-validation, as a buggy or future caller could)
//! wgpu itself rejects it, and the error must come back from that call only
//! instead of landing in the sticky device log.

mod common;

use common::*;
use engine_render::*;

const UNTEXTURED: &str = "
@vertex fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(position, 1.0);
}
@fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0, 0.0, 0.0, 1.0); }
";

fn uniform_shader(group: u32, vec4s: u32) -> ShaderResource {
    let source = format!(
        "
struct Block {{ values: array<vec4<f32>, {vec4s}> }}
@group({group}) @binding(0) var<uniform> block: Block;
@vertex fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {{
    return vec4<f32>(position, 1.0) * block.values[0];
}}
@fragment fn fs_main() -> @location(0) vec4<f32> {{ return vec4<f32>(1.0); }}
"
    );
    ShaderResource::new("bad uniform", ShaderSource::wgsl(&source))
}

fn wide_mesh(stride: u32) -> MeshResource {
    let layout = VertexLayout::new(
        stride,
        [VertexAttribute {
            location: 0,
            format: VertexFormat::Float32x3,
            offset: 0,
        }],
    );
    MeshResource::new(vec![0; stride as usize * 3], layout, vec![]).unwrap()
}

/// Everything a healthy device must still do after a rejected resource: create a
/// shader, mesh and target, render a frame and read it back.
fn assert_device_still_renders(device: &mut Device, log: &GpuLog) {
    let target = device.create_target(16, 16).expect("target");
    let program = shader(device, TEXTURED);
    let mesh = device
        .create_mesh(&quad(0.0, 0.0, 16.0, 16.0, 0.5, [1.0, 0.0, 0.0, 1.0]))
        .expect("mesh");
    let camera = pixel_camera(16.0, 16.0);
    let image = render_and_read(
        device,
        target,
        BLACK,
        vec![draw(
            program,
            mesh,
            None,
            &camera,
            state(BlendMode::Opaque, true, true),
        )],
    );
    assert_eq!(pixel(&image, 8, 8), [255, 0, 0, 255]);
    assert_no_validation_errors(log);
    device.wait_idle().expect("device is idle and healthy");
}

fn camera_draw_of(shader: ShaderHandle, mesh: MeshHandle) -> DrawCommand {
    DrawCommand::new(shader, mesh)
}

fn offscreen(target: TargetHandle) -> FrameDesc {
    FrameDesc::new(RenderTarget::Offscreen(target))
}

fn is_limit(error: &RenderError) -> bool {
    matches!(error, RenderError::LimitExceeded { .. })
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn capabilities_report_the_real_device_limits() {
    let (device, log) = open(2);
    let limits = device.capabilities().limits;
    eprintln!("{limits:?}");
    assert!(limits.max_bind_groups >= 4);
    assert!(limits.max_vertex_buffer_array_stride >= 255);
    assert!(limits.max_vertex_attributes >= 16);
    assert!(limits.max_uniform_buffer_binding_size >= 16 << 10);
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn pre_validation_rejects_each_bad_input_and_the_device_keeps_working() {
    let (mut device, log) = open(2);
    let limits = device.capabilities().limits;

    let group = limits.max_bind_groups;
    let error = device
        .create_shader(uniform_shader(group, 1))
        .expect_err("bind group beyond the limit");
    assert!(is_limit(&error), "{error:?}");
    assert_device_still_renders(&mut device, &log);

    let stride = (limits.max_vertex_buffer_array_stride / 4 + 1) * 4;
    let error = device
        .create_mesh(&wide_mesh(stride))
        .expect_err("stride beyond the limit");
    assert!(is_limit(&error), "{error:?}");
    assert_device_still_renders(&mut device, &log);

    let vec4s = limits.max_uniform_buffer_binding_size / 16 + 1;
    let error = device
        .create_shader(uniform_shader(0, vec4s))
        .expect_err("uniform beyond the limit");
    assert!(is_limit(&error), "{error:?}");
    assert_device_still_renders(&mut device, &log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn a_group_beyond_the_device_limit_fails_only_its_own_call() {
    let (mut device, log) = open(2);
    // The reported cap would reject this first, so go to the backend directly.
    let result = device
        .backend_mut()
        .create_shader(ShaderHandle::from_raw(9_001), &uniform_shader(5, 1));
    let error = result.expect_err("wgpu rejects @group(5)");
    assert!(matches!(error, RenderError::Backend(_)), "{error:?}");
    eprintln!("{error}");
    assert_no_validation_errors(&log);
    assert_device_still_renders(&mut device, &log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn an_oversized_vertex_stride_fails_only_its_own_frame() {
    let (mut device, log) = open(2);
    let target = device.create_target(16, 16).unwrap();
    let backend = device.backend_mut();
    let (program, mesh) = (ShaderHandle::from_raw(9_010), MeshHandle::from_raw(9_011));
    backend
        .create_shader(
            program,
            &ShaderResource::new("plain", ShaderSource::wgsl(UNTEXTURED)),
        )
        .expect("a valid shader");
    // The buffer is fine; the pipeline for a 4096-byte stride is not.
    backend
        .create_mesh(mesh, &wide_mesh(4096))
        .expect("the buffer itself is valid");
    let draws = [camera_draw_of(program, mesh)];
    let error = backend
        .submit_frame(&offscreen(target), &draws)
        .expect_err("pipeline creation is rejected");
    assert!(matches!(error, RenderError::Backend(_)), "{error:?}");
    eprintln!("{error}");
    // The same bad draw keeps failing for its own reason; nothing is latched.
    assert!(backend.submit_frame(&offscreen(target), &draws).is_err());
    assert_no_validation_errors(&log);
    assert_device_still_renders(&mut device, &log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn an_oversized_uniform_fails_only_its_own_call() {
    let (mut device, log) = open(2);
    let target = device.create_target(16, 16).unwrap();
    let max = device.capabilities().limits.max_uniform_buffer_binding_size;
    let vec4s = max / 16 * 2;
    let bytes = u64::from(vec4s) * 16;
    let backend = device.backend_mut();
    let (program, mesh) = (ShaderHandle::from_raw(9_020), MeshHandle::from_raw(9_021));
    backend
        .create_mesh(mesh, &wide_mesh(12))
        .expect("a valid mesh");
    let created = backend.create_shader(program, &uniform_shader(0, vec4s));
    let error = match created {
        Err(error) => error,
        Ok(()) => {
            let draw = camera_draw_of(program, mesh).with_uniform(DrawUniform::from_bytes(
                "block",
                UniformSlot::new(0, 0),
                vec![0; bytes as usize],
            ));
            backend
                .submit_frame(&offscreen(target), &[draw])
                .expect_err("the oversized uniform is rejected")
        }
    };
    assert!(matches!(error, RenderError::Backend(_)), "{error:?}");
    eprintln!("{error}");
    assert_no_validation_errors(&log);
    assert_device_still_renders(&mut device, &log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn bad_calls_in_a_row_each_report_their_own_error() {
    let (mut device, log) = open(2);
    let first = device
        .backend_mut()
        .create_shader(ShaderHandle::from_raw(9_030), &uniform_shader(5, 1))
        .unwrap_err();
    let second = device
        .backend_mut()
        .create_shader(ShaderHandle::from_raw(9_031), &uniform_shader(6, 1))
        .unwrap_err();
    assert_ne!(first, second, "the second error must not repeat the first");
    // A valid call between failures succeeds, and the device lost nothing.
    shader(&mut device, TEXTURED);
    assert_no_validation_errors(&log);
    assert_device_still_renders(&mut device, &log);
}
