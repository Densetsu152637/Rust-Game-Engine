use bevy_ecs::prelude::{Entity, Resource, World};
use engine_project::{input::*, *};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone)]
struct Handle {
    sample: Arc<Mutex<Result<DeviceSample>>>,
    polls: Arc<AtomicUsize>,
}
struct Source {
    id: String,
    handle: Handle,
}
impl InputSource for Source {
    fn descriptor(&self) -> DeviceDescriptor {
        DeviceDescriptor {
            id: self.id.clone(),
            label: "Adaptive switch and axis".into(),
            controls: BTreeMap::from([
                ("switch".into(), ControlKind::Button),
                ("axis".into(), ControlKind::Axis { deadzone: 0.2 }),
            ]),
        }
    }
    fn poll(&mut self) -> Result<DeviceSample> {
        self.handle.polls.fetch_add(1, Ordering::Relaxed);
        self.handle.sample.lock().unwrap().clone()
    }
}
fn source(id: &str) -> (Source, Handle) {
    let handle = Handle {
        sample: Arc::new(Mutex::new(Ok(DeviceSample::connected()))),
        polls: Arc::new(AtomicUsize::new(0)),
    };
    (
        Source {
            id: id.into(),
            handle: handle.clone(),
        },
        handle,
    )
}
impl Handle {
    fn value(&self, name: &str, value: f32) {
        let mut sample = DeviceSample::connected();
        let result = sample.set(name, value).map(|()| sample);
        *self.sample.lock().unwrap() = result;
    }
    fn disconnect(&self) {
        *self.sample.lock().unwrap() = Ok(DeviceSample::disconnected());
    }
}
fn binding(device: &str, control: &str) -> ControlBinding {
    ControlBinding {
        device: device.into(),
        control: control.into(),
        direction: AxisDirection::Positive,
    }
}
fn physical(held: bool) -> ActionFrame {
    ActionFrame {
        held: if held {
            ["jump".into()].into()
        } else {
            Default::default()
        },
        ..Default::default()
    }
}
fn devices() -> (InputDevices, Handle) {
    let (source, handle) = source("switchboard");
    let mut devices = InputDevices::default();
    devices.register(source).unwrap();
    devices
        .set_bindings(BTreeMap::from([(
            "jump".into(),
            vec![binding("switchboard", "switch")],
        )]))
        .unwrap();
    devices.poll_and_merge(ActionFrame::default()); // connection requires neutral before activation
    (devices, handle)
}

