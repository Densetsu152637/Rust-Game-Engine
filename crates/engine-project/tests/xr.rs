use bevy_ecs::prelude::World;
use engine_project::{input::*, xr::*, *};
use serde_json::{Value, json};
use std::fs;

fn catalog() -> ComponentCatalog {
    ComponentCatalog::new(vec![]).unwrap()
}
fn check(data: Value) -> engine_project::Result<()> {
    catalog().validate(XR_SETTINGS_COMPONENT, &CustomValue { version: 1, data })
}

#[test]
fn settings_schema_accepts_defaults_and_rejects_unknown_fields_and_ranges() {
    let catalog = catalog();
    let descriptor = &catalog.0[XR_SETTINGS_COMPONENT];
    assert_eq!(descriptor.requires, vec!["Transform".to_owned()]);
    assert_eq!(descriptor.schema["additionalProperties"], false);
    assert_eq!(descriptor.defaults["render_scale"], 1.0);
    assert_eq!(descriptor.defaults["reference_space"], "stage");
    for property in descriptor.schema["properties"]
        .as_object()
        .unwrap()
        .values()
    {
        assert!(property["description"].is_string(), "{property}");
    }
    check(descriptor.defaults.clone()).unwrap();
    check(json!({})).unwrap();
    check(json!({"reference_space":"local","mirror":"none","render_scale":0.25})).unwrap();
    for (bad, why) in [
        (json!({"fov": 90}), "Unknown field fov"),
        (json!({"haptics":{"strength":1}}), "Unknown field strength"),
        (json!({"render_scale": 2.5}), "outside bounds"),
        (json!({"render_scale": 0.1}), "outside bounds"),
        (json!({"reference_space": "room"}), "not in enum"),
        (json!({"mirror": "both"}), "not in enum"),
        (json!({"haptics":{"amplitude": 1.5}}), "outside bounds"),
        (
            json!({"near_clip": 1.0, "far_clip": 0.5}),
            "far_clip must exceed near_clip",
        ),
        (json!({"enabled": "yes"}), "incorrect type"),
    ] {
        let error = check(bad.clone()).unwrap_err().to_string();
        assert!(error.contains(why), "{bad}: {error}");
    }
    let parsed: XrSettings = serde_json::from_value(json!({"render_scale": 1.25})).unwrap();
    assert_eq!(parsed.render_scale, 1.25);
    assert_eq!(parsed.haptics, XrHapticDefaults::default());
    assert!(serde_json::from_value::<XrSettings>(json!({"extra": 1})).is_err());
}

#[test]
fn native_validation_matches_the_schema_ranges() {
    assert!(XrSettings::default().validate().is_ok());
    let bad = [
        XrSettings {
            render_scale: 2.01,
            ..Default::default()
        },
        XrSettings {
            near_clip: 0.0,
            ..Default::default()
        },
        XrSettings {
            trigger_deadzone: 0.96,
            ..Default::default()
        },
        XrSettings {
            thumbstick_deadzone: f32::NAN,
            ..Default::default()
        },
        XrSettings {
            haptics: XrHapticDefaults {
                frequency_hz: 1001.0,
                ..Default::default()
            },
            ..Default::default()
        },
    ];
    for settings in bad {
        assert!(settings.validate().is_err(), "{settings:?}");
    }
}

