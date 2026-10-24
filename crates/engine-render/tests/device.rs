//! Behavioural tests of `RenderDevice` over the deterministic recording backend.

use engine_render::headless::{
    BackendEvent, FailPoint, RecordingBackend, RecordingConfig, RecordingLog,
};
use engine_render::*;

const SHADER: &str = "
struct Camera { view_projection: mat4x4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
struct VertexOutput { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs_main(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>) -> VertexOutput {
    var output: VertexOutput;
    output.position = camera.view_projection * vec4<f32>(position, 1.0);
    output.uv = uv;
    return output;
}
@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(image, image_sampler, input.uv);
}
";

const UNTEXTURED: &str = "
@vertex fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(position, 1.0);
}
@fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }
";

fn layout() -> VertexLayout {
    VertexLayout::new(
        20,
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
        ],
    )
}

fn triangle() -> MeshResource {
    MeshResource::new(vec![0; 60], layout(), vec![]).unwrap()
}

fn device(frames: u32) -> (RenderDevice<RecordingBackend>, RecordingLog) {
    let backend = RecordingBackend::new();
    let log = backend.log();
    let device = RenderDevice::new(
        backend,
        DeviceConfig {
            frames_in_flight: frames,
        },
    )
    .unwrap();
    (device, log)
}

fn camera_uniform() -> DrawUniform {
    DrawUniform::from_bytes("camera", UniformSlot::new(0, 0), vec![0; 64])
}

fn textured_draw(shader: ShaderHandle, mesh: MeshHandle) -> DrawCommand {
    DrawCommand::new(shader, mesh).with_uniform(camera_uniform())
}

fn frame_of(draws: Vec<DrawCommand>) -> RenderCommandBuffer {
    let mut buffer = RenderCommandBuffer::new();
    for draw in draws {
        buffer.record(draw).unwrap();
    }
    buffer
}

fn surface_frame() -> FrameDesc {
    FrameDesc::new(RenderTarget::Surface)
}

#[test]
fn new_device_initializes_the_backend_and_clamps_frames_in_flight() {
    let backend = RecordingBackend::with_config(RecordingConfig {
        max_frames_in_flight: 3,
        ..Default::default()
    });
    let log = backend.log();
    let device = RenderDevice::new(
        backend,
        DeviceConfig {
            frames_in_flight: 8,
        },
    )
    .unwrap();
    assert_eq!(device.frames_in_flight(), 3);
    assert!(device.capabilities().backend_ready);
    assert_eq!(log.events(), [BackendEvent::Initialize]);
    assert!(
        RenderDevice::new(
            RecordingBackend::new(),
            DeviceConfig {
                frames_in_flight: 0
            }
        )
        .is_err()
    );
}

#[test]
fn initialization_failure_is_reported_verbatim() {
    let backend = RecordingBackend::new();
    backend.log().fail_next(
        FailPoint::Initialize,
        RenderError::Unavailable("no adapter".into()),
    );
    let error = RenderDevice::new(backend, DeviceConfig::default())
        .err()
        .unwrap();
    assert_eq!(error, RenderError::Unavailable("no adapter".into()));
}

#[test]
fn handles_are_unique_across_kinds_and_tracked_per_kind() {
    let (mut device, log) = device(2);
    let shader = device
        .create_shader(ShaderResource::new("s", ShaderSource::wgsl(SHADER)))
        .unwrap();
    let mesh = device.create_mesh(&triangle()).unwrap();
    let texture = device.create_texture(&Texture2D::white()).unwrap();
    let target = device.create_target(8, 8).unwrap();
    let ids = [shader.id(), mesh.id(), texture.id(), target.id()];
    let mut sorted = ids;
    sorted.sort_unstable();
    sorted.windows(2).for_each(|w| assert_ne!(w[0], w[1]));
    assert_eq!(
        device.counts(),
        ResourceCounts {
            shaders: 1,
            meshes: 1,
            textures: 1,
            targets: 1,
            pending_destruction: 0
        }
    );
    assert!(device.is_alive(shader) && device.is_alive(target));
    assert_eq!(log.live_resources().len(), 4);
    assert_eq!(device.target_size(target), Some((8, 8)));
    assert_eq!(device.target_size(TargetHandle::from_raw(999)), None);
}