#[test]
fn multiple_sources_release_only_after_the_last_hold_and_keep_tap_edges() {
    let (mut devices, a) = devices();
    let (source, b) = source("second");
    devices.register(source).unwrap();
    devices
        .set_bindings(BTreeMap::from([(
            "jump".into(),
            vec![
                binding("switchboard", "switch"),
                binding("second", "switch"),
            ],
        )]))
        .unwrap();
    devices.poll_and_merge(ActionFrame::default());
    a.value("switch", 1.0);
    b.value("switch", 1.0);
    assert!(devices.poll_and_merge(physical(true)).pressed("jump"));
    a.disconnect();
    let frame = devices.poll_and_merge(physical(false));
    assert!(frame.held("jump") && !frame.released("jump") && !frame.pressed("jump"));
    b.disconnect();
    assert!(devices.poll_and_merge(physical(true)).held("jump"));
    assert!(devices.poll_and_merge(physical(false)).released("jump"));
    let tap = ActionFrame {
        pressed: ["jump".into()].into(),
        released: ["jump".into()].into(),
        ..Default::default()
    };
    let frame = devices.poll_and_merge(tap);
    assert!(frame.pressed("jump") && frame.released("jump") && !frame.held("jump"));
}
#[test]
fn focus_hotplug_and_errors_neutralize_without_stuck_or_synthetic_presses() {
    let (mut devices, handle) = devices();
    handle.value("switch", 1.0);
    assert!(devices.poll_and_merge(physical(false)).pressed("jump"));
    let unfocused = ActionFrame {
        focused: false,
        ..Default::default()
    };
    assert!(devices.poll_and_merge(unfocused).released("jump"));
    assert!(!devices.poll_and_merge(physical(false)).held("jump"));
    handle.value("switch", 0.0);
    devices.poll_and_merge(physical(false));
    handle.value("switch", 1.0);
    assert!(devices.poll_and_merge(physical(false)).pressed("jump"));
    handle.value("axis", f32::NAN);
    assert!(devices.poll_and_merge(physical(false)).released("jump"));
    assert!(devices.snapshot().devices[0].error.is_some());
    handle.value("switch", 1.0);
    assert!(!devices.poll_and_merge(physical(false)).held("jump"));
    handle.value("switch", 0.0);
    devices.poll_and_merge(physical(false));
    handle.value("switch", 1.0);
    assert!(devices.poll_and_merge(physical(false)).pressed("jump"));
    handle.value("undeclared", 1.0);
    assert!(devices.poll_and_merge(physical(false)).released("jump"));
    assert!(
        devices.snapshot().devices[0]
            .error
            .as_ref()
            .unwrap()
            .contains("undeclared")
    );
    assert_eq!(handle.polls.load(Ordering::Relaxed), 11);
}
#[test]
fn remapping_is_atomic_and_held_controls_require_neutral() {
    let (mut devices, handle) = devices();
    handle.value("switch", 1.0);
    devices.poll_and_merge(physical(false));
    let before = devices.snapshot().bindings;
    assert!(
        devices
            .set_bindings(BTreeMap::from([(
                "bad".into(),
                vec![binding("absent", "switch")]
            )]))
            .is_err()
    );
    assert_eq!(devices.snapshot().bindings, before);
    devices
        .set_bindings(BTreeMap::from([(
            "confirm".into(),
            vec![binding("switchboard", "switch")],
        )]))
        .unwrap();
    let frame = devices.poll_and_merge(physical(false));
    assert!(frame.released("jump") && !frame.pressed("confirm"));
    handle.value("switch", 0.0);
    devices.poll_and_merge(physical(false));
    handle.value("switch", 1.0);
    assert!(devices.poll_and_merge(physical(false)).pressed("confirm"));
    assert!(devices.unregister("switchboard"));
    assert!(devices.poll_and_merge(physical(false)).released("confirm"));
}
#[test]
fn signed_axes_normalize_and_toggle_works_across_physical_and_custom_sources() {
    let (mut devices, handle) = devices();
    let mut negative = binding("switchboard", "axis");
    negative.direction = AxisDirection::Negative;
    devices
        .set_bindings(BTreeMap::from([
            ("left".into(), vec![negative]),
            ("jump".into(), vec![binding("switchboard", "switch")]),
        ]))
        .unwrap();
    handle.value("axis", -0.6);
    assert!((devices.poll_and_merge(physical(false)).value("left") - 0.5).abs() < 1e-6);
    handle.value("axis", 0.0);
    devices.poll_and_merge(physical(false));
    devices
        .set_behavior("jump", ActionBehavior::Toggle)
        .unwrap();
    assert!(devices.poll_and_merge(physical(true)).pressed("jump"));
    assert!(devices.poll_and_merge(physical(false)).held("jump"));
    handle.value("switch", 1.0);
    let frame = devices.poll_and_merge(physical(false));
    assert!(frame.released("jump") && !frame.pressed("jump"));
    handle.value("switch", 0.0);
    devices.poll_and_merge(physical(false));
    handle.value("switch", 1.0);
    devices.poll_and_merge(physical(false));
    handle.disconnect();
    assert!(devices.poll_and_merge(physical(false)).released("jump"));
}
#[test]
fn disconnect_clears_only_unheld_affected_toggles() {
    let (mut devices, a) = devices();
    let (source, b) = source("second");
    devices.register(source).unwrap();
    devices
        .set_bindings(BTreeMap::from([(
            "jump".into(),
            vec![
                binding("switchboard", "switch"),
                binding("second", "switch"),
            ],
        )]))
        .unwrap();
    devices
        .set_behavior("jump", ActionBehavior::Toggle)
        .unwrap();
    devices.set_behavior("run", ActionBehavior::Toggle).unwrap();
    let run = ActionFrame {
        pressed: ["run".into()].into(),
        ..Default::default()
    };
    devices.poll_and_merge(run);
    a.value("switch", 1.0);
    b.value("switch", 1.0);
    assert!(
        devices
            .poll_and_merge(ActionFrame::default())
            .pressed("jump")
    );
    a.disconnect();
    let frame = devices.poll_and_merge(ActionFrame::default());
    assert!(frame.held("jump") && frame.held("run") && !frame.released("jump"));
    b.disconnect();
    let frame = devices.poll_and_merge(ActionFrame::default());
    assert!(frame.released("jump") && frame.held("run"));
}
#[test]
fn source_and_sample_limits_are_enforced() {
    let mut devices = InputDevices::default();
    for n in 0..MAX_INPUT_DEVICES {
        devices.register(source(&format!("device-{n}")).0).unwrap();
    }
    assert!(devices.register(source("overflow").0).is_err());
    let mut sample = DeviceSample::connected();
    for n in 0..MAX_DEVICE_CONTROLS {
        sample.set(format!("control-{n}"), 0.0).unwrap();
    }
    assert!(sample.set("overflow", 0.0).is_err());
    assert!(sample.set("control-0", f32::INFINITY).is_err());
    assert!(sample.set("control-0", 1.0).is_ok());
}

