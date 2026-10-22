use engine_core::EngineApp;
use engine_desktop::{GpuContext, GpuRenderer, NativeRenderPasses, OffscreenTarget, RenderLimits};
use engine_project::{Camera2D, runtime::RenderFrame};
use native_extension_gameplay::*;

#[test]
fn external_heap_components_bundles_resources_sets_and_extraction() {
    let mut app = EngineApp::new();
    configure(&mut app).unwrap();
    app.tick(1.0 / 60.0).unwrap();
    let mut query = app.world_mut().query::<&CustomActor>();
    let actor = query.single(app.world()).unwrap();
    assert_eq!(actor.name, "external heap-owned actor");
    assert_eq!(actor.payload, vec![3, 1, 4, 1]);
    assert_eq!(actor.ticks, 1);
    assert_eq!(
        app.world().resource::<CustomState>().messages,
        vec!["external heap-owned actor:1:4"]
    );
    let count = app
        .render_world()
        .iter_entities()
        .filter(|e| e.contains::<CustomActor>() && e.contains::<CustomColor>())
        .count();
    assert_eq!(count, 1);
    app.shutdown().unwrap();
}

#[test]
#[ignore = "requires a real GPU adapter; run explicitly --ignored --nocapture"]
fn external_custom_pass_verifies_real_offscreen_pixels() {
    let mut app = EngineApp::new();
    configure(&mut app).unwrap();
    app.tick(1.0 / 60.0).unwrap();
    let gpu = pollster::block_on(GpuContext::headless()).expect("real GPU adapter");
    let target = OffscreenTarget::new(&gpu, 64, 64).unwrap();
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default()).unwrap();
    let passes = app
        .world_mut()
        .remove_resource::<NativeRenderPasses>()
        .unwrap();
    renderer.set_passes(passes).unwrap();
    let frame = RenderFrame {
        camera: Camera2D::default(),
        draws: vec![],
    };
    renderer
        .render(&frame, &target.view, Some(app.render_world()))
        .unwrap();
    let pixels = target.read_rgba(&renderer.gpu).unwrap();
    let at = |x: usize, y: usize| &pixels[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
    assert_eq!(at(32, 32), &[0, 255, 0, 255]);
    assert_ne!(at(1, 1), &[0, 255, 0, 255]);
    // Change a game-defined component, extract it and verify the custom GPU pass
    // sees the new value. This is not a fixed engine primitive or CPU mock.
    let mut query = app.world_mut().query::<&mut CustomColor>();
    for mut color in query.iter_mut(app.world_mut()) {
        color.0 = [1.0, 0.0, 0.0, 1.0];
    }
    app.tick(1.0 / 60.0).unwrap();
    renderer
        .render(&frame, &target.view, Some(app.render_world()))
        .unwrap();
    let pixels = target.read_rgba(&renderer.gpu).unwrap();
    assert_eq!(
        &pixels[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 4],
        &[255, 0, 0, 255]
    );
    println!(
        "NATIVE_EXTENSION_GPU_PASS: external WGSL triangle green-to-red verified by actual offscreen readback"
    );
    app.shutdown().unwrap();
}