#[test]
fn invalid_resources_are_rejected_before_reaching_the_backend() {
    let (mut device, log) = device(2);
    log.clear_events();
    let bad_shader = ShaderResource::new("bad", ShaderSource::wgsl("@vertex fn vs_main("));
    assert!(matches!(
        device.create_shader(bad_shader),
        Err(RenderError::Shader(_))
    ));
    let no_fragment = ShaderResource::new(
        "half",
        ShaderSource::wgsl(
            "@vertex fn vs_main() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }",
        ),
    );
    assert!(
        device
            .create_shader(no_fragment)
            .unwrap_err()
            .to_string()
            .contains("fragment entry")
    );
    let mut broken_mesh = triangle();
    broken_mesh.vertices.pop();
    assert!(matches!(
        device.create_mesh(&broken_mesh),
        Err(RenderError::InvalidResource(_))
    ));
    let huge = Texture2D {
        width: 8192,
        height: 1,
        rgba8: vec![0; 8192 * 4],
    };
    assert!(
        device
            .create_texture(&huge)
            .unwrap_err()
            .to_string()
            .contains("1..=4096")
    );
    assert!(device.create_target(0, 4).is_err());
    assert!(log.events().is_empty(), "{:?}", log.events());
    assert_eq!(device.counts(), ResourceCounts::default());
}

#[test]
fn backend_creation_failure_does_not_leave_a_tracked_resource() {
    let (mut device, log) = device(2);
    log.fail_next(
        FailPoint::CreateMesh,
        RenderError::Backend("out of memory".into()),
    );
    assert!(device.create_mesh(&triangle()).is_err());
    assert_eq!(device.counts().meshes, 0);
    let mesh = device.create_mesh(&triangle()).unwrap();
    assert!(device.is_alive(mesh));
}

#[test]
fn destroyed_resources_are_stale_immediately_but_released_after_frames_in_flight() {
    let (mut device, log) = device(2);
    let mesh = device.create_mesh(&triangle()).unwrap();
    device.destroy(mesh).unwrap();
    assert!(!device.is_alive(mesh));
    assert_eq!(device.counts().pending_destruction, 1);
    assert!(
        log.destroyed().is_empty(),
        "backend release must be deferred"
    );
    assert_eq!(
        device.destroy(mesh),
        Err(RenderError::StaleHandle(mesh.into()))
    );

    device
        .submit(surface_frame(), RenderCommandBuffer::new())
        .unwrap();
    assert!(
        log.destroyed().is_empty(),
        "one frame later is still too early for 2 in flight"
    );
    device
        .submit(surface_frame(), RenderCommandBuffer::new())
        .unwrap();
    assert_eq!(log.destroyed(), [ResourceId::Mesh(mesh)]);
    assert_eq!(device.counts().pending_destruction, 0);
}

#[test]
fn deferral_window_scales_with_frames_in_flight() {
    let (mut device, log) = device(1);
    let texture = device.create_texture(&Texture2D::white()).unwrap();
    device.destroy(texture).unwrap();
    device
        .submit(surface_frame(), RenderCommandBuffer::new())
        .unwrap();
    assert_eq!(log.destroyed(), [ResourceId::Texture(texture)]);
}

