//! Presents to a real (hidden) winit window through `RenderDevice::resize`.
//! Needs a Vulkan device and a desktop session; ignored in ordinary runs:
//! `cargo test -p engine-vulkan --test gpu_window -- --ignored --nocapture`.
//!
//! Windows only: winit needs `with_any_thread` to create its event loop on the
//! test thread, which is a Windows-specific builder extension.
#![cfg(windows)]

mod common;

use common::*;
use engine_render::*;
use engine_vulkan::{VulkanBackend, VulkanConfig};
use std::any::Any;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::{Window, WindowId};

struct App {
    outcome: Option<Result<(), Box<dyn Any + Send>>>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.outcome.is_some() {
            return;
        }
        let window = event_loop
            .create_window(
                Window::default_attributes()
                    .with_visible(false)
                    .with_inner_size(PhysicalSize::new(160u32, 120u32)),
            )
            .expect("hidden window");
        let window = Arc::new(window);
        self.outcome = Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || {
                exercise(window);
            },
        )));
        event_loop.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn render_surface_frames(
    device: &mut Device,
    program: ShaderHandle,
    mesh: MeshHandle,
    count: u32,
) -> u32 {
    let mut rendered = 0;
    for frame in 0..count {
        let mut desc = FrameDesc::new(RenderTarget::Surface);
        let shade = f32::from((frame * 40 % 256) as u8) / 255.0;
        desc.clear_color = [shade, 0.1, 0.3, 1.0];
        let outcome = submit(
            device,
            desc,
            vec![draw(
                program,
                mesh,
                None,
                &IDENTITY,
                state(BlendMode::Opaque, true, true),
            )],
        )
        .expect("surface frame");
        match outcome {
            FrameOutcome::Rendered(stats) => {
                assert_eq!(stats.draws, 1);
                rendered += 1;
            }
            // Acceptable while a resize is being absorbed.
            FrameOutcome::Skipped(FrameSkip::OutOfDate | FrameSkip::Timeout) => {}
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    rendered
}

fn exercise(window: Arc<Window>) {
    let size = window.inner_size();
    let backend = VulkanBackend::for_window(
        window.clone(),
        (size.width, size.height),
        VulkanConfig {
            application_name: "engine-vulkan-window-test".into(),
            enable_validation: true,
            frames_in_flight: 2,
        },
    );
    let log = backend.validation_log();
    let mut device = RenderDevice::new(
        backend,
        DeviceConfig {
            frames_in_flight: 2,
        },
    )
    .expect("window device");
    let stats = device.backend().stats().unwrap();
    eprintln!(
        "adapter: {} ({}), swapchain {:?}, validation layer {}",
        stats.adapter,
        stats.device_type,
        stats.surface_extent,
        if log.is_active() {
            "ACTIVE"
        } else {
            "not installed/inactive"
        }
    );
    assert!(device.capabilities().surface_presentation);
    assert_eq!(stats.surface_extent, Some((size.width, size.height)));

    let program = shader(&mut device, TEXTURED);
    let mesh = device
        .create_mesh(&quad(-0.5, 0.5, 0.5, -0.5, 0.5, [1.0, 0.5, 0.0, 1.0]))
        .unwrap();

    // Plain presentation.
    assert!(render_surface_frames(&mut device, program, mesh, 6) >= 5);

    // Offscreen and surface frames share the frame ring.
    let offscreen = device.create_target(16, 16).unwrap();
    for _ in 0..3 {
        let image = render_and_read(&mut device, offscreen, [0.0, 1.0, 0.0, 1.0], vec![]);
        assert_eq!(pixel(&image, 5, 5), [0, 255, 0, 255]);
        render_surface_frames(&mut device, program, mesh, 1);
    }

    // Resize through the device: the swapchain follows.
    let _ = window.request_inner_size(PhysicalSize::new(220u32, 140u32));
    let resized = window.inner_size();
    device.resize(resized.width, resized.height).unwrap();
    assert_eq!(
        device.backend().stats().unwrap().surface_extent,
        Some((resized.width, resized.height))
    );
    assert!(render_surface_frames(&mut device, program, mesh, 6) >= 5);

    // Resize the window without telling the device: out-of-date / suboptimal
    // handling recreates the swapchain on its own.
    let _ = window.request_inner_size(PhysicalSize::new(100u32, 80u32));
    let shrunk = window.inner_size();
    render_surface_frames(&mut device, program, mesh, 8);
    assert_eq!(
        device.backend().stats().unwrap().surface_extent,
        Some((shrunk.width, shrunk.height)),
        "swapchain must follow the window"
    );

    // Resizing a few times in a row, interleaved with frames, stays healthy.
    for (w, h) in [(300u32, 200u32), (64, 64), (180, 90)] {
        let _ = window.request_inner_size(PhysicalSize::new(w, h));
        let now = window.inner_size();
        device.resize(now.width, now.height).unwrap();
        assert!(render_surface_frames(&mut device, program, mesh, 3) >= 2);
    }

    // The device rejects zero-size surfaces before the backend sees them.
    assert!(matches!(
        device.resize(0, 10),
        Err(RenderError::InvalidResource(_))
    ));

    device.wait_idle().unwrap();
    assert_no_validation_errors(&log);
    device.shutdown().unwrap();
}

#[test]
#[ignore = "requires a Vulkan device and a desktop session"]
fn presents_to_a_hidden_window_and_follows_resizes() {
    let mut event_loop = EventLoop::builder()
        .with_any_thread(true)
        .build()
        .expect("event loop");
    let mut app = App { outcome: None };
    event_loop.run_app_on_demand(&mut app).expect("event loop");
    match app.outcome.expect("the window test body ran") {
        Ok(()) => {}
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