#[derive(Resource, Default)]
struct Seen(Vec<ActionFrame>);

#[test]
fn physical_remap_quarantines_controls_even_when_action_names_change() {
    for token in ["Key:Space", "Gamepad:A", "Gamepad:LeftXPositive"] {
        let mut mapper =
            ActionMapper::new(BTreeMap::from([("jump".into(), vec![token.into()])])).unwrap();
        let mut raw = RawInput {
            gamepad_connected: true,
            ..Default::default()
        };
        if token.ends_with("Positive") {
            raw.axes.insert("LeftX".into(), 1.0);
        } else {
            raw.down.insert(token.into());
        }
        assert!(mapper.sample(&raw).pressed("jump"));
        assert!(
            mapper
                .rebind(
                    BTreeMap::from([("confirm".into(), vec![token.into()])]),
                    false
                )
                .errors
                .is_empty()
        );
        let frame = mapper.sample(&raw);
        assert!(
            frame.cancelled.contains("jump") && !frame.held("confirm"),
            "{token}"
        );
        raw.down.clear();
        raw.axes.clear();
        mapper.sample(&raw);
        if token.ends_with("Positive") {
            raw.axes.insert("LeftX".into(), 1.0);
        } else {
            raw.down.insert(token.into());
        }
        assert!(mapper.sample(&raw).pressed("confirm"), "{token}");
        mapper.rebind(BTreeMap::new(), false);
        raw.down.clear();
        raw.axes.clear();
        mapper.sample(&raw);
        mapper.rebind(
            BTreeMap::from([("confirm".into(), vec![token.into()])]),
            false,
        );
        if token.ends_with("Positive") {
            raw.axes.insert("LeftX".into(), 1.0);
        } else {
            raw.down.insert(token.into());
        }
        assert!(
            mapper.sample(&raw).pressed("confirm"),
            "neutral while unbound must rearm {token}"
        );
    }
    let mut mapper = ActionMapper::new(BTreeMap::new()).unwrap();
    let raw = RawInput {
        down: ["Key:A".into()].into(),
        ..Default::default()
    };
    mapper.sample(&raw);
    mapper.rebind(
        BTreeMap::from([("new".into(), vec!["Key:A".into()])]),
        false,
    );
    assert!(
        !mapper.sample(&raw).pressed("new"),
        "previously unbound held controls are quarantined too"
    );
}

