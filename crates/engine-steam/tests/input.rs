//! Steam Input: scripted fake controllers through `SteamInputSource` and the engine's
//! `InputDevices`/`ActionMapper`.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use bevy_ecs::prelude::World;
use engine_project::input::{
    ActionFrame, ActionMapper, AxisDirection, ControlBinding, DeviceBindings, InputDevices,
    InputSource, RawInput, poll_devices, register_input_source,
};
use engine_steam::*;

struct Shared {
    fake: Arc<Mutex<FakeBackend>>,
}

impl SteamBackend for Shared {
    fn status(&self) -> SteamStatus {
        self.fake.lock().unwrap().status()
    }
    common::forward_input!();
}

const PAD: ControllerId = ControllerId(11);

fn ready() -> FakeBackend {
    FakeBackend::ready(AppId(480), "tester")
}

fn layout() -> SteamInputLayout {
    SteamInputLayout::new()
        .digital("jump")
        .digital("menu_select")
        .analog("move")
}

struct Rig {
    fake: Arc<Mutex<FakeBackend>>,
    session: SteamSession,
    world: World,
    mapper: ActionMapper,
}

impl Rig {
    fn new(fake: FakeBackend, layout: SteamInputLayout) -> Self {
        let fake = Arc::new(Mutex::new(fake));
        let session = SteamSession::new(Box::new(Shared { fake: fake.clone() }));
        let mut world = World::new();
        let source = SteamInputSource::new(session.clone(), layout).unwrap();
        register_input_source(&mut world, source).unwrap();
        Self {
            fake,
            session,
            world,
            mapper: ActionMapper::new(BTreeMap::new()).unwrap(),
        }
    }

    fn bind(&mut self, action: &str, control: &str, direction: AxisDirection) {
        let mut devices = self.world.resource_mut::<InputDevices>();
        let mut bindings: DeviceBindings = devices.snapshot().bindings;
        bindings
            .entry(action.to_owned())
            .or_default()
            .push(ControlBinding {
                device: STEAM_INPUT_DEVICE_ID.into(),
                control: control.into(),
                direction,
            });
        devices.set_bindings(bindings).unwrap();
    }

    fn tick(&mut self) -> ActionFrame {
        let physical = self.mapper.sample(&RawInput::default());
        poll_devices(&mut self.world, physical)
    }

    fn fake(&self) -> std::sync::MutexGuard<'_, FakeBackend> {
        self.fake.lock().unwrap()
    }

    fn device_connected(&self) -> bool {
        self.world.resource::<InputDevices>().snapshot().devices[0].connected
    }
}

#[test]
fn scripted_button_press_reaches_a_bound_action() {
    let mut rig = Rig::new(ready().with_controller(PAD), layout());
    rig.bind("Jump", "jump", AxisDirection::Positive);
    assert!(!rig.tick().held("Jump"));
    assert!(rig.device_connected());

    rig.fake().set_digital(PAD, "jump", true);
    let frame = rig.tick();
    assert!(frame.pressed("Jump") && frame.held("Jump"));
    assert_eq!(frame.value("Jump"), 1.0);
    let frame = rig.tick();
    assert!(frame.held("Jump") && !frame.pressed("Jump"));

    rig.fake().set_digital(PAD, "jump", false);
    let frame = rig.tick();
    assert!(frame.released("Jump") && !frame.held("Jump"));
    // Steam Input is polled once per tick: init once, RunFrame every tick.
    assert_eq!(rig.fake().call_count("input_init"), 4);
    assert_eq!(rig.fake().call_count("input_run_frame"), 4);
}

#[test]
fn scripted_axis_reaches_actions_in_both_directions() {
    let mut rig = Rig::new(ready().with_controller(PAD), layout());
    rig.bind("Forward", "move.y", AxisDirection::Positive);
    rig.bind("Back", "move.y", AxisDirection::Negative);
    rig.bind("Right", "move.x", AxisDirection::Positive);
    rig.tick();

    rig.fake().set_analog(PAD, "move", 0.0, 0.8);
    let frame = rig.tick();
    assert!(frame.held("Forward") && !frame.held("Back") && !frame.held("Right"));
    assert!(frame.value("Forward") > 0.7);

    rig.fake().set_analog(PAD, "move", 0.5, -1.0);
    let frame = rig.tick();
    assert!(frame.held("Back") && frame.held("Right") && !frame.held("Forward"));
    assert!(frame.value("Back") > 0.9);

    // Inside the engine dead zone the stick reads neutral.
    rig.fake().set_analog(PAD, "move", 0.01, 0.0);
    let frame = rig.tick();
    assert!(!frame.held("Right"));
}