#[test]
fn a_resource_used_by_the_last_frame_survives_until_that_frame_is_complete() {
    let (mut device, log) = device(2);
    let shader = device
        .create_shader(ShaderResource::new("s", ShaderSource::wgsl(SHADER)))
        .unwrap();
    let mesh = device.create_mesh(&triangle()).unwrap();
    device
        .submit(surface_frame(), frame_of(vec![textured_draw(shader, mesh)]))
        .unwrap();
    device.destroy(mesh).unwrap();
    let frame_event = log
        .position(|e| matches!(e, BackendEvent::Frame(_)))
        .unwrap();
    // Two more frames complete the window; the destroy event must come after all of them.
    device
        .submit(surface_frame(), RenderCommandBuffer::new())
        .unwrap();
    device
        .submit(surface_frame(), RenderCommandBuffer::new())
        .unwrap();
    let destroy_event = log
        .position(|e| matches!(e, BackendEvent::Destroy(_)))
        .unwrap();
    let frames_before_destroy = log.events()[..destroy_event]
        .iter()
        .filter(|e| matches!(e, BackendEvent::Frame(_)))
        .count();
    assert!(destroy_event > frame_event);
    assert_eq!(frames_before_destroy, 3);
}

#[test]
fn skipped_frames_do_not_advance_destruction() {
    let (mut device, log) = device(1);
    let mesh = device.create_mesh(&triangle()).unwrap();
    device.destroy(mesh).unwrap();
    device.resize(10, 10).unwrap();
    // Minimized surface: backend reports Suspended, nothing was rendered.
    let outcome = {
        // Force a zero-size surface directly through the backend, as a window host would.
        use engine_render::RenderBackend;
        device.backend_mut().resize(0, 0).unwrap();
        device
            .submit(surface_frame(), RenderCommandBuffer::new())
            .unwrap()
    };
    assert_eq!(outcome, FrameOutcome::Skipped(FrameSkip::Suspended));
    assert_eq!(device.frames_submitted(), 0);
    assert!(log.destroyed().is_empty());
    device.collect_garbage().unwrap();
    assert!(log.destroyed().is_empty());
}

#[test]
fn wait_idle_flushes_pending_destruction() {
    let (mut device, log) = device(2);
    let mesh = device.create_mesh(&triangle()).unwrap();
    device.destroy(mesh).unwrap();
    device.wait_idle().unwrap();
    assert_eq!(log.destroyed(), [ResourceId::Mesh(mesh)]);
    let wait = log
        .position(|e| matches!(e, BackendEvent::WaitIdle))
        .unwrap();
    let destroy = log
        .position(|e| matches!(e, BackendEvent::Destroy(_)))
        .unwrap();
    assert!(wait < destroy);
}

#[test]
fn shutdown_releases_everything_in_a_defined_order_and_is_idempotent() {
    let (mut device, log) = device(2);
    let shader = device
        .create_shader(ShaderResource::new("s", ShaderSource::wgsl(SHADER)))
        .unwrap();
    let mesh = device.create_mesh(&triangle()).unwrap();
    let texture = device.create_texture(&Texture2D::white()).unwrap();
    device.destroy(texture).unwrap();
    device.shutdown().unwrap();
    device.shutdown().unwrap();
    assert!(log.is_shut_down());
    assert!(log.live_resources().is_empty());
    assert_eq!(
        log.destroyed(),
        [
            ResourceId::Texture(texture),
            ResourceId::Shader(shader),
            ResourceId::Mesh(mesh)
        ]
    );
    assert!(matches!(
        device.create_mesh(&triangle()),
        Err(RenderError::NotReady(_))
    ));
}

#[test]
fn dropping_a_device_shuts_the_backend_down() {
    let (mut device, log) = device(2);
    device.create_mesh(&triangle()).unwrap();
    drop(std::mem::replace(
        &mut device,
        RenderDevice::new(RecordingBackend::new(), DeviceConfig::default()).unwrap(),
    ));
    assert!(log.is_shut_down());
    assert!(log.live_resources().is_empty());
}

