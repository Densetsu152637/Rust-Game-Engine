//! Tier selection, self-test gating and honest fallbacks on real hardware.
//! Ignored by default; run with
//! `cargo test -p engine-render-rt --test gpu_tiers -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_render_rt::{
    ProbeRay, RayTracingTier, RtDevice, RtError, RtFeatureRequest, RtInitOptions,
    RtUnavailableCause, StandaloneOptions, probe_adapters,
};

fn vulkan_adapter() -> (wgpu::Instance, wgpu::Adapter) {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(descriptor);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .expect("Vulkan adapter");
    (instance, adapter)
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn adapters_report_eligibility_with_reasons() {
    let adapters = probe_adapters(wgpu::Backends::PRIMARY);
    for a in &adapters {
        println!(
            "adapter {:?} {:?} {:?} ray_query={} reason={:?}",
            a.name, a.backend, a.device_type, a.ray_query, a.reason
        );
        assert_eq!(a.ray_query, a.reason.is_none());
    }
    assert!(adapters.iter().any(|a| a.ray_query), "no eligible adapter");
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn hardware_tier_passes_its_self_test() {
    let start = std::time::Instant::now();
    let rt = open();
    let status = rt.status();
    let report = rt.self_test_report();
    println!("status: {status}");
    println!(
        "self-test ({:.1} ms incl. device creation): {report:?}",
        start.elapsed().as_secs_f64() * 1e3
    );
    assert_eq!(status.tier, RayTracingTier::Hardware);
    assert!(status.adapter.as_deref().unwrap().contains("Vulkan"));
    assert!(report.expected_hits > 300 && report.expected_misses > 200);
    assert_eq!(report.matched_hits, report.expected_hits);
    assert!(report.mask_rays > 100 && report.back_face_hits > 0);
    assert!(report.max_t_error <= 1e-4 * 20.0);
    assert!(report.max_barycentric_error <= 1e-4);
    assert!(report.max_normal_error <= 1e-4);
}

#[test]
#[ignore = "requires a DX12 adapter (Windows)"]
fn dx12_reports_why_it_is_not_used() {
    let status = RtDevice::open_standalone(&StandaloneOptions {
        backends: Some(wgpu::Backends::DX12),
        ..Default::default()
    })
    .unwrap_err();
    println!("DX12: {status}");
    assert_eq!(status.tier, RayTracingTier::Unavailable);
    assert_eq!(status.cause, Some(RtUnavailableCause::UnsupportedBackend));
    assert!(status.reason.unwrap().contains("validated on Vulkan only"));
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn forced_self_test_failure_reports_unavailable() {
    let status = RtDevice::open_standalone(&StandaloneOptions {
        init: RtInitOptions {
            force_self_test_failure: true,
        },
        ..Default::default()
    })
    .unwrap_err();
    println!("forced: {status}");
    assert_eq!(status.cause, Some(RtUnavailableCause::SelfTestFailed));
    assert!(status.reason.unwrap().contains("test hook"));
}

#[test]
#[ignore = "requires a Vulkan adapter"]
fn a_device_without_the_request_is_refused() {
    let (_instance, adapter) = vulkan_adapter();
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("plain device");
    let status = RtDevice::new(&adapter, &device, &queue).unwrap_err();
    println!("plain device: {status}");
    assert_eq!(status.cause, Some(RtUnavailableCause::DeviceMissingFeature));
    assert!(status.reason.unwrap().contains("EXPERIMENTAL_RAY_QUERY"));
}

/// The renderer path: merge the request into the renderer's own descriptor,
/// verify, then raster and trace on the same device.
#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn merged_renderer_device_rasters_and_traces() {
    let (_instance, adapter) = vulkan_adapter();
    let request = RtFeatureRequest::for_adapter(&adapter).expect("eligible adapter");
    assert!(request.is_hardware());
    // As engine-render-wgpu's GpuContext::device_descriptor builds it.
    let mut descriptor = wgpu::DeviceDescriptor {
        label: Some("engine desktop device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    };
    request.apply(&mut descriptor);
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor)).expect("device");
    let rt = RtDevice::new(&adapter, &device, &queue).expect("self-test");
    assert!(rt.status().is_hardware());

    // Raster: clear a target to a known colour.
    let target = texture(
        &rt,
        wgpu::TextureFormat::Rgba8Unorm,
        64,
        64,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let view = target.create_view(&Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 1.0,
                    g: 0.5,
                    b: 0.0,
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    submit(&rt, encoder);
    let pixels = read_texture(&rt, &target, 4);
    println!("raster clear pixel: {:?}", &pixels[..4]);
    assert_eq!(pixels[0], 255);
    assert!(pixels[1].abs_diff(128) <= 1, "{:?}", &pixels[..4]);
    assert_eq!((pixels[2], pixels[3]), (0, 255));

    // Trace on the same device.
    let mut scene = scene(&rt);
    let cube = cuboid([-1.0; 3], [1.0; 3]);
    let mesh = scene.add_mesh(&cube.data()).unwrap();
    scene
        .add_instance(&instance(mesh, translation(0.0, 0.0, 5.0)))
        .unwrap();
    scene.commit().unwrap();
    let hits = rt
        .cast_rays(
            &scene,
            &[ProbeRay::new([0.0; 3], [0.0, 0.0, 1.0], 0.0, 100.0, 0xff)],
        )
        .unwrap();
    println!("merged device hit: t = {}", hits[0].t);
    assert!(hits[0].hit() && (hits[0].t - 4.0).abs() < 1e-4);
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn reported_device_loss_demotes_the_status() {
    let rt = open();
    let mut scene = scene(&rt);
    rt.report_device_lost("simulated by the test");
    let status = rt.status();
    println!("after loss: {status}");
    assert_eq!(status.cause, Some(RtUnavailableCause::DeviceLost));
    let cube = cuboid([-1.0; 3], [1.0; 3]);
    scene.add_mesh(&cube.data()).unwrap();
    assert!(matches!(scene.commit(), Err(RtError::Unavailable(_))));
}