#[test]
fn mapper_to_runtime_cancels_toggles_on_rebind_and_controller_loss() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("scene.json"),
        r#"{"schema":1,"scene_id":"input","entities":[]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("project.json"),
        r#"{"schema":1,"name":"Physical Input","startup_scene":"scene.json","assets":[]}"#,
    )
    .unwrap();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    let (source, custom) = source("switchboard");
    register_input_source(runtime.app_mut().world_mut(), source).unwrap();
    let bindings = BTreeMap::from([("jump".into(), vec!["Key:Space".into(), "Gamepad:A".into()])]);
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<runtime::InputSettings>()
        .rebind(bindings.clone(), 0.2)
        .unwrap();
    {
        let mut devices = runtime.app_mut().world_mut().resource_mut::<InputDevices>();
        devices
            .set_bindings(BTreeMap::from([(
                "jump".into(),
                vec![binding("switchboard", "switch")],
            )]))
            .unwrap();
        devices
            .set_behavior("jump", ActionBehavior::Toggle)
            .unwrap();
    }
    let mut mapper = ActionMapper::new(bindings.clone()).unwrap();
    let mut raw = RawInput {
        gamepad_connected: true,
        ..Default::default()
    };
    let tick = |runtime: &mut Runtime, mapper: &mut ActionMapper, raw: &RawInput| {
        runtime.tick(1.0 / 60.0, mapper.sample(raw)).unwrap();
        runtime.app().world().resource::<ActionFrame>().clone()
    };
    tick(&mut runtime, &mut mapper, &raw);
    raw.down.insert("Key:Space".into());
    assert!(tick(&mut runtime, &mut mapper, &raw).pressed("jump"));
    raw.down.clear();
    assert!(tick(&mut runtime, &mut mapper, &raw).held("jump"));
    mapper.rebind(BTreeMap::new(), false);
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<runtime::InputSettings>()
        .rebind(BTreeMap::new(), 0.2)
        .unwrap();
    assert!(tick(&mut runtime, &mut mapper, &raw).released("jump"));
    mapper.rebind(bindings.clone(), false);
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<runtime::InputSettings>()
        .rebind(bindings, 0.2)
        .unwrap();
    tick(&mut runtime, &mut mapper, &raw);
    raw.down.insert("Gamepad:A".into());
    assert!(tick(&mut runtime, &mut mapper, &raw).held("jump"));
    raw.down.clear();
    tick(&mut runtime, &mut mapper, &raw);
    raw.gamepad_connected = false;
    assert!(tick(&mut runtime, &mut mapper, &raw).released("jump"));
    raw.gamepad_connected = true;
    tick(&mut runtime, &mut mapper, &raw);
    raw.down.insert("Gamepad:A".into());
    custom.value("switch", 1.0);
    assert!(tick(&mut runtime, &mut mapper, &raw).held("jump"));
    raw.gamepad_connected = false;
    let frame = tick(&mut runtime, &mut mapper, &raw);
    assert!(frame.held("jump") && !frame.released("jump"));
    custom.disconnect();
    assert!(tick(&mut runtime, &mut mapper, &raw).released("jump"));
    // Hosts supplying frames directly still receive InputSettings revision cleanup.
    runtime
        .tick(
            1.0 / 60.0,
            ActionFrame {
                pressed: ["jump".into()].into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(runtime.app().world().resource::<ActionFrame>().held("jump"));
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<runtime::InputSettings>()
        .rebind(BTreeMap::new(), 0.2)
        .unwrap();
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    assert!(
        runtime
            .app()
            .world()
            .resource::<ActionFrame>()
            .released("jump")
    );
    runtime.stop().unwrap();
}

struct Observer;
impl NativeScript for Observer {
    fn on_update(&mut self, world: &mut World, _: Entity, _: f32) -> Result<()> {
        let frame = services::gameplay_input(world);
        let open = frame.pressed("jump");
        world.resource_mut::<Seen>().0.push(frame);
        if open {
            world.resource_mut::<ui::UiModel>().open(ui::PanelOptions {
                id: "modal".into(),
                modal: true,
                ..Default::default()
            })?;
            let captured = services::gameplay_input(world);
            world.resource_mut::<Seen>().0.push(captured);
        }
        Ok(())
    }
}
fn observe(_: &mut World, _: Entity) -> Result<Box<dyn NativeScript>> {
    Ok(Box::new(Observer))
}
#[test]
fn ordinary_headless_runtime_polls_before_callbacks_and_modal_capture() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("script.rs"), "pub fn configure() {}\n").unwrap();
    std::fs::write(dir.path().join("scene.json"), r#"{"schema":1,"scene_id":"input","entities":[{"id":"observer","name":"Observer","components":{"Script":{"asset":"script"}}}]}"#).unwrap();
    std::fs::write(dir.path().join("project.json"), r#"{"schema":1,"name":"Input","startup_scene":"scene.json","assets":[{"id":"script","kind":"script","path":"script.rs"}]}"#).unwrap();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.register_script("script", observe).unwrap();
    let (source, handle) = source("switchboard");
    // Same operations available to an external game's configure(app) function.
    register_input_source(runtime.app_mut().world_mut(), source).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<InputDevices>()
        .set_bindings(BTreeMap::from([(
            "jump".into(),
            vec![binding("switchboard", "switch")],
        )]))
        .unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Seen::default());
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    handle.value("switch", 1.0);
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    let seen = &runtime.app().world().resource::<Seen>().0;
    assert!(seen[seen.len() - 2].pressed("jump"));
    assert!(
        !seen.last().unwrap().held("jump"),
        "same-callback modal opening must capture the custom action"
    );
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    assert!(
        !runtime
            .app()
            .world()
            .resource::<Seen>()
            .0
            .last()
            .unwrap()
            .held("jump")
    );
    assert_eq!(handle.polls.load(Ordering::Relaxed), 3);
    let snapshot = serde_json::to_value(runtime.try_snapshot().unwrap()).unwrap();
    assert_eq!(
        snapshot["input_devices"]["devices"][0]["descriptor"]["id"],
        "switchboard"
    );
    assert_eq!(snapshot["input_devices"]["devices"][0]["connected"], true);
    assert_eq!(
        snapshot["input_devices"]["bindings"]["jump"][0]["control"],
        "switch"
    );
    runtime.stop().unwrap();
}