#[test]
fn xr_tokens_are_bindable_and_map_to_device_controls() {
    let registry = binding_registry();
    let descriptor = xr_device_descriptor(XR_TRIGGER_DEADZONE, XR_THUMBSTICK_DEADZONE);
    assert_eq!(descriptor.id, XR_DEVICE_ID);
    assert_eq!(descriptor.controls.len(), 16);
    let tokens = xr_binding_tokens();
    assert_eq!(tokens.len(), 20);
    for token in &tokens {
        assert!(registry.contains(token), "{token}");
        let binding = xr_token_binding(token).unwrap();
        assert!(
            descriptor.controls.contains_key(&binding.control),
            "{token}"
        );
    }
    assert_eq!(
        xr_token_binding("XR:LeftThumbstickYNegative"),
        Some(ControlBinding {
            device: "xr".into(),
            control: "LeftThumbstickY".into(),
            direction: AxisDirection::Negative,
        })
    );
    assert_eq!(
        xr_token_binding("XR:RightTrigger").unwrap().direction,
        AxisDirection::Positive
    );
    for bad in [
        "XR:Trigger",
        "XR:LeftThumbstickX",
        "XR:LeftTriggerPositive",
        "Key:A",
        "XR:MiddleGrip",
    ] {
        assert_eq!(xr_token_binding(bad), None, "{bad}");
    }
    let validation = validate_bindings(
        &[(
            "grab".to_owned(),
            vec!["XR:RightGrip".to_owned(), "Gamepad:A".to_owned()],
        )]
        .into(),
        true,
    );
    assert!(validation.errors.is_empty(), "{:?}", validation.errors);
    assert!(
        !validate_bindings(
            &[("x".to_owned(), vec!["XR:RightBogus".to_owned()])].into(),
            false
        )
        .errors
        .is_empty()
    );
}

#[test]
fn controller_state_samples_respect_deadzones_and_disconnects() {
    let mut devices = InputDevices::default();
    let feed = XrInputFeed::default();
    devices
        .register(XrInputSource::new(feed.clone(), &XrSettings::default()))
        .unwrap();
    devices.synchronize_physical_bindings(
        &[
            ("fire".to_owned(), vec!["XR:RightTrigger".to_owned()]),
            (
                "forward".to_owned(),
                vec!["XR:LeftThumbstickYPositive".to_owned()],
            ),
            (
                "back".to_owned(),
                vec!["XR:LeftThumbstickYNegative".to_owned()],
            ),
            ("jump".to_owned(), vec!["XR:RightPrimary".to_owned()]),
        ]
        .into(),
    );
    let frame = |devices: &mut InputDevices| devices.poll_and_merge(ActionFrame::default());
    feed.set(XrControllerState {
        connected: true,
        ..Default::default()
    });
    assert!(frame(&mut devices).held.is_empty());
    let mut state = XrControllerState {
        connected: true,
        ..Default::default()
    };
    state.right.trigger = 0.04; // inside the 0.05 trigger deadzone
    state.left.thumbstick = [0.0, 0.15 + 0.85 * 0.25]; // 0.25 after the 0.15 deadzone
    state.right.primary = true;
    feed.set(state);
    let out = frame(&mut devices);
    assert_eq!(out.value("fire"), 0.0);
    assert!((out.value("forward") - 0.25).abs() < 1e-5, "{out:?}");
    assert_eq!(out.value("back"), 0.0);
    assert!(out.pressed("jump"));
    state.left.thumbstick = [0.0, -1.0];
    state.right.trigger = 1.0;
    feed.set(state);
    let out = frame(&mut devices);
    assert_eq!(out.value("back"), 1.0);
    assert_eq!(out.value("forward"), 0.0);
    assert_eq!(out.value("fire"), 1.0);
    feed.set(XrControllerState::default());
    let out = frame(&mut devices);
    assert!(out.held.is_empty());
    assert!(out.released.contains("fire") && out.released.contains("jump"));
}

#[test]
fn haptic_requests_validate_bound_and_resolve_defaults() {
    let mut world = World::new();
    assert!(
        request_haptic(
            &mut world,
            XrHapticRequest {
                amplitude: Some(1.5),
                ..Default::default()
            }
        )
        .is_err()
    );
    request_haptic(
        &mut world,
        XrHapticRequest {
            target: XrHapticTarget::Left,
            amplitude: Some(0.9),
            ..Default::default()
        },
    )
    .unwrap();
    request_haptic(&mut world, XrHapticRequest::default()).unwrap();
    let pulses = drain_haptics(&mut world);
    let defaults = XrHapticDefaults::default();
    assert_eq!(
        pulses,
        vec![
            XrHapticPulse {
                hand: XrHapticTarget::Left,
                amplitude: 0.9,
                duration_ms: defaults.duration_ms,
                frequency_hz: 0.0
            },
            XrHapticPulse {
                hand: XrHapticTarget::Left,
                amplitude: defaults.amplitude,
                duration_ms: defaults.duration_ms,
                frequency_hz: 0.0
            },
            XrHapticPulse {
                hand: XrHapticTarget::Right,
                amplitude: defaults.amplitude,
                duration_ms: defaults.duration_ms,
                frequency_hz: 0.0
            },
        ]
    );
    assert!(drain_haptics(&mut world).is_empty());
    for _ in 0..MAX_QUEUED_HAPTICS {
        request_haptic(&mut world, XrHapticRequest::default()).unwrap();
    }
    let error = request_haptic(&mut world, XrHapticRequest::default()).unwrap_err();
    assert!(error.to_string().contains("xr.haptic.limit"));
}

