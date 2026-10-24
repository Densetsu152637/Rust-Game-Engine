//! Tests against the OpenXR runtime installed on this machine. Ignored by
//! default; see docs/xr.md "Manual headset test" for the exact steps.
use engine_project::xr::{XrHapticPulse, XrHapticTarget};
use engine_xr::{
    Eye, LoaderMode, MirrorTarget, SessionPhase, TestPattern, XrAppInfo, XrRuntime, XrSession,
    XrStatus, manifest::DiscoveryInputs, probe,
};
use std::time::{Duration, Instant};

fn app() -> XrAppInfo {
    XrAppInfo {
        application_name: "engine-xr test".into(),
        application_version: 1,
    }
}

#[test]
#[ignore = "uses the installed OpenXR runtime; run with no headset connected"]
fn installed_runtime_without_a_headset_reports_an_honest_status() {
    let inputs = DiscoveryInputs::from_environment();
    eprintln!("discovery inputs: {inputs:?}");
    let status = probe(&inputs, &LoaderMode::BuiltIn, &app());
    eprintln!("observed XR status: {} -> {status}", status.state());
    assert!(
        !status.headset_available(),
        "a headset is available: {status}"
    );
    match &status {
        XrStatus::NoHeadset { runtime, result } => {
            assert!(!runtime.is_empty());
            assert!(result.into_raw() < 0, "{result:?}");
        }
        XrStatus::InstanceCreationFailed { runtime, result } => {
            assert!(!runtime.is_empty());
            assert!(result.into_raw() < 0, "{result:?}");
        }
        other => panic!("an installed runtime should load and reach instance creation: {other}"),
    }
}

/// Frames rendered with the test pattern once the session is focused.
const FRAMES: u64 = 180;

#[test]
#[ignore = "requires a connected, worn headset and its OpenXR runtime; see docs/xr.md"]
fn headset_session_renders_tracks_and_vibrates() {
    let inputs = DiscoveryInputs::from_environment();
    let instance = XrRuntime::load(&inputs, &LoaderMode::BuiltIn)
        .and_then(|runtime| runtime.create_instance(&app()))
        .expect("runtime and instance");
    let system = instance.hmd_system().expect("headset system");
    eprintln!("system: {system:?}");
    let mut session = XrSession::create(instance, system, Default::default()).expect("XR session");
    eprintln!("session notes: {:?}", session.notes());
    let mut pattern = TestPattern::new(session.device());
    let mirror_texture = session.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("mirror"),
        size: wgpu::Extent3d {
            width: 640,
            height: 360,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let mirror_view = mirror_texture.create_view(&Default::default());
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut rendered = 0;
    let mut tracked_head = false;
    while rendered < FRAMES {
        assert!(
            Instant::now() < deadline,
            "timed out: {} after {rendered} frames",
            session.status()
        );
        session.poll_events().expect("events");
        assert!(
            !session.is_finished(),
            "session ended early: {}",
            session.status()
        );
        let report = session
            .frame(
                &mut pattern,
                Some(MirrorTarget {
                    eye: Eye::Left,
                    view: &mirror_view,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                }),
            )
            .expect("frame");
        if !report.submitted {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        if session.lifecycle().phase() == SessionPhase::Focused && report.rendered {
            rendered += 1;
            let head = session.tracking().head;
            tracked_head |= head.tracked && head.position != [0.0; 3];
            assert_eq!(report.views.len(), 2);
            assert_ne!(
                report.views[0].pose, report.views[1].pose,
                "eyes must differ"
            );
        }
    }
    assert!(pattern.frames() >= FRAMES);
    assert!(
        tracked_head,
        "head pose stayed identity: {:?}",
        session.tracking().head
    );
    eprintln!("tracking: {:?}", session.tracking());
    for hand in [XrHapticTarget::Left, XrHapticTarget::Right] {
        session
            .apply_haptic(&XrHapticPulse {
                hand,
                amplitude: 0.8,
                duration_ms: 200.0,
                frequency_hz: 0.0,
            })
            .expect("haptic pulse");
    }
    session.request_exit().expect("exit request");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !session.is_finished() && Instant::now() < deadline {
        session.poll_events().expect("events");
        session
            .frame(&mut pattern, None)
            .expect("frame while stopping");
    }
    assert!(session.is_finished(), "{}", session.status());
}
