//! Device-limit pre-validation of `RenderDevice` over the recording backend.

use engine_render::headless::{BackendEvent, RecordingBackend, RecordingConfig, RecordingLog};
use engine_render::*;

const VALID: &str = "
@vertex fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(position, 1.0);
}
@fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }
";

fn device_with(limits: DeviceLimits) -> (RenderDevice<RecordingBackend>, RecordingLog) {
    let backend = RecordingBackend::with_config(RecordingConfig {
        limits,
        ..Default::default()
    });
    let log = backend.log();
    (
        RenderDevice::new(backend, DeviceConfig::default()).unwrap(),
        log,
    )
}

fn limit_name(error: &RenderError) -> &'static str {
    match error {
        RenderError::LimitExceeded { limit, .. } => limit,
        other => panic!("expected LimitExceeded, got {other:?}"),
    }
}

/// A shader whose uniform block is `vec4s` vec4s (16 bytes each) at `@group(group)`.
fn shader_with(group: u32, binding: u32, vec4s: u32) -> ShaderResource {
    let source = format!(
        "
struct Block {{ values: array<vec4<f32>, {vec4s}> }}
@group({group}) @binding({binding}) var<uniform> block: Block;
@vertex fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {{
    return vec4<f32>(position, 1.0) * block.values[0];
}}
@fragment fn fs_main() -> @location(0) vec4<f32> {{ return vec4<f32>(1.0); }}
"
    );
    ShaderResource::new("limits", ShaderSource::wgsl(&source))
}

fn wide_layout(stride: u32, attributes: u32) -> VertexLayout {
    VertexLayout::new(
        stride,
        (0..attributes).map(|location| VertexAttribute {
            location,
            format: VertexFormat::Float32,
            offset: location * 4,
        }),
    )
}

fn mesh(stride: u32, attributes: u32) -> MeshResource {
    MeshResource::new(
        vec![0; stride as usize * 3],
        wide_layout(stride, attributes),
        vec![],
    )
    .unwrap()
}

fn created_shader(log: &RecordingLog) -> bool {
    log.events()
        .iter()
        .any(|event| matches!(event, BackendEvent::CreateShader { .. }))
}

#[test]
fn recording_backend_reports_the_downlevel_default_limits() {
    let (device, _log) = device_with(DeviceLimits::default());
    let limits = device.capabilities().limits;
    assert_eq!(limits.max_bind_groups, 4);
    assert_eq!(limits.max_bindings_per_bind_group, 1000);
    assert_eq!(limits.max_vertex_buffer_array_stride, 255);
    assert_eq!(limits.max_vertex_attributes, 16);
    assert_eq!(limits.max_uniform_buffer_binding_size, 16 << 10);
    let plain = RecordingBackend::new().capabilities();
    assert_eq!(plain.limits, limits);
}

#[test]
fn shader_bind_group_index_beyond_the_limit_is_rejected_before_the_backend() {
    let (mut device, log) = device_with(DeviceLimits::default());
    let max = device.capabilities().limits.max_bind_groups;
    let error = device.create_shader(shader_with(max, 0, 1)).unwrap_err();
    assert!(limit_name(&error).starts_with("max_bind_groups"), "{error}");
    assert!(matches!(
        error,
        RenderError::LimitExceeded {
            requested, max: m, ..
        } if requested == u64::from(max) + 1 && m == u64::from(max)
    ));
    assert_eq!(device.counts().shaders, 0);
    assert!(!created_shader(&log), "the backend must not see the shader");
    device.create_shader(shader_with(max - 1, 0, 1)).unwrap();
}

#[test]
fn shader_binding_number_beyond_the_limit_is_rejected() {
    let (mut device, log) = device_with(DeviceLimits::default());
    let max = device.capabilities().limits.max_bindings_per_bind_group;
    let error = device.create_shader(shader_with(0, max, 1)).unwrap_err();
    assert!(
        limit_name(&error).starts_with("max_bindings_per_bind_group"),
        "{error}"
    );
    assert!(!created_shader(&log));
    device.create_shader(shader_with(0, max - 1, 1)).unwrap();
}

#[test]
fn shader_uniform_beyond_the_size_limit_is_rejected() {
    let (mut device, log) = device_with(DeviceLimits::default());
    let max = device.capabilities().limits.max_uniform_buffer_binding_size;
    let error = device
        .create_shader(shader_with(0, 0, max / 16 + 1))
        .unwrap_err();
    assert_eq!(limit_name(&error), "max_uniform_buffer_binding_size");
    assert!(!created_shader(&log));
    device.create_shader(shader_with(0, 0, max / 16)).unwrap();
}

#[test]
fn mesh_stride_beyond_the_limit_is_rejected_on_create_and_update() {
    let (mut device, log) = device_with(DeviceLimits::default());
    let max = device.capabilities().limits.max_vertex_buffer_array_stride;
    let stride = (max / 4 + 1) * 4;
    let too_wide = mesh(stride, 2);
    let error = device.create_mesh(&too_wide).unwrap_err();
    assert_eq!(limit_name(&error), "max_vertex_buffer_array_stride");
    assert!(
        !log.events()
            .iter()
            .any(|event| matches!(event, BackendEvent::CreateMesh { .. }))
    );
    let handle = device.create_mesh(&mesh(8, 2)).unwrap();
    let error = device.update_mesh(handle, &too_wide).unwrap_err();
    assert_eq!(limit_name(&error), "max_vertex_buffer_array_stride");
    device.update_mesh(handle, &mesh(12, 3)).unwrap();
}

#[test]
fn mesh_attribute_count_beyond_the_limit_is_rejected() {
    // A generous stride limit so that only the attribute count can trip.
    let (mut device, _log) = device_with(DeviceLimits {
        max_vertex_buffer_array_stride: 4096,
        ..DeviceLimits::default()
    });
    let max = device.capabilities().limits.max_vertex_attributes;
    let error = device
        .create_mesh(&mesh((max + 1) * 4, max + 1))
        .unwrap_err();
    assert_eq!(limit_name(&error), "max_vertex_attributes");
    device.create_mesh(&mesh(max * 4, max)).unwrap();
}

#[test]
fn custom_backend_limits_drive_the_checks() {
    let (mut device, _log) = device_with(DeviceLimits {
        max_bind_groups: 8,
        max_uniform_buffer_binding_size: 64 << 10,
        max_vertex_buffer_array_stride: 2048,
        ..DeviceLimits::default()
    });
    device
        .create_shader(shader_with(5, 0, 4096))
        .expect("group 5 and a 64 KiB uniform fit the reported limits");
    device.create_mesh(&mesh(1024, 2)).unwrap();
    let error = device.create_shader(shader_with(8, 0, 1)).unwrap_err();
    assert!(limit_name(&error).starts_with("max_bind_groups"));
}

#[test]
fn limit_errors_do_not_poison_the_device() {
    let (mut device, _log) = device_with(DeviceLimits::default());
    assert!(device.create_shader(shader_with(9, 0, 1)).is_err());
    assert!(device.create_mesh(&mesh(4096, 2)).is_err());
    device
        .create_shader(ShaderResource::new("ok", ShaderSource::wgsl(VALID)))
        .unwrap();
    device.create_mesh(&mesh(12, 3)).unwrap();
}