fn project(settings: Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let rig = json!({"id":"rig","name":"XR rig","components":{
        "Transform":{"position":[0,0,0]},
        "EventBindings":{"bindings":[{"event":"hit","handler":XR_HAPTIC_HANDLER}]},
        "custom":{XR_SETTINGS_COMPONENT:{"version":1,"data":settings}}
    }});
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":[rig]})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"xr-test","startup_scene":"scene.json","assets":[],
            "inputBindings":{"grab":["XR:RightGrip","Key:G"]}}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}

#[test]
fn authored_settings_hydrate_report_status_and_route_haptic_events() {
    let dir = project(json!({"render_scale":1.5,"haptics":{"amplitude":0.25,"duration_ms":20}}));
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap(); // hydrates authored components
    let (_, settings) = active_settings(runtime.app().world()).unwrap().unwrap();
    assert_eq!(settings.render_scale, 1.5);
    assert_eq!(settings.haptics.duration_ms, 20.0);
    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.xr.as_ref().unwrap().state, "not_attached");
    let encoded = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(encoded["xr"]["state"], "not_attached");
    let rig = snapshot
        .scene
        .entities
        .iter()
        .find(|e| e.id == "rig")
        .unwrap();
    assert_eq!(
        rig.components.custom[XR_SETTINGS_COMPONENT].data["render_scale"],
        1.5
    );

    emit_event(
        runtime.app_mut().world_mut(),
        EngineEvent {
            name: "hit".into(),
            source: None,
            target: None,
            payload: json!({"target":"right","amplitude":0.9}),
        },
    )
    .unwrap();
    for _ in 0..2 {
        runtime.tick(1.0 / 60.0, InputSnapshot::default()).unwrap();
    }
    let pulses = drain_haptics(runtime.app_mut().world_mut());
    assert_eq!(
        pulses,
        vec![XrHapticPulse {
            hand: XrHapticTarget::Right,
            amplitude: 0.9,
            duration_ms: 20.0,
            frequency_hz: 0.0
        }]
    );

    publish_status(
        runtime.app_mut().world_mut(),
        XrStatusReport::new(
            "no_headset",
            "x".repeat(MAX_STATUS_REASON + 10),
            Some("Runtime 1".into()),
        ),
    );
    let report = runtime.snapshot().xr.unwrap();
    assert_eq!(report.state, "no_headset");
    assert_eq!(report.reason.len(), MAX_STATUS_REASON);
}

#[test]
fn disabled_settings_report_nothing_and_drop_haptics_when_haptics_are_off() {
    let dir = project(json!({"enabled": false}));
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap(); // hydrates authored components
    assert!(runtime.snapshot().xr.is_none());
    assert!(active_settings(runtime.app().world()).unwrap().is_none());

    let dir = project(json!({"haptics": {"enabled": false}}));
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap(); // hydrates authored components
    request_haptic(runtime.app_mut().world_mut(), XrHapticRequest::default()).unwrap();
    assert!(drain_haptics(runtime.app_mut().world_mut()).is_empty());
}

#[test]
fn invalid_authored_settings_fail_project_validation() {
    let dir = project(json!({"near_clip": 5.0, "far_clip": 1.0}));
    let error = match Project::load(dir.path().join("project.json")) {
        Err(error) => error,
        Ok(project) => Runtime::new(project)
            .err()
            .expect("invalid settings must be rejected"),
    };
    assert!(
        error.to_string().contains("far_clip must exceed near_clip"),
        "{error}"
    );
}
