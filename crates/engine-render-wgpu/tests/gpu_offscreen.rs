//! GPU tests of the wgpu backend through `RenderDevice<WgpuBackend>` with
//! offscreen targets (they mirror `engine-vulkan`'s `gpu_offscreen.rs` so both
//! backends honour the same contract). They fail (not skip) without a wgpu
//! adapter and are ignored in ordinary runs:
//! `cargo test -p engine-render-wgpu -- --ignored --nocapture`.

mod common;

use common::*;
use engine_render::*;

const OPAQUE: DrawState = DrawState {
    blend: BlendMode::Opaque,
    depth_test: true,
    depth_write: true,
    cull: CullMode::None,
    scissor: None,
};

fn camera_64() -> [f32; 16] {
    pixel_camera(64.0, 64.0)
}

fn full_quad(z: f32, color: [f32; 4]) -> MeshResource {
    quad(0.0, 0.0, 64.0, 64.0, z, color)
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn capabilities_describe_the_real_device() {
    let (device, log) = open(2);
    let caps = device.capabilities();
    assert_eq!(caps.kind, BackendKind::Wgpu);
    assert!(
        caps.backend_name.starts_with("wgpu/"),
        "{}",
        caps.backend_name
    );
    assert!(caps.backend_ready);
    assert!(caps.device_name.as_deref().is_some_and(|n| !n.is_empty()));
    assert_eq!(caps.max_frames_in_flight, 2);
    assert!(caps.max_texture_dimension >= 4096);
    assert!(caps.offscreen_targets && caps.readback && caps.sampled_textures);
    assert!(caps.depth_attachment);
    assert!(
        !caps.surface_presentation,
        "offscreen backend has no surface"
    );
    assert!(
        !caps
            .feature(RenderFeature::SurfacePresentation)
            .reason
            .is_empty()
    );
    assert!(!caps.parallel_recording);
    eprintln!("{}", caps.diagnostics());
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn frames_in_flight_are_clamped_to_eight() {
    let backend = engine_render_wgpu::WgpuBackend::offscreen(engine_render_wgpu::WgpuOptions {
        frames_in_flight: 20,
        ..Default::default()
    });
    let device = RenderDevice::new(
        backend,
        DeviceConfig {
            frames_in_flight: 8,
        },
    )
    .unwrap();
    assert_eq!(device.capabilities().max_frames_in_flight, 8);
    assert_eq!(device.frames_in_flight(), 8);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn clear_color_reads_back_as_the_exact_srgb_value() {
    let (mut device, log) = open(2);
    let target = device.create_target(16, 8).unwrap();
    // Linear clear values chosen so their sRGB encoding is an exact byte.
    let clear = [
        srgb_to_linear(64),
        srgb_to_linear(128),
        srgb_to_linear(200),
        1.0,
    ];
    let image = render_and_read(&mut device, target, clear, vec![]);
    assert_eq!((image.width, image.height), (16, 8));
    assert_eq!(image.rgba8.len(), 16 * 8 * 4);
    for y in 0..8 {
        for x in 0..16 {
            assert_eq!(pixel(&image, x, y), [64, 128, 200, 255], "at ({x},{y})");
        }
    }
    // Extremes and a transparent clear (alpha is stored linearly).
    let image = render_and_read(&mut device, target, [1.0, 0.0, 1.0, 0.0], vec![]);
    assert_eq!(pixel(&image, 3, 3), [255, 0, 255, 0]);
    let image = render_and_read(&mut device, target, [0.0, 1.0, 0.0, 1.0], vec![]);
    assert_eq!(pixel(&image, 15, 7), [0, 255, 0, 255]);
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn a_fresh_target_reads_back_as_defined_transparent_black() {
    let (mut device, log) = open(2);
    let target = device.create_target(8, 8).unwrap();
    let image = device.read_target(target).unwrap();
    assert!(image.rgba8.iter().all(|b| *b == 0));
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn textured_quad_has_exact_pixels_and_the_expected_orientation() {
    let (mut device, log) = open(2);
    let target = device.create_target(64, 64).unwrap();
    let program = shader(&mut device, TEXTURED);
    // Row 0 is red, green; row 1 is blue, white.
    let atlas = device
        .create_texture(
            &Texture2D::new(
                2,
                2,
                vec![
                    255, 0, 0, 255, 0, 255, 0, 255, //
                    0, 0, 255, 255, 255, 255, 255, 255,
                ],
            )
            .unwrap(),
        )
        .unwrap();
    let mesh = device
        .create_mesh(&quad(16.0, 16.0, 48.0, 48.0, 0.5, [1.0; 4]))
        .unwrap();
    let camera = camera_64();
    let draws = || vec![draw(program, mesh, Some(atlas), &camera, OPAQUE)];

    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = BLACK;
    let outcome = submit(&mut device, desc, draws()).unwrap();
    assert_eq!(
        outcome,
        FrameOutcome::Rendered(FrameStats {
            draws: 1,
            batches: 1
        })
    );
    let image = device.read_target(target).unwrap();
    assert_eq!(pixel(&image, 20, 20), [255, 0, 0, 255], "top-left is red");
    assert_eq!(
        pixel(&image, 44, 20),
        [0, 255, 0, 255],
        "top-right is green"
    );
    assert_eq!(
        pixel(&image, 20, 44),
        [0, 0, 255, 255],
        "bottom-left is blue"
    );
    assert_eq!(
        pixel(&image, 44, 44),
        [255, 255, 255, 255],
        "bottom-right is white"
    );
    assert_eq!(
        pixel(&image, 2, 2),
        [0, 0, 0, 255],
        "background stays black"
    );
    assert_eq!(pixel(&image, 62, 62), [0, 0, 0, 255]);
    // Quad edges: first/last covered pixel on both axes.
    assert_eq!(pixel(&image, 16, 16), [255, 0, 0, 255]);
    assert_eq!(pixel(&image, 47, 47), [255, 255, 255, 255]);
    assert_eq!(pixel(&image, 15, 16), [0, 0, 0, 255]);
    assert_eq!(pixel(&image, 48, 47), [0, 0, 0, 255]);
    assert_eq!(pixel(&image, 16, 15), [0, 0, 0, 255]);

    // Rendering the same frame again is byte-identical.
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = BLACK;
    submit(&mut device, desc, draws()).unwrap();
    assert_eq!(device.read_target(target).unwrap(), image);

    // A draw without a texture binds the white texture: color is the vertex color.
    let tinted = device
        .create_mesh(&quad(0.0, 0.0, 32.0, 32.0, 0.5, [1.0, 1.0, 0.0, 1.0]))
        .unwrap();
    let image = render_and_read(
        &mut device,
        target,
        BLACK,
        vec![draw(program, tinted, None, &camera, OPAQUE)],
    );
    assert_eq!(pixel(&image, 10, 10), [255, 255, 0, 255]);
    assert_eq!(pixel(&image, 40, 40), [0, 0, 0, 255]);

    // A texture tint multiplies in linear space: 0.5 gray vertex color over white.
    let gray = device
        .create_mesh(&quad(0.0, 0.0, 64.0, 64.0, 0.5, [0.5, 0.5, 0.5, 1.0]))
        .unwrap();
    let image = render_and_read(
        &mut device,
        target,
        BLACK,
        vec![draw(program, gray, None, &camera, OPAQUE)],
    );
    assert_eq!(pixel(&image, 32, 32), [188, 188, 188, 255]);
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn straight_alpha_blends_green_over_red() {
    let (mut device, log) = open(2);
    let target = device.create_target(64, 64).unwrap();
    let program = shader(&mut device, TEXTURED);
    let red = device
        .create_mesh(&full_quad(0.2, [1.0, 0.0, 0.0, 1.0]))
        .unwrap();
    let green = device
        .create_mesh(&full_quad(0.8, [0.0, 1.0, 0.0, 0.5]))
        .unwrap();
    let camera = camera_64();
    let blended = state(BlendMode::StraightAlpha, false, false);
    let image = render_and_read(
        &mut device,
        target,
        BLACK,
        vec![
            draw(program, red, None, &camera, OPAQUE),
            draw(program, green, None, &camera, blended),
        ],
    );
    let color = pixel(&image, 32, 32);
    assert!((187..=188).contains(&color[0]), "{color:?}");
    assert!((187..=188).contains(&color[1]), "{color:?}");
    assert_eq!(&color[2..], &[0, 255], "{color:?}");

    // The same translucent green with blending off replaces the destination.
    let image = render_and_read(
        &mut device,
        target,
        BLACK,
        vec![
            draw(program, red, None, &camera, OPAQUE),
            draw(
                program,
                green,
                None,
                &camera,
                state(BlendMode::Opaque, false, false),
            ),
        ],
    );
    assert_eq!(pixel(&image, 32, 32)[..3], [0, 255, 0]);
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn depth_test_and_write_flags_decide_what_is_visible() {
    let (mut device, log) = open(2);
    let target = device.create_target(64, 64).unwrap();
    let program = shader(&mut device, TEXTURED);
    let camera = camera_64();
    let mesh = |device: &mut Device, z: f32, color: [f32; 4]| {
        device.create_mesh(&full_quad(z, color)).unwrap()
    };
    let near_red = mesh(&mut device, 0.2, [1.0, 0.0, 0.0, 1.0]);
    let far_blue = mesh(&mut device, 0.8, [0.0, 0.0, 1.0, 1.0]);
    let mid_green = mesh(&mut device, 0.5, [0.0, 1.0, 0.0, 1.0]);
    let at = |device: &mut Device, draws| {
        let image = render_and_read(device, target, BLACK, draws);
        pixel(&image, 32, 32)
    };

    // Near-then-far with depth testing: the nearer red survives.
    let near_first = at(
        &mut device,
        vec![
            draw(program, near_red, None, &camera, OPAQUE),
            draw(program, far_blue, None, &camera, OPAQUE),
        ],
    );
    assert_eq!(near_first, [255, 0, 0, 255]);

    // Far-then-near: the nearer red wins regardless of draw order.
    let far_first = at(
        &mut device,
        vec![
            draw(program, far_blue, None, &camera, OPAQUE),
            draw(program, near_red, None, &camera, OPAQUE),
        ],
    );
    assert_eq!(far_first, [255, 0, 0, 255]);

    // Depth test off: the later draw always wins.
    let no_test = at(
        &mut device,
        vec![
            draw(program, near_red, None, &camera, OPAQUE),
            draw(
                program,
                far_blue,
                None,
                &camera,
                state(BlendMode::Opaque, false, true),
            ),
        ],
    );
    assert_eq!(no_test, [0, 0, 255, 255]);

    // Depth write off: the far blue left no depth, so green at 0.5 shows.
    let no_write = at(
        &mut device,
        vec![
            draw(
                program,
                far_blue,
                None,
                &camera,
                state(BlendMode::Opaque, true, false),
            ),
            draw(program, mid_green, None, &camera, OPAQUE),
        ],
    );
    assert_eq!(no_write, [0, 255, 0, 255]);

    // Control for the above: with writes on, blue at 0.8 is behind green at 0.5
    // only if drawn first; drawing green first hides the later blue.
    let with_write = at(
        &mut device,
        vec![
            draw(program, mid_green, None, &camera, OPAQUE),
            draw(program, far_blue, None, &camera, OPAQUE),
        ],
    );
    assert_eq!(with_write, [0, 255, 0, 255]);

    // Write without test still writes depth: red at 0.2 is recorded even though
    // it was drawn untested, so the tested green at 0.5 is rejected afterwards.
    let write_only = at(
        &mut device,
        vec![
            draw(
                program,
                near_red,
                None,
                &camera,
                state(BlendMode::Opaque, false, true),
            ),
            draw(program, mid_green, None, &camera, OPAQUE),
        ],
    );
    assert_eq!(write_only, [255, 0, 0, 255]);

    // The depth buffer is cleared every frame: the same green now shows alone.
    let cleared = at(
        &mut device,
        vec![draw(program, mid_green, None, &camera, OPAQUE)],
    );
    assert_eq!(cleared, [0, 255, 0, 255]);

    // Clear depth 0.0 makes every tested fragment fail LESS_OR_EQUAL (z > 0).
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_depth = 0.0;
    submit(
        &mut device,
        desc,
        vec![draw(program, mid_green, None, &camera, OPAQUE)],
    )
    .unwrap();
    assert_eq!(
        pixel(&device.read_target(target).unwrap(), 32, 32),
        [0, 0, 0, 255]
    );
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn scissor_clips_in_top_left_pixel_coordinates_and_clamps() {
    let (mut device, log) = open(2);
    let target = device.create_target(64, 64).unwrap();
    let program = shader(&mut device, TEXTURED);
    let camera = camera_64();
    let red = device
        .create_mesh(&full_quad(0.5, [1.0, 0.0, 0.0, 1.0]))
        .unwrap();

    let mut clipped = OPAQUE;
    clipped.scissor = Some(PixelRect::new(4, 40, 20, 10));
    let image = render_and_read(
        &mut device,
        target,
        BLACK,
        vec![draw(program, red, None, &camera, clipped)],
    );
    assert_eq!(
        pixel(&image, 4, 40),
        [255, 0, 0, 255],
        "first covered pixel"
    );
    assert_eq!(
        pixel(&image, 23, 49),
        [255, 0, 0, 255],
        "last covered pixel"
    );
    assert_eq!(
        pixel(&image, 24, 45),
        [0, 0, 0, 255],
        "right of the scissor"
    );
    assert_eq!(pixel(&image, 10, 50), [0, 0, 0, 255], "below the scissor");
    assert_eq!(pixel(&image, 3, 45), [0, 0, 0, 255], "left of the scissor");
    assert_eq!(pixel(&image, 10, 39), [0, 0, 0, 255], "above the scissor");
    assert_eq!(
        pixel(&image, 10, 10),
        [0, 0, 0, 255],
        "y is top-down, not bottom-up"
    );
    let filled = image
        .rgba8
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[..3] == [255, 0, 0])
        .count();
    assert_eq!(filled, 20 * 10);

    // A scissor sticking out of the target is clamped; a fully outside one draws nothing.
    let mut overhang = OPAQUE;
    overhang.scissor = Some(PixelRect::new(60, 60, 100, 100));
    let mut outside = OPAQUE;
    outside.scissor = Some(PixelRect::new(100, 100, 5, 5));
    let image = render_and_read(
        &mut device,
        target,
        BLACK,
        vec![
            draw(program, red, None, &camera, overhang),
            draw(program, red, None, &camera, outside),
        ],
    );
    assert_eq!(pixel(&image, 63, 63), [255, 0, 0, 255]);
    assert_eq!(pixel(&image, 59, 59), [0, 0, 0, 255]);
    let filled = image
        .rgba8
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] == 255)
        .count();
    assert_eq!(filled, 16);

    // Scissors change per draw without a new pipeline: two rectangles in one frame.
    let mut left = OPAQUE;
    left.scissor = Some(PixelRect::new(0, 0, 8, 8));
    let mut right = OPAQUE;
    right.scissor = Some(PixelRect::new(56, 56, 8, 8));
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = BLACK;
    let outcome = submit(
        &mut device,
        desc,
        vec![
            draw(program, red, None, &camera, left),
            draw(program, red, None, &camera, right),
        ],
    )
    .unwrap();
    assert_eq!(
        outcome,
        FrameOutcome::Rendered(FrameStats {
            draws: 2,
            batches: 2
        })
    );
    let image = device.read_target(target).unwrap();
    assert_eq!(pixel(&image, 3, 3), [255, 0, 0, 255]);
    assert_eq!(pixel(&image, 60, 60), [255, 0, 0, 255]);
    assert_eq!(pixel(&image, 32, 32), [0, 0, 0, 255]);
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn viewport_maps_clip_space_into_the_requested_pixel_rectangle() {
    let (mut device, log) = open(2);
    let target = device.create_target(64, 64).unwrap();
    let program = shader(&mut device, TEXTURED);
    // Clip-space quad covering the upper half (y from 0 to 1) and the left half.
    let upper_left = device
        .create_mesh(&quad(-1.0, 1.0, 0.0, 0.0, 0.5, [1.0, 0.0, 0.0, 1.0]))
        .unwrap();
    let whole = device
        .create_mesh(&quad(-1.0, 1.0, 1.0, -1.0, 0.5, [0.0, 0.0, 1.0, 1.0]))
        .unwrap();

    // Without a viewport the clip-space upper-left quarter lands at the image's top-left.
    let image = render_and_read(
        &mut device,
        target,
        BLACK,
        vec![draw(program, upper_left, None, &IDENTITY, OPAQUE)],
    );
    assert_eq!(pixel(&image, 10, 10), [255, 0, 0, 255], "clip +y is up");
    assert_eq!(pixel(&image, 40, 10), [0, 0, 0, 255]);
    assert_eq!(pixel(&image, 10, 40), [0, 0, 0, 255]);

    // A viewport on the bottom-right quarter: the whole clip square shrinks into it.
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = BLACK;
    desc.viewport = Some(PixelRect::new(32, 32, 32, 32));
    submit(
        &mut device,
        desc,
        vec![draw(program, whole, None, &IDENTITY, OPAQUE)],
    )
    .unwrap();
    let image = device.read_target(target).unwrap();
    assert_eq!(pixel(&image, 32, 32), [0, 0, 255, 255]);
    assert_eq!(pixel(&image, 63, 63), [0, 0, 255, 255]);
    assert_eq!(pixel(&image, 31, 40), [0, 0, 0, 255]);
    assert_eq!(pixel(&image, 40, 31), [0, 0, 0, 255]);
    assert_eq!(pixel(&image, 10, 10), [0, 0, 0, 255]);

    // The upper-left clip quarter inside that viewport is its own top-left quarter.
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = BLACK;
    desc.viewport = Some(PixelRect::new(32, 32, 32, 32));
    submit(
        &mut device,
        desc,
        vec![draw(program, upper_left, None, &IDENTITY, OPAQUE)],
    )
    .unwrap();
    let image = device.read_target(target).unwrap();
    assert_eq!(pixel(&image, 40, 40), [255, 0, 0, 255]);
    assert_eq!(pixel(&image, 56, 40), [0, 0, 0, 255]);
    assert_eq!(pixel(&image, 40, 56), [0, 0, 0, 255]);

    // Viewport plus scissor: the scissor is in target pixels, not viewport pixels.
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = BLACK;
    desc.viewport = Some(PixelRect::new(0, 0, 64, 64));
    let mut scissored = OPAQUE;
    scissored.scissor = Some(PixelRect::new(0, 0, 4, 64));
    submit(
        &mut device,
        desc,
        vec![draw(program, whole, None, &IDENTITY, scissored)],
    )
    .unwrap();
    let image = device.read_target(target).unwrap();
    assert_eq!(pixel(&image, 3, 30), [0, 0, 255, 255]);
    assert_eq!(pixel(&image, 4, 30), [0, 0, 0, 255]);

    // An empty (fully clamped away) viewport still clears and draws nothing.
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = [0.0, 1.0, 0.0, 1.0];
    desc.viewport = Some(PixelRect::new(100, 100, 8, 8));
    let outcome = submit(
        &mut device,
        desc,
        vec![draw(program, whole, None, &IDENTITY, OPAQUE)],
    )
    .unwrap();
    assert!(matches!(outcome, FrameOutcome::Rendered(_)));
    let image = device.read_target(target).unwrap();
    assert_eq!(pixel(&image, 32, 32), [0, 255, 0, 255]);
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn culling_follows_counter_clockwise_fronts_as_seen_on_screen() {
    let (mut device, log) = open(2);
    let target = device.create_target(32, 32).unwrap();
    let program = shader(&mut device, TEXTURED);
    // Clip space, y up: (-0.8,-0.8) -> (0.8,-0.8) -> (0,0.8) turns counter-clockwise.
    let ccw_points = [[-0.8, -0.8], [0.8, -0.8], [0.0, 0.8]];
    let cw_points = [[-0.8, -0.8], [0.0, 0.8], [0.8, -0.8]];
    let ccw = device
        .create_mesh(&triangle(ccw_points, 0.5, [1.0, 0.0, 0.0, 1.0]))
        .unwrap();
    let cw = device
        .create_mesh(&triangle(cw_points, 0.5, [1.0, 0.0, 0.0, 1.0]))
        .unwrap();
    let with = |cull| DrawState { cull, ..OPAQUE };
    let visible = |device: &mut Device, mesh, cull| {
        let image = render_and_read(
            device,
            target,
            BLACK,
            vec![draw(program, mesh, None, &IDENTITY, with(cull))],
        );
        // The triangle's centroid is at clip (0, -0.25), i.e. pixel (16, 20).
        pixel(&image, 16, 20) == [255, 0, 0, 255]
    };
    assert!(visible(&mut device, ccw, CullMode::None));
    assert!(visible(&mut device, cw, CullMode::None));
    assert!(
        visible(&mut device, ccw, CullMode::Back),
        "CCW is front: kept by Back"
    );
    assert!(
        !visible(&mut device, ccw, CullMode::Front),
        "CCW is front: culled by Front"
    );
    assert!(
        !visible(&mut device, cw, CullMode::Back),
        "CW is back: culled by Back"
    );
    assert!(
        visible(&mut device, cw, CullMode::Front),
        "CW is back: kept by Front"
    );
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn shaders_with_sparse_groups_and_several_uniform_buffers() {
    let (mut device, log) = open(2);
    let target = device.create_target(16, 16).unwrap();

    // Uses only group 2 (groups 0 and 1 exist as empty set layouts), two uniform
    // buffers whose bindings are declared out of order.
    let program = shader(
        &mut device,
        "struct Tint { value: vec4<f32> }\n\
         @group(2) @binding(3) var<uniform> second: Tint;\n\
         @group(2) @binding(1) var<uniform> first: Tint;\n\
         @vertex fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {\n\
             return vec4<f32>(position, 1.0);\n\
         }\n\
         @fragment fn fs_main() -> @location(0) vec4<f32> { return first.value + second.value; }",
    );
    let mesh = device
        .create_mesh(&quad(-1.0, 1.0, 1.0, -1.0, 0.5, [1.0; 4]))
        .unwrap();
    let tint = |a: f32, b: f32| {
        let mut command = DrawCommand::new(program, mesh);
        command.append_uniform("first", UniformSlot::new(2, 1), &[a, 0.0, 0.0, 1.0]);
        command.append_uniform("second", UniformSlot::new(2, 3), &[0.0, b, 0.0, 0.0]);
        command
    };
    let image = render_and_read(&mut device, target, BLACK, vec![tint(1.0, 1.0)]);
    assert_eq!(pixel(&image, 8, 8), [255, 255, 0, 255]);
    let image = render_and_read(&mut device, target, BLACK, vec![tint(0.0, 1.0)]);
    assert_eq!(pixel(&image, 8, 8), [0, 255, 0, 255]);

    // Many draws with identical vs. changing uniforms: batches count rebinds.
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = BLACK;
    let outcome = submit(
        &mut device,
        desc,
        vec![tint(1.0, 0.0), tint(1.0, 0.0), tint(1.0, 0.0)],
    )
    .unwrap();
    assert_eq!(
        outcome,
        FrameOutcome::Rendered(FrameStats {
            draws: 3,
            batches: 1
        }),
        "identical consecutive draws bind once"
    );
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = BLACK;
    let outcome = submit(
        &mut device,
        desc,
        vec![
            tint(1.0, 0.0),
            tint(0.5, 0.0),
            tint(0.5, 0.0),
            tint(1.0, 0.0),
        ],
    )
    .unwrap();
    assert_eq!(
        outcome,
        FrameOutcome::Rendered(FrameStats {
            draws: 4,
            batches: 3
        })
    );
    // The last draw's uniforms are the visible ones.
    assert_eq!(
        pixel(&device.read_target(target).unwrap(), 4, 4),
        [255, 0, 0, 255]
    );

    // A shader that uses only the texture group, no uniforms at all.
    let textured_only = shader(
        &mut device,
        "@group(1) @binding(0) var image: texture_2d<f32>;\n\
         @group(1) @binding(1) var smp: sampler;\n\
         struct V { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }\n\
         @vertex fn vs_main(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>) -> V {\n\
             var out: V; out.position = vec4<f32>(position, 1.0); out.uv = uv; return out;\n\
         }\n\
         @fragment fn fs_main(in: V) -> @location(0) vec4<f32> {\n\
             return textureSample(image, smp, in.uv);\n\
         }",
    );
    let blue = device
        .create_texture(&solid_texture([0, 0, 255, 255]))
        .unwrap();
    let image = render_and_read(
        &mut device,
        target,
        BLACK,
        vec![DrawCommand::new(textured_only, mesh).with_texture(blue)],
    );
    assert_eq!(pixel(&image, 8, 8), [0, 0, 255, 255]);
    // Without a texture the same shader samples the white default.
    let image = render_and_read(
        &mut device,
        target,
        BLACK,
        vec![DrawCommand::new(textured_only, mesh)],
    );
    assert_eq!(pixel(&image, 8, 8), [255, 255, 255, 255]);
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn uniform_arena_and_descriptor_pools_grow_within_one_frame() {
    let (mut device, log) = open(2);
    let target = device.create_target(48, 48).unwrap();
    let program = shader(&mut device, TEXTURED);
    // 144 cells of 4x4 pixels; 144 distinct textures force more than one
    // descriptor pool, and 3 passes x 144 distinct camera blocks (256 bytes
    // apart) force the arena past its 64 KiB starting size.
    const CELLS: u32 = 144;
    let colors: Vec<[u8; 4]> = (0..CELLS)
        .map(|i| {
            [
                (i * 7 % 256) as u8,
                (i * 13 % 256) as u8,
                (i * 29 % 256) as u8,
                255,
            ]
        })
        .collect();
    let textures: Vec<TextureHandle> = colors
        .iter()
        .map(|c| device.create_texture(&solid_texture(*c)).unwrap())
        .collect();
    let mesh = device
        .create_mesh(&quad(0.0, 0.0, 4.0, 4.0, 0.5, [1.0; 4]))
        .unwrap();
    let mut draws = Vec::new();
    for pass in 0..3u32 {
        for cell in 0..CELLS {
            let (cx, cy) = ((cell % 12) as f32 * 4.0, (cell / 12) as f32 * 4.0);
            let mut camera = pixel_camera(48.0, 48.0);
            // Translate the unit quad to its cell (x then y), in pixel space.
            camera[12] += 2.0 * cx / 48.0;
            camera[13] -= 2.0 * cy / 48.0;
            let texture = textures[((cell + pass) % CELLS) as usize];
            draws.push(draw(program, mesh, Some(texture), &camera, OPAQUE));
        }
    }
    let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
    desc.clear_color = BLACK;
    let outcome = submit(&mut device, desc, draws).unwrap();
    let FrameOutcome::Rendered(stats) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(stats.draws, 3 * CELLS as usize);
    let image = device.read_target(target).unwrap();
    for cell in 0..CELLS {
        let (x, y) = ((cell % 12) * 4 + 2, (cell / 12) * 4 + 2);
        // Pass 2 drew last: cell i shows texture (i + 2) % CELLS.
        let expected = colors[((cell + 2) % CELLS) as usize];
        assert_eq!(pixel(&image, x, y), expected, "cell {cell}");
    }
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn pipelines_are_cached_per_state_layout_and_destroyed_with_their_shader() {
    let (mut device, log) = open(2);
    let target = device.create_target(16, 16).unwrap();
    let program = shader(&mut device, TEXTURED);
    let mesh = device
        .create_mesh(&quad(0.0, 0.0, 16.0, 16.0, 0.5, [1.0; 4]))
        .unwrap();
    let camera = pixel_camera(16.0, 16.0);
    let pipelines = |device: &Device| device.backend().stats().unwrap().pipelines;
    assert_eq!(pipelines(&device), 0);
    render_and_read(
        &mut device,
        target,
        BLACK,
        vec![draw(program, mesh, None, &camera, OPAQUE)],
    );
    assert_eq!(pipelines(&device), 1);
    let mut clipped = OPAQUE;
    clipped.scissor = Some(PixelRect::new(0, 0, 4, 4));
    render_and_read(
        &mut device,
        target,
        BLACK,
        vec![draw(program, mesh, None, &camera, clipped)],
    );
    assert_eq!(pipelines(&device), 1, "scissor is dynamic state");
    render_and_read(
        &mut device,
        target,
        BLACK,
        vec![draw(
            program,
            mesh,
            None,
            &camera,
            state(BlendMode::StraightAlpha, true, true),
        )],
    );
    assert_eq!(pipelines(&device), 2);
    // A different vertex layout needs its own pipeline.
    let narrow_layout = VertexLayout::new(
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
            VertexAttribute {
                location: 5,
                format: VertexFormat::Float32,
                offset: 32,
            },
        ],
    );
    let other = device
        .create_mesh(&MeshResource::new(vec![0; 36 * 3], narrow_layout, vec![]).unwrap())
        .unwrap();
    render_and_read(
        &mut device,
        target,
        BLACK,
        vec![draw(program, other, None, &camera, OPAQUE)],
    );
    assert_eq!(pipelines(&device), 3);

    device.destroy(program).unwrap();
    device.wait_idle().unwrap();
    assert_eq!(pipelines(&device), 0, "pipelines go with their shader");
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn updating_and_destroying_resources_while_frames_are_in_flight() {
    let (mut device, log) = open(2);
    let target = device.create_target(32, 32).unwrap();
    let program = shader(&mut device, TEXTURED);
    let camera = pixel_camera(32.0, 32.0);
    let mut mesh = device
        .create_mesh(&quad(0.0, 0.0, 32.0, 32.0, 0.5, [1.0, 0.0, 0.0, 1.0]))
        .unwrap();
    let texture = device
        .create_texture(&solid_texture([255, 255, 255, 255]))
        .unwrap();
    let mut last_expected = [0u8; 4];
    let mut retired_meshes = Vec::new();

    // Submit without waiting so frames overlap; update and recreate resources in between.
    for frame in 0..40u32 {
        let shade = (frame * 6 % 250) as u8;
        let color = [
            f32::from(shade) / 255.0,
            0.0,
            1.0 - f32::from(shade) / 255.0,
            1.0,
        ];
        device
            .update_mesh(mesh, &quad(0.0, 0.0, 32.0, 32.0, 0.5, color))
            .unwrap();
        // Texture contents (and dimensions) change every few frames.
        let texel = [255 - shade, 255, 255, 255];
        let size = 1 + frame % 3;
        let data: Vec<u8> = (0..size * size).flat_map(|_| texel).collect();
        device
            .update_texture(texture, &Texture2D::new(size, size, data).unwrap())
            .unwrap();
        let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
        desc.clear_color = BLACK;
        let outcome = submit(
            &mut device,
            desc,
            vec![draw(program, mesh, Some(texture), &camera, OPAQUE)],
        )
        .unwrap();
        assert!(matches!(outcome, FrameOutcome::Rendered(_)));
        // Mesh churn: create, then retire after a few frames.
        let extra = device
            .create_mesh(&quad(0.0, 0.0, 4.0, 4.0, 0.5, [0.0, 1.0, 0.0, 1.0]))
            .unwrap();
        retired_meshes.push(extra);
        if retired_meshes.len() > 3 {
            device.destroy(retired_meshes.remove(0)).unwrap();
        }
        if frame % 7 == 0 {
            // Replace the whole mesh object mid-flight.
            let replacement = device
                .create_mesh(&quad(0.0, 0.0, 32.0, 32.0, 0.5, color))
                .unwrap();
            device.destroy(mesh).unwrap();
            mesh = replacement;
        }
        last_expected = {
            // Linear color shade/255 and blue (1 - shade/255) times the texel, then sRGB-encoded
            // by the target: compare with a tolerance below.
            let to_srgb = |v: f32| {
                let c = v.clamp(0.0, 1.0);
                let e = if c <= 0.003_130_8 {
                    c * 12.92
                } else {
                    1.055 * c.powf(1.0 / 2.4) - 0.055
                };
                (e * 255.0 + 0.5) as u8
            };
            let linear_texel = srgb_to_linear(255 - shade);
            [to_srgb(color[0] * linear_texel), 0, to_srgb(color[2]), 255]
        };
    }
    assert_eq!(device.frames_submitted(), 40);
    let image = device.read_target(target).unwrap();
    let got = pixel(&image, 16, 16);
    for channel in 0..4 {
        assert!(
            got[channel].abs_diff(last_expected[channel]) <= 1,
            "final frame {got:?} vs {last_expected:?}"
        );
    }
    device.wait_idle().unwrap();
    assert_eq!(device.counts().pending_destruction, 0);
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn destroying_everything_then_shutting_down_is_clean_and_idempotent() {
    let (mut device, log) = open(3);
    let target = device.create_target(16, 16).unwrap();
    let program = shader(&mut device, TEXTURED);
    let mesh = device
        .create_mesh(&quad(0.0, 0.0, 16.0, 16.0, 0.5, [1.0; 4]))
        .unwrap();
    let texture = device
        .create_texture(&solid_texture([9, 9, 9, 255]))
        .unwrap();
    let camera = pixel_camera(16.0, 16.0);
    for _ in 0..5 {
        let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
        desc.clear_color = BLACK;
        submit(
            &mut device,
            desc,
            vec![draw(program, mesh, Some(texture), &camera, OPAQUE)],
        )
        .unwrap();
    }
    // Destroy while the last frames may still run, then keep rendering.
    device.destroy(texture).unwrap();
    device.destroy(mesh).unwrap();
    let mesh2 = device
        .create_mesh(&quad(0.0, 0.0, 16.0, 16.0, 0.5, [1.0; 4]))
        .unwrap();
    submit(
        &mut device,
        FrameDesc::new(RenderTarget::Offscreen(target)),
        vec![draw(program, mesh2, None, &camera, OPAQUE)],
    )
    .unwrap();
    device.shutdown().unwrap();
    device.shutdown().unwrap();
    assert!(matches!(
        device.create_target(8, 8),
        Err(RenderError::NotReady(_))
    ));
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn dropping_the_device_without_shutdown_does_not_leak_or_crash() {
    let (mut device, log) = open(2);
    let target = device.create_target(16, 16).unwrap();
    let program = shader(&mut device, TEXTURED);
    let mesh = device
        .create_mesh(&quad(0.0, 0.0, 16.0, 16.0, 0.5, [1.0; 4]))
        .unwrap();
    let camera = pixel_camera(16.0, 16.0);
    for _ in 0..3 {
        submit(
            &mut device,
            FrameDesc::new(RenderTarget::Offscreen(target)),
            vec![draw(program, mesh, None, &camera, OPAQUE)],
        )
        .unwrap();
    }
    drop(device);
    assert_no_validation_errors(&log);

    // A raw backend dropped after initialize but before any shutdown call.
    let mut backend =
        engine_render_wgpu::WgpuBackend::offscreen(engine_render_wgpu::WgpuOptions::default());
    backend.initialize().unwrap();
    backend.initialize().unwrap();
    assert!(backend.capabilities().backend_ready);
    drop(backend);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn offscreen_backend_rejects_surface_frames() {
    let (mut device, log) = open(2);
    let error = submit(&mut device, FrameDesc::new(RenderTarget::Surface), vec![]).unwrap_err();
    assert!(
        matches!(error, RenderError::Unsupported { .. }),
        "{error:?}"
    );
    let error = device.resize(64, 64).unwrap_err();
    assert!(
        matches!(error, RenderError::Unsupported { .. }),
        "{error:?}"
    );
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn packed_unorm_vertex_colors_feed_float_shader_inputs() {
    let (mut device, log) = open(2);
    let target = device.create_target(16, 16).unwrap();
    let program = shader(&mut device, TEXTURED);
    let layout = VertexLayout::new(
        24,
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
                format: VertexFormat::Unorm8x4,
                offset: 20,
            },
        ],
    );
    let mut bytes = Vec::new();
    for position in [[-1.0f32, 1.0], [1.0, 1.0], [-1.0, -1.0], [1.0, -1.0]] {
        for value in [position[0], position[1], 0.5, 0.0, 0.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&[255, 128, 0, 255]);
    }
    let mesh = device
        .create_mesh(&MeshResource::new(bytes, layout, vec![0, 1, 2, 2, 1, 3]).unwrap())
        .unwrap();
    let image = render_and_read(
        &mut device,
        target,
        BLACK,
        vec![draw(program, mesh, None, &IDENTITY, OPAQUE)],
    );
    let color = pixel(&image, 8, 8);
    assert_eq!(color[0], 255);
    assert!((187..=188).contains(&color[1]), "{color:?}");
    assert_eq!(&color[2..], &[0, 255]);
    assert_no_validation_errors(&log);
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn frame_rings_of_one_and_eight_render_correctly_under_load() {
    for frames in [1u32, 8] {
        let (mut device, log) = open(frames);
        assert_eq!(device.capabilities().max_frames_in_flight, frames);
        let target = device.create_target(32, 32).unwrap();
        let program = shader(&mut device, TEXTURED);
        let camera = pixel_camera(32.0, 32.0);
        let mesh = device
            .create_mesh(&quad(0.0, 0.0, 32.0, 32.0, 0.5, [0.0, 0.0, 1.0, 1.0]))
            .unwrap();
        for _ in 0..(frames * 4 + 3) {
            let mut desc = FrameDesc::new(RenderTarget::Offscreen(target));
            desc.clear_color = BLACK;
            submit(
                &mut device,
                desc,
                vec![draw(program, mesh, None, &camera, OPAQUE)],
            )
            .unwrap();
        }
        assert_eq!(
            pixel(&device.read_target(target).unwrap(), 16, 16),
            [0, 0, 255, 255],
            "{frames} frames in flight"
        );
        assert_no_validation_errors(&log);
    }
}