#[test]
fn draws_reach_the_backend_in_submission_order_with_uniform_bytes_and_state() {
    let (mut device, log) = device(2);
    let shader = device
        .create_shader(ShaderResource::new("s", ShaderSource::wgsl(SHADER)))
        .unwrap();
    let mesh_a = device.create_mesh(&triangle()).unwrap();
    let mesh_b = device.create_mesh(&triangle()).unwrap();
    let texture = device.create_texture(&Texture2D::white()).unwrap();
    let blended = DrawState {
        blend: BlendMode::StraightAlpha,
        depth_write: false,
        scissor: Some(PixelRect::new(1, 2, 3, 4)),
        ..Default::default()
    };
    let mut camera = textured_draw(shader, mesh_b)
        .with_texture(texture)
        .with_state(blended);
    camera.uniforms[0].bytes = (0..64).collect();
    let mut buffer = RenderCommandBuffer::new();
    buffer.record(textured_draw(shader, mesh_a)).unwrap();
    buffer.record(camera).unwrap();
    let mut desc = surface_frame();
    desc.clear_color = [0.25, 0.5, 0.75, 1.0];
    desc.viewport = Some(PixelRect::new(0, 8, 64, 48));
    let outcome = device.submit(desc, buffer).unwrap();
    assert!(matches!(outcome, FrameOutcome::Rendered(stats) if stats.draws == 2));
    let frames = log.frames();
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    assert_eq!(frame.desc.frame, 0);
    assert_eq!(frame.desc.clear_color, [0.25, 0.5, 0.75, 1.0]);
    assert_eq!(frame.desc.viewport, Some(PixelRect::new(0, 8, 64, 48)));
    assert_eq!(
        frame.draws.iter().map(|d| d.mesh).collect::<Vec<_>>(),
        [mesh_a, mesh_b]
    );
    assert_eq!(
        frame.draws.iter().map(|d| d.order).collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(frame.draws[1].texture, Some(texture));
    assert_eq!(frame.draws[1].state, blended);
    assert_eq!(frame.draws[1].uniforms[0].1, (0..64).collect::<Vec<u8>>());
}

#[test]
fn keyed_buffers_submit_in_key_order_not_recording_order() {
    let (mut device, log) = device(2);
    let shader = device
        .create_shader(ShaderResource::new("s", ShaderSource::wgsl(SHADER)))
        .unwrap();
    let meshes: Vec<_> = (0..3)
        .map(|_| device.create_mesh(&triangle()).unwrap())
        .collect();
    let mut buffer = device.make_frame();
    for (mesh, key) in [
        (meshes[0], (1, 0)),
        (meshes[1], (0, 9)),
        (meshes[2], (0, 2)),
    ] {
        buffer
            .record_keyed(textured_draw(shader, mesh), DrawOrderKey::new(key.0, key.1))
            .unwrap();
    }
    device.submit(surface_frame(), buffer).unwrap();
    let order: Vec<_> = log.frames()[0].draws.iter().map(|d| d.mesh).collect();
    assert_eq!(order, [meshes[2], meshes[1], meshes[0]]);
}

#[test]
fn invalid_draws_fail_the_whole_frame_without_submitting_anything() {
    let (mut device, log) = device(2);
    let shader = device
        .create_shader(ShaderResource::new("s", ShaderSource::wgsl(SHADER)))
        .unwrap();
    let plain = device
        .create_shader(ShaderResource::new("plain", ShaderSource::wgsl(UNTEXTURED)))
        .unwrap();
    let mesh = device.create_mesh(&triangle()).unwrap();
    let texture = device.create_texture(&Texture2D::white()).unwrap();
    let narrow = device
        .create_mesh(
            &MeshResource::new(
                vec![0; 36],
                VertexLayout::new(
                    12,
                    [VertexAttribute {
                        location: 0,
                        format: VertexFormat::Float32x3,
                        offset: 0,
                    }],
                ),
                vec![],
            )
            .unwrap(),
        )
        .unwrap();
    let good = || textured_draw(shader, mesh);

    let missing_uniform = DrawCommand::new(shader, mesh);
    let wrong_size = DrawCommand::new(shader, mesh).with_uniform(DrawUniform::from_bytes(
        "camera",
        UniformSlot::new(0, 0),
        vec![0; 16],
    ));
    let missing_attribute = textured_draw(shader, narrow);
    let texture_without_sampling = DrawCommand::new(plain, mesh).with_texture(texture);
    let cases = [
        (missing_uniform, "missing uniform"),
        (wrong_size, "64"),
        (missing_attribute, "location 1"),
        (texture_without_sampling, "samples none"),
    ];
    for (bad, expected) in cases {
        let error = device
            .submit(surface_frame(), frame_of(vec![good(), bad]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("draw 1"), "{error}");
        assert!(error.contains(expected), "expected {expected:?} in {error}");
    }
    assert!(log.frames().is_empty());
    assert_eq!(device.frames_submitted(), 0);
}

#[test]
fn draws_of_destroyed_or_foreign_handles_are_stale_even_before_backend_release() {
    let (mut device, log) = device(2);
    let shader = device
        .create_shader(ShaderResource::new("s", ShaderSource::wgsl(SHADER)))
        .unwrap();
    let mesh = device.create_mesh(&triangle()).unwrap();
    device.destroy(mesh).unwrap();
    let error = device
        .submit(surface_frame(), frame_of(vec![textured_draw(shader, mesh)]))
        .unwrap_err();
    assert_eq!(error, RenderError::StaleHandle(mesh.into()));
    let foreign = MeshHandle::from_raw(12345);
    assert!(matches!(
        device.submit(
            surface_frame(),
            frame_of(vec![textured_draw(shader, foreign)])
        ),
        Err(RenderError::StaleHandle(_))
    ));
    assert!(log.frames().is_empty());
}

#[test]
fn offscreen_frames_render_and_read_back_the_srgb_clear_color() {
    let (mut device, _log) = device(2);
    let target = device.create_target(4, 3).unwrap();
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = [0.0, 0.5, 1.0, 1.0];
    device.submit(desc, RenderCommandBuffer::new()).unwrap();
    let image = device.read_target(target).unwrap();
    assert_eq!((image.width, image.height, image.rgba8.len()), (4, 3, 48));
    assert_eq!(image.pixel(3, 2), Some([0, 188, 255, 255]));
    device.destroy(target).unwrap();
    assert!(matches!(
        device.read_target(target),
        Err(RenderError::StaleHandle(_))
    ));
    assert!(matches!(
        device.submit(
            FrameDesc::new(RenderTarget::Offscreen(target)),
            RenderCommandBuffer::new()
        ),
        Err(RenderError::StaleHandle(_))
    ));
}

#[test]
fn preserved_color_frames_keep_the_existing_image() {
    let (mut device, _log) = device(2);
    let target = device.create_target(2, 2).unwrap();
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = [0.0, 0.5, 1.0, 1.0];
    device.submit(desc, RenderCommandBuffer::new()).unwrap();
    let mut overlay = FrameDesc::new(RenderTarget::Offscreen(target));
    assert!(!overlay.preserve_color, "frames clear by default");
    overlay.clear_color = [1.0, 0.0, 0.0, 1.0];
    overlay.preserve_color = true;
    device.submit(overlay, RenderCommandBuffer::new()).unwrap();
    let image = device.read_target(target).unwrap();
    assert_eq!(image.pixel(1, 1), Some([0, 188, 255, 255]));
}

#[test]
fn updates_validate_handles_and_forward_to_the_backend() {
    let (mut device, log) = device(2);
    let mesh = device.create_mesh(&triangle()).unwrap();
    let texture = device.create_texture(&Texture2D::white()).unwrap();
    device.update_mesh(mesh, &triangle()).unwrap();
    device
        .update_texture(texture, &Texture2D::new(2, 2, vec![9; 16]).unwrap())
        .unwrap();
    assert!(
        log.position(|e| matches!(e, BackendEvent::UpdateMesh { .. }))
            .is_some()
    );
    assert!(
        log.position(|e| matches!(e, BackendEvent::UpdateTexture { .. }))
            .is_some()
    );
    device.destroy(mesh).unwrap();
    assert!(matches!(
        device.update_mesh(mesh, &triangle()),
        Err(RenderError::StaleHandle(_))
    ));
    let mut bad = triangle();
    bad.vertex_count = 7;
    let live = device.create_mesh(&triangle()).unwrap();
    assert!(device.update_mesh(live, &bad).is_err());
}

#[test]
fn updating_a_mesh_changes_which_shaders_accept_it() {
    let (mut device, _log) = device(2);
    let shader = device
        .create_shader(ShaderResource::new("s", ShaderSource::wgsl(SHADER)))
        .unwrap();
    let mesh = device.create_mesh(&triangle()).unwrap();
    let positions_only = MeshResource::new(
        vec![0; 36],
        VertexLayout::new(
            12,
            [VertexAttribute {
                location: 0,
                format: VertexFormat::Float32x3,
                offset: 0,
            }],
        ),
        vec![],
    )
    .unwrap();
    device.update_mesh(mesh, &positions_only).unwrap();
    assert!(
        device
            .submit(surface_frame(), frame_of(vec![textured_draw(shader, mesh)]))
            .is_err()
    );
}

#[test]
fn resize_rejects_zero_oversize_and_surfaceless_backends() {
    let (mut device, log) = device(2);
    assert!(
        device
            .resize(0, 10)
            .unwrap_err()
            .to_string()
            .contains("suspended")
    );
    assert!(
        device
            .resize(5000, 10)
            .unwrap_err()
            .to_string()
            .contains("exceeds")
    );
    device.resize(320, 200).unwrap();
    assert!(
        log.position(|e| matches!(
            e,
            BackendEvent::Resize {
                width: 320,
                height: 200
            }
        ))
        .is_some()
    );

    let headless = RecordingBackend::with_config(RecordingConfig {
        surface: None,
        ..Default::default()
    });
    let mut device = RenderDevice::new(headless, DeviceConfig::default()).unwrap();
    assert!(matches!(
        device.resize(8, 8),
        Err(RenderError::Unsupported { .. })
    ));
    let error = device
        .submit(surface_frame(), RenderCommandBuffer::new())
        .unwrap_err();
    assert!(
        matches!(error, RenderError::Unsupported { feature, .. } if feature == "surface-presentation")
    );
}

#[test]
fn device_loss_poisons_the_device_while_other_errors_do_not() {
    let (mut device, log) = device(2);
    log.fail_next(
        FailPoint::SubmitFrame,
        RenderError::Backend("transient".into()),
    );
    assert_eq!(
        device.submit(surface_frame(), RenderCommandBuffer::new()),
        Err(RenderError::Backend("transient".into()))
    );
    device
        .submit(surface_frame(), RenderCommandBuffer::new())
        .unwrap();
    log.fail_next(
        FailPoint::SubmitFrame,
        RenderError::DeviceLost("TDR".into()),
    );
    assert!(matches!(
        device.submit(surface_frame(), RenderCommandBuffer::new()),
        Err(RenderError::DeviceLost(_))
    ));
    assert_eq!(
        device.create_mesh(&triangle()),
        Err(RenderError::DeviceLost("TDR".into()))
    );
    assert!(device.wait_idle().is_err());
    // Cleanup still reaches the backend.
    device.shutdown().unwrap();
    assert!(log.is_shut_down());
}

#[test]
fn shutdown_reports_the_first_backend_failure_but_still_tears_everything_down() {
    let (mut device, log) = device(2);
    let a = device.create_mesh(&triangle()).unwrap();
    let b = device.create_mesh(&triangle()).unwrap();
    log.fail_next(
        FailPoint::Destroy,
        RenderError::Backend("driver hiccup".into()),
    );
    let error = device.shutdown().unwrap_err();
    assert_eq!(error, RenderError::Backend("driver hiccup".into()));
    assert!(log.is_shut_down());
    assert_eq!(
        log.destroyed().len(),
        1,
        "the other mesh was still destroyed: {a:?} {b:?}"
    );
}

#[test]
fn boxed_dyn_backends_drive_a_device_unchanged() {
    let backend: Box<dyn RenderBackend> = Box::new(RecordingBackend::new());
    let mut device = RenderDevice::new(backend, DeviceConfig::default()).unwrap();
    assert_eq!(device.capabilities().kind, BackendKind::Headless);
    let mesh = device.create_mesh(&triangle()).unwrap();
    device.destroy(mesh).unwrap();
    device.shutdown().unwrap();
}

#[test]
fn unsupported_backend_features_surface_their_reason() {
    struct NoTextures(RecordingBackend);
    macro_rules! forward {
        ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {$(
            fn $name(&mut self, $($arg: $ty),*) -> $ret { self.0.$name($($arg),*) }
        )*};
    }
    impl RenderBackend for NoTextures {
        fn capabilities(&self) -> RenderCapabilities {
            let mut caps = self.0.capabilities();
            caps.sampled_textures = false;
            caps.unsupported_reasons
                .insert(RenderFeature::SampledTextures, "portability subset".into());
            caps
        }
        forward! {
            initialize() -> Result<()>;
            create_shader(h: ShaderHandle, r: &ShaderResource) -> Result<()>;
            create_mesh(h: MeshHandle, r: &MeshResource) -> Result<()>;
            create_texture(h: TextureHandle, t: &Texture2D) -> Result<()>;
            create_target(h: TargetHandle, d: &OffscreenDesc) -> Result<()>;
            update_mesh(h: MeshHandle, r: &MeshResource) -> Result<()>;
            update_texture(h: TextureHandle, t: &Texture2D) -> Result<()>;
            destroy_resource(id: ResourceId) -> Result<()>;
            resize(w: u32, h: u32) -> Result<()>;
            submit_frame(f: &FrameDesc, d: &[DrawCommand]) -> Result<FrameOutcome>;
            read_target(t: TargetHandle) -> Result<ReadbackImage>;
            wait_idle() -> Result<()>;
            shutdown() -> Result<()>;
        }
    }
    let mut device =
        RenderDevice::new(NoTextures(RecordingBackend::new()), DeviceConfig::default()).unwrap();
    let error = device.create_texture(&Texture2D::white()).unwrap_err();
    assert_eq!(
        error,
        RenderError::Unsupported {
            feature: "sampled-textures".into(),
            reason: "portability subset".into()
        }
    );
}
#[test]
fn invalid_shadow_commands_fail_before_backend_submission() {
    let (mut device, log) = device(2);
    let shader = device
        .create_shader(ShaderResource::new(
            "untextured",
            ShaderSource::wgsl(UNTEXTURED),
        ))
        .unwrap();
    let mesh = device.create_mesh(&triangle()).unwrap();
    let mut draw = DrawCommand::new(shader, mesh);
    let identity = [
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ];
    for resolution in [0, 65, 8192] {
        draw.shadow = Some(ShadowCaster {
            light_mvp: identity,
            resolution,
        });
        assert!(
            device
                .submit(surface_frame(), frame_of(vec![draw.clone()]))
                .is_err()
        );
    }
    draw.shadow = Some(ShadowCaster {
        light_mvp: identity,
        resolution: 256,
    });
    let mut other = draw.clone();
    other.shadow.as_mut().unwrap().resolution = 512;
    assert!(
        device
            .submit(surface_frame(), frame_of(vec![draw.clone(), other]))
            .is_err()
    );
    draw.shadow.as_mut().unwrap().light_mvp[0][0] = f32::NAN;
    assert!(
        device
            .submit(surface_frame(), frame_of(vec![draw]))
            .is_err()
    );
    assert!(log.frames().is_empty());
}