#[test]
fn several_controllers_merge() {
    let other = ControllerId(12);
    let mut rig = Rig::new(
        ready().with_controller(PAD).with_controller(other),
        layout(),
    );
    rig.bind("Jump", "jump", AxisDirection::Positive);
    rig.bind("Right", "move.x", AxisDirection::Positive);
    rig.tick();
    rig.fake().set_digital(other, "jump", true);
    rig.fake().set_analog(PAD, "move", 0.3, 0.0);
    rig.fake().set_analog(other, "move", 0.9, 0.0);
    let frame = rig.tick();
    assert!(frame.held("Jump"));
    assert!(frame.value("Right") > 0.8);
}

#[test]
fn action_sets_gate_actions_and_apply_to_new_controllers() {
    let fake = ready()
        .with_controller(PAD)
        .with_action_set("gameplay", &["jump", "move"])
        .with_action_set("menu", &["menu_select"]);
    let mut rig = Rig::new(fake, layout());
    rig.bind("Jump", "jump", AxisDirection::Positive);
    rig.bind("Select", "menu_select", AxisDirection::Positive);
    rig.tick();

    rig.session.set_action_set("gameplay").unwrap();
    assert_eq!(rig.session.action_set().as_deref(), Some("gameplay"));
    assert_eq!(rig.fake().action_set(PAD), Some("gameplay"));
    rig.fake().set_digital(PAD, "jump", true);
    rig.fake().set_digital(PAD, "menu_select", true);
    let frame = rig.tick();
    assert!(frame.held("Jump") && !frame.held("Select"));

    rig.session.set_action_set("menu").unwrap();
    let frame = rig.tick();
    assert!(frame.held("Select"));
    assert!(frame.released("Jump"));

    // A controller plugged in later gets the requested set on the next poll.
    let late = ControllerId(21);
    rig.fake().plug_controller(late);
    rig.tick();
    assert_eq!(rig.fake().action_set(late), Some("menu"));
}

#[test]
fn unknown_action_set_is_rejected_and_changes_nothing() {
    let fake = ready()
        .with_controller(PAD)
        .with_action_set("gameplay", &["jump"]);
    let rig = Rig::new(fake, layout());
    assert!(matches!(
        rig.session.set_action_set("nope"),
        Err(SteamError::NotFound(_))
    ));
    assert_eq!(rig.session.action_set(), None);
    // Before Steam Input starts, a request is remembered and applied by the first poll.
    let early = Rig::new(ready(), layout());
    early.session.set_action_set("gameplay").unwrap();
    assert_eq!(early.session.action_set().as_deref(), Some("gameplay"));
}

#[test]
fn controller_connect_and_disconnect_raise_events_and_the_device_status() {
    let mut rig = Rig::new(ready(), layout());
    rig.bind("Jump", "jump", AxisDirection::Positive);
    rig.tick();
    assert!(!rig.device_connected(), "no controller means disconnected");
    assert!(rig.session.run_callbacks().is_empty());

    rig.fake().plug_controller(PAD);
    rig.tick();
    assert!(rig.device_connected());
    assert_eq!(
        rig.session.run_callbacks(),
        vec![SteamEvent::ControllerConnected(PAD)]
    );
    assert_eq!(rig.session.controllers().unwrap(), vec![PAD]);

    rig.fake().set_digital(PAD, "jump", true);
    assert!(rig.tick().held("Jump"));
    rig.fake().unplug_controller(PAD);
    let frame = rig.tick();
    assert!(!rig.device_connected());
    assert!(!frame.held("Jump"));
    assert!(frame.released("Jump"));
    assert_eq!(
        rig.session.run_callbacks(),
        vec![SteamEvent::ControllerDisconnected(PAD)]
    );
}

