//! GPU tests of backend selection, shared contexts and external target views.
//! Ignored in ordinary runs: `cargo test -p engine-render-wgpu -- --ignored`.

mod common;

use common::*;
use engine_render::*;
use engine_render_wgpu::{GpuContext, OffscreenTarget, WgpuBackend, WgpuBackends, WgpuOptions};

#[test]
#[ignore = "requires a real wgpu adapter"]
fn explicit_backend_selection_picks_that_graphics_api() {
    for (backends, label) in [
        (WgpuBackends::Vulkan, "wgpu/vulkan"),
        (WgpuBackends::Dx12, "wgpu/dx12"),
    ] {
        let backend = WgpuBackend::offscreen(WgpuOptions {
            backends,
            ..Default::default()
        });
        match RenderDevice::new(backend, DeviceConfig::default()) {
            Ok(device) => assert_eq!(device.capabilities().backend_name, label),
            Err(RenderError::Unavailable(message)) => {
                // An API that is not installed must say so, not crash.
                eprintln!("{label} unavailable here: {message}");
                assert!(message.contains("adapter"), "{message}");
            }
            Err(other) => panic!("unexpected error {other:?}"),
        }
    }
}

#[test]
#[ignore = "requires a real wgpu adapter"]
fn a_backend_can_share_a_context_and_render_into_a_foreign_view() {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default()).unwrap();
    let target = OffscreenTarget::new(&gpu, 64, 64).unwrap();
    let mut device = RenderDevice::new(
        WgpuBackend::from_context(gpu.clone()),
        DeviceConfig::default(),
    )
    .unwrap();
    // A context-sharing backend has no swapchain; its Surface target is the
    // host-supplied external view.
    assert!(device.capabilities().surface_presentation);
    assert!(
        device.resize(32, 32).is_err(),
        "there is no surface to resize"
    );
    assert!(matches!(
        submit(&mut device, FrameDesc::new(RenderTarget::Surface), vec![]),
        Err(RenderError::Unsupported { .. })
    ));
    let program = shader(&mut device, TEXTURED);
    let mesh = device
        .create_mesh(&quad(16.0, 16.0, 48.0, 48.0, 0.5, [1.0, 0.0, 0.0, 1.0]))
        .unwrap();
    let camera = pixel_camera(64.0, 64.0);
    let draws = vec![draw(program, mesh, None, &camera, DrawState::default())];
    let mut desc = FrameDesc::new(RenderTarget::Surface);
    desc.clear_color = BLACK;
    let stats = device
        .backend_mut()
        .render_to_view(&target.view, target.info, &desc, &draws)
        .unwrap();
    assert_eq!(stats.draws, 1);
    let pixels = target.read_rgba(&gpu).unwrap();
    let at = |x: usize, y: usize| -> [u8; 4] {
        pixels[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4]
            .try_into()
            .unwrap()
    };
    assert_eq!(at(32, 32), [255, 0, 0, 255]);
    assert_eq!(at(4, 4), [0, 0, 0, 255]);

    // The same frame through the device: Surface draws into the external view.
    device
        .backend_mut()
        .set_external_target(target.view.clone(), target.info);
    let mut desc = FrameDesc::new(RenderTarget::Surface);
    desc.clear_color = [0.0, 0.0, 1.0, 1.0];
    let draws = vec![draw(program, mesh, None, &camera, DrawState::default())];
    let outcome = submit(&mut device, desc, draws).unwrap();
    assert!(matches!(outcome, FrameOutcome::Rendered(_)), "{outcome:?}");
    let pixels = target.read_rgba(&gpu).unwrap();
    assert_eq!(pixels[(32 * 64 + 32) * 4..][..4], [255, 0, 0, 255]);
    assert_eq!(pixels[(4 * 64 + 4) * 4..][..4], [0, 0, 255, 255]);
    assert_eq!(device.frames_submitted(), 1);
    // The shared context reports no errors and both handles stay usable.
    gpu.wait_idle().unwrap();
}