#[test]
fn glyphs_are_looked_up_through_the_session() {
    let glyph = ActionGlyph {
        origin: ActionOrigin(3),
        name: "Button A".into(),
        glyph_path: "glyphs/a.png".into(),
    };
    let fake =
        ready()
            .with_controller(PAD)
            .with_action_glyphs("gameplay", "jump", vec![glyph.clone()]);
    let rig = Rig::new(fake, layout());
    assert_eq!(
        rig.session
            .action_glyphs(PAD, "gameplay", "jump", InputActionKind::Digital),
        Ok(vec![glyph])
    );
    assert_eq!(
        rig.session
            .action_glyphs(PAD, "gameplay", "move", InputActionKind::Analog),
        Ok(Vec::new())
    );
    assert!(matches!(
        rig.session.action_glyphs(
            ControllerId(99),
            "gameplay",
            "jump",
            InputActionKind::Digital
        ),
        Err(SteamError::NotFound(_))
    ));
}

#[test]
fn without_steam_the_device_is_disconnected_without_errors() {
    let session = SteamSession::new(Box::new(NullBackend::with_status(SteamStatus::NotRunning)));
    let mut source = SteamInputSource::new(session.clone(), layout()).unwrap();
    assert!(source.poll().is_ok());
    let mut world = World::new();
    register_input_source(
        &mut world,
        SteamInputSource::new(session, layout()).unwrap(),
    )
    .unwrap();
    let physical = ActionMapper::new(BTreeMap::new())
        .unwrap()
        .sample(&RawInput::default());
    poll_devices(&mut world, physical);
    let snapshot = world.resource::<InputDevices>().snapshot();
    assert!(!snapshot.devices[0].connected);
    assert_eq!(snapshot.devices[0].error, None);
}

#[test]
fn a_missing_action_in_the_manifest_is_reported_not_swallowed() {
    // A backend that knows no actions: NotFound surfaces as a device error.
    struct NoActions(FakeBackend);
    impl SteamBackend for NoActions {
        fn status(&self) -> SteamStatus {
            self.0.status()
        }
        fn input_init(&mut self, m: Option<&std::path::Path>) -> SteamResult<()> {
            self.0.input_init(m)
        }
        fn input_run_frame(&mut self) -> SteamResult<()> {
            self.0.input_run_frame()
        }
        fn controllers(&self) -> SteamResult<Vec<ControllerId>> {
            self.0.controllers()
        }
        fn input_digital(&self, _: ControllerId, action: &str) -> SteamResult<DigitalActionState> {
            Err(SteamError::NotFound(format!("digital action {action:?}")))
        }
    }
    let session = SteamSession::new(Box::new(NoActions(ready().with_controller(PAD))));
    let mut world = World::new();
    register_input_source(
        &mut world,
        SteamInputSource::new(session, layout()).unwrap(),
    )
    .unwrap();
    let physical = ActionMapper::new(BTreeMap::new())
        .unwrap()
        .sample(&RawInput::default());
    poll_devices(&mut world, physical);
    let snapshot = world.resource::<InputDevices>().snapshot();
    assert!(!snapshot.devices[0].connected);
    assert!(
        snapshot.devices[0]
            .error
            .as_deref()
            .unwrap()
            .contains("jump")
    );
}

#[test]
fn layout_validation_rejects_bad_names() {
    let session = SteamSession::new(Box::new(NullBackend::not_compiled()));
    let new = |layout| SteamInputSource::new(session.clone(), layout);
    assert!(new(SteamInputLayout::new()).is_err());
    assert!(new(SteamInputLayout::new().digital("")).is_err());
    assert!(new(SteamInputLayout::new().digital("a").digital("a")).is_err());
    // "m.x" as a button collides with the analog action "m"'s x axis.
    assert!(new(SteamInputLayout::new().digital("m.x").analog("m")).is_err());
    assert!(new(SteamInputLayout::new().digital("ok")).is_ok());
    let descriptor = new(layout()).unwrap().descriptor();
    assert_eq!(descriptor.id, STEAM_INPUT_DEVICE_ID);
    assert_eq!(descriptor.controls.len(), 4);
}
