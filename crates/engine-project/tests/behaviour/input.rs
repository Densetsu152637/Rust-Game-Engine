//! Input snapshots: binding registry and validation, action sampling, axes,
//! focus loss, rebinding quarantine, gamepad reconnects and injected pulses.
use super::common::*;
use engine_project::input::*;
use std::collections::{BTreeMap, BTreeSet};

fn bind(pairs: &[(&str, &[&str])]) -> BindingMap {
    pairs
        .iter()
        .map(|(action, tokens)| {
            (
                action.to_string(),
                tokens.iter().map(|t| t.to_string()).collect(),
            )
        })
        .collect()
}

fn mapper(pairs: &[(&str, &[&str])]) -> ActionMapper {
    ActionMapper::new(bind(pairs)).unwrap()
}

fn raw_down(tokens: &[&str]) -> RawInput {
    RawInput {
        down: tokens.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    }
}

fn pad(axes: &[(&str, f32)]) -> RawInput {
    RawInput {
        gamepad_connected: true,
        axes: axes.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        ..Default::default()
    }
}

fn names(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ---- ActionFrame -------------------------------------------------------------------

#[test]
fn default_frame_is_focused_and_empty() {
    let frame = ActionFrame::default();
    assert!(frame.focused);
    assert!(frame.pressed.is_empty() && frame.held.is_empty() && frame.released.is_empty());
    assert_eq!((frame.pointer, frame.wheel), ([0.0; 2], [0.0; 2]));
}

#[test]
fn frame_queries_report_membership_and_values() {
    let frame = ActionFrame {
        pressed: names(&["jump"]),
        held: names(&["jump", "run"]),
        released: names(&["crouch"]),
        values: BTreeMap::from([("run".to_string(), 0.75)]),
        ..Default::default()
    };
    assert!(frame.pressed("jump") && !frame.pressed("run"));
    assert!(frame.held("run") && !frame.held("crouch"));
    assert!(frame.released("crouch") && !frame.released("jump"));
    assert_eq!(frame.value("run"), 0.75);
}

#[test]
fn unknown_action_value_is_zero() {
    assert_eq!(ActionFrame::default().value("nothing"), 0.0);
}

#[test]
fn frame_deserializes_from_an_empty_object_as_focused() {
    let frame: ActionFrame = serde_json::from_str("{}").unwrap();
    assert_eq!(frame, ActionFrame::default());
}

#[test]
fn frame_deserializes_partial_objects_keeping_other_defaults() {
    let frame: ActionFrame = serde_json::from_str(r#"{"focused":false,"held":["a"]}"#).unwrap();
    assert!(!frame.focused);
    assert!(frame.held("a"));
    assert!(frame.pressed.is_empty());
}

#[test]
fn frame_round_trips_through_json() {
    let frame = ActionFrame {
        pressed: names(&["a"]),
        values: BTreeMap::from([("a".to_string(), 1.0)]),
        pointer: [3.0, 4.0],
        wheel: [0.0, -1.0],
        focused: false,
        ..Default::default()
    };
    let again: ActionFrame = serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
    assert_eq!(again, frame);
}

#[test]
fn input_snapshot_is_an_alias_of_the_action_frame() {
    let snapshot: InputSnapshot = ActionFrame {
        held: names(&["x"]),
        ..Default::default()
    };
    assert!(snapshot.held("x"));
}

// ---- binding registry -----------------------------------------------------------------

fn registry(token: &str, expected: bool) {
    assert_eq!(binding_registry().contains(token), expected, "{token}");
}

table! { registry;
    registry_has_the_first_letter => ("Key:A", true);
    registry_has_the_last_letter => ("Key:Z", true);
    registry_has_digit_zero => ("Key:0", true);
    registry_has_digit_nine => ("Key:9", true);
    registry_has_space => ("Key:Space", true);
    registry_has_escape => ("Key:Escape", true);
    registry_has_the_first_function_key => ("Key:F1", true);
    registry_has_the_last_function_key => ("Key:F25", true);
    registry_has_keypad_zero => ("Key:Keypad0", true);
    registry_has_keypad_nine => ("Key:Keypad9", true);
    registry_has_keypad_enter => ("Key:KeypadEnter", true);
    registry_has_left_shift => ("Key:LeftShift", true);
    registry_has_menu => ("Key:Menu", true);
    registry_has_arrow_keys => ("Key:Up", true);
    registry_has_primary_mouse_button => ("Mouse:Left", true);
    registry_has_middle_mouse_button => ("Mouse:Middle", true);
    registry_has_extra_mouse_buttons => ("Mouse:8", true);
    registry_has_wheel_directions => ("Mouse:WheelLeft", true);
    registry_has_gamepad_face_button => ("Gamepad:A", true);
    registry_has_gamepad_guide => ("Gamepad:Guide", true);
    registry_has_gamepad_dpad => ("Gamepad:DpadLeft", true);
    registry_has_gamepad_trigger => ("Gamepad:LeftTrigger", true);
    registry_has_gamepad_axis_directions => ("Gamepad:RightXNegative", true);
    registry_rejects_lowercase_letters => ("Key:a", false);
    registry_rejects_function_key_zero => ("Key:F0", false);
    registry_rejects_function_key_twenty_six => ("Key:F26", false);
    registry_rejects_keypad_ten => ("Key:Keypad10", false);
    registry_rejects_unprefixed_names => ("A", false);
    registry_rejects_empty_key_names => ("Key:", false);
    registry_rejects_unknown_devices => ("Joystick:1", false);
    registry_rejects_mouse_button_nine => ("Mouse:9", false);
    registry_rejects_lowercase_wheel => ("Mouse:wheelup", false);
    registry_rejects_trigger_directions => ("Gamepad:LeftTriggerPositive", false);
    registry_rejects_unknown_gamepad_buttons => ("Gamepad:Z", false);
}

// ---- validation -----------------------------------------------------------------------

#[test]
fn valid_bindings_have_no_errors_or_conflicts() {
    let v = validate_bindings(
        &bind(&[
            ("jump", &["Key:Space"]),
            ("fire", &["Mouse:Left", "Gamepad:A"]),
        ]),
        true,
    );
    assert!(v.errors.is_empty() && v.conflicts.is_empty());
}

#[test]
fn unknown_tokens_are_reported_by_name() {
    let v = validate_bindings(&bind(&[("x", &["Key:Nope"])]), false);
    assert_eq!(v.errors, vec!["Unknown binding: Key:Nope"]);
}

#[test]
fn duplicate_tokens_inside_one_action_are_reported() {
    let v = validate_bindings(&bind(&[("x", &["Key:A", "Key:A"])]), false);
    assert_eq!(v.errors, vec!["Duplicate binding: Key:A"]);
    assert!(v.conflicts.is_empty());
}

#[test]
fn empty_token_lists_are_reported() {
    let v = validate_bindings(&bind(&[("x", &[])]), false);
    assert_eq!(v.errors.len(), 1);
    assert!(v.errors[0].contains("1 to 16"));
}

#[test]
fn empty_action_names_are_reported() {
    let v = validate_bindings(&bind(&[("", &["Key:A"])]), false);
    assert_eq!(v.errors.len(), 1);
}

#[test]
fn more_than_sixteen_tokens_are_reported() {
    let tokens: Vec<String> = ('A'..='Q').map(|c| format!("Key:{c}")).collect();
    let refs: Vec<&str> = tokens.iter().map(String::as_str).collect();
    let v = validate_bindings(&bind(&[("x", &refs)]), false);
    assert_eq!(v.errors.len(), 1);
}

#[test]
fn more_than_256_actions_are_reported() {
    let map: BindingMap = (0..257)
        .map(|i| (format!("a{i}"), vec!["Key:A".to_string()]))
        .collect();
    let v = validate_bindings(&map, false);
    assert!(v.errors.iter().any(|e| e.contains("256")));
}

#[test]
fn errors_accumulate_across_actions() {
    let v = validate_bindings(
        &bind(&[("a", &["Key:Nope"]), ("b", &["Key:A", "Key:A"])]),
        false,
    );
    assert_eq!(v.errors.len(), 2);
}

#[test]
fn conflicts_are_listed_without_errors_when_not_strict() {
    let v = validate_bindings(&bind(&[("b", &["Key:A"]), ("a", &["Key:A"])]), false);
    assert!(v.errors.is_empty());
    assert_eq!(v.conflicts["Key:A"], vec!["a", "b"]);
}

#[test]
fn conflicts_become_errors_when_strict() {
    let v = validate_bindings(&bind(&[("a", &["Key:A"]), ("b", &["Key:A"])]), true);
    assert_eq!(v.errors, vec!["Binding conflict: Key:A"]);
    assert_eq!(v.conflicts.len(), 1);
}

#[test]
fn three_way_conflicts_name_every_action() {
    let v = validate_bindings(
        &bind(&[("a", &["Key:Q"]), ("b", &["Key:Q"]), ("c", &["Key:Q"])]),
        false,
    );
    assert_eq!(v.conflicts["Key:Q"], vec!["a", "b", "c"]);
}

#[test]
fn each_conflicting_token_is_reported_separately_in_strict_mode() {
    let v = validate_bindings(
        &bind(&[("a", &["Key:A", "Key:B"]), ("b", &["Key:A", "Key:B"])]),
        true,
    );
    assert_eq!(v.errors.len(), 2);
}

// ---- mapper construction --------------------------------------------------------------

#[test]
fn mapper_rejects_invalid_bindings_with_every_error() {
    let error = ActionMapper::new(bind(&[("a", &["Key:Nope"]), ("b", &["Mouse:9"])])).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "input.bindings.invalid");
    assert!(error.diagnostics[0].message.contains("Key:Nope"));
    assert!(error.diagnostics[0].message.contains("Mouse:9"));
}

#[test]
fn mapper_accepts_conflicting_bindings() {
    assert!(ActionMapper::new(bind(&[("a", &["Key:A"]), ("b", &["Key:A"])])).is_ok());
}

#[test]
fn mapper_exposes_its_bindings_and_default_deadzone() {
    let m = mapper(&[("a", &["Key:A"])]);
    assert_eq!(m.bindings()["a"], vec!["Key:A"]);
    assert_eq!(m.deadzone(), 0.2);
}

fn deadzone_accepted(value: f32) {
    let mut m = mapper(&[("a", &["Key:A"])]);
    m.set_deadzone(value).unwrap();
    assert_eq!(m.deadzone(), value);
}

table! { deadzone_accepted;
    deadzone_zero_is_accepted => (0.0);
    deadzone_half_is_accepted => (0.5);
    deadzone_just_below_one_is_accepted => (0.99);
}

fn deadzone_rejected(value: f32) {
    let mut m = mapper(&[("a", &["Key:A"])]);
    expect_code(m.set_deadzone(value), "input.deadzone.invalid");
    assert_eq!(m.deadzone(), 0.2);
}

table! { deadzone_rejected;
    deadzone_one_is_rejected => (1.0);
    deadzone_negative_is_rejected => (-0.01);
    deadzone_nan_is_rejected => (f32::NAN);
    deadzone_infinity_is_rejected => (f32::INFINITY);
}

// ---- digital sampling -------------------------------------------------------------------

#[test]
fn a_key_press_lasts_one_pressed_frame_then_holds_then_releases() {
    let mut m = mapper(&[("jump", &["Key:Space"])]);
    let down = raw_down(&["Key:Space"]);
    let f1 = m.sample(&down);
    assert!(f1.pressed("jump") && f1.held("jump") && !f1.released("jump"));
    let f2 = m.sample(&down);
    assert!(!f2.pressed("jump") && f2.held("jump"));
    let f3 = m.sample(&RawInput::default());
    assert!(f3.released("jump") && !f3.held("jump"));
    let f4 = m.sample(&RawInput::default());
    assert!(!f4.released("jump"));
}

#[test]
fn held_actions_report_full_value() {
    let mut m = mapper(&[("jump", &["Key:Space"])]);
    assert_eq!(m.sample(&raw_down(&["Key:Space"])).value("jump"), 1.0);
}

#[test]
fn every_bound_action_has_a_value_even_when_idle() {
    let mut m = mapper(&[("a", &["Key:A"]), ("b", &["Key:B"])]);
    let frame = m.sample(&RawInput::default());
    assert_eq!(frame.values.len(), 2);
    assert!(frame.values.values().all(|v| *v == 0.0));
}

#[test]
fn unbound_keys_are_ignored() {
    let mut m = mapper(&[("jump", &["Key:Space"])]);
    let frame = m.sample(&raw_down(&["Key:Q"]));
    assert!(frame.held.is_empty() && frame.pressed.is_empty());
}

#[test]
fn simultaneous_actions_press_together() {
    let mut m = mapper(&[("a", &["Key:A"]), ("b", &["Key:B"])]);
    let frame = m.sample(&raw_down(&["Key:A", "Key:B"]));
    assert_eq!(frame.pressed, names(&["a", "b"]));
}

#[test]
fn any_of_several_bindings_activates_the_action() {
    let mut m = mapper(&[("fire", &["Key:F", "Mouse:Left"])]);
    assert!(m.sample(&raw_down(&["Mouse:Left"])).pressed("fire"));
}

#[test]
fn second_binding_joining_a_hold_does_not_press_again() {
    let mut m = mapper(&[("fire", &["Key:F", "Mouse:Left"])]);
    m.sample(&raw_down(&["Key:F"]));
    let frame = m.sample(&raw_down(&["Key:F", "Mouse:Left"]));
    assert!(frame.held("fire") && !frame.pressed("fire"));
}

#[test]
fn action_stays_held_until_every_binding_is_released() {
    let mut m = mapper(&[("fire", &["Key:F", "Mouse:Left"])]);
    m.sample(&raw_down(&["Key:F", "Mouse:Left"]));
    let still = m.sample(&raw_down(&["Mouse:Left"]));
    assert!(still.held("fire") && !still.released("fire"));
    let done = m.sample(&RawInput::default());
    assert!(done.released("fire"));
}

#[test]
fn shared_key_drives_every_action_bound_to_it() {
    let mut m = mapper(&[("confirm", &["Key:Enter"]), ("start", &["Key:Enter"])]);
    assert_eq!(
        m.sample(&raw_down(&["Key:Enter"])).pressed,
        names(&["confirm", "start"])
    );
}

#[test]
fn a_steady_hold_presses_exactly_once_over_many_frames() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    let presses = (0..100)
        .filter(|_| m.sample(&raw_down(&["Key:A"])).pressed("a"))
        .count();
    assert_eq!(presses, 1);
}

#[test]
fn rapid_tapping_produces_a_press_and_release_per_tap() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    let (mut presses, mut releases) = (0, 0);
    for i in 0..20 {
        let frame = if i % 2 == 0 {
            m.sample(&raw_down(&["Key:A"]))
        } else {
            m.sample(&RawInput::default())
        };
        presses += frame.pressed("a") as u32;
        releases += frame.released("a") as u32;
    }
    assert_eq!((presses, releases), (10, 10));
}

#[test]
fn pointer_position_is_passed_through_and_sanitized() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    let frame = m.sample(&RawInput {
        pointer: [12.5, f32::NAN],
        ..Default::default()
    });
    assert_eq!(frame.pointer, [12.5, 0.0]);
}

#[test]
fn infinite_pointer_coordinates_are_zeroed() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    let frame = m.sample(&RawInput {
        pointer: [f32::INFINITY, -4.0],
        ..Default::default()
    });
    assert_eq!(frame.pointer, [0.0, -4.0]);
}

#[test]
fn wheel_is_passed_through_when_focused() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    let frame = m.sample(&RawInput {
        wheel: [1.0, -2.0],
        ..Default::default()
    });
    assert_eq!(frame.wheel, [1.0, -2.0]);
}

#[test]
fn nan_wheel_components_are_zeroed() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    let frame = m.sample(&RawInput {
        wheel: [f32::NAN, 3.0],
        ..Default::default()
    });
    assert_eq!(frame.wheel, [0.0, 3.0]);
}

#[test]
fn frame_focus_mirrors_the_raw_state() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    assert!(m.sample(&RawInput::default()).focused);
    assert!(
        !m.sample(&RawInput {
            focused: false,
            ..Default::default()
        })
        .focused
    );
}

// ---- wheel tokens -----------------------------------------------------------------------

fn wheel_action(token: &str, wheel: [f32; 2], expected: f32) {
    let mut m = mapper(&[("scroll", &[token])]);
    let frame = m.sample(&RawInput {
        wheel,
        ..Default::default()
    });
    assert!(near(frame.value("scroll"), expected), "{token} {wheel:?}");
    assert_eq!(frame.held("scroll"), expected > 0.0);
}

table! { wheel_action;
    wheel_up_reads_positive_vertical_motion => ("Mouse:WheelUp", [0.0, 0.5], 0.5);
    wheel_up_clamps_to_one => ("Mouse:WheelUp", [0.0, 3.0], 1.0);
    wheel_up_ignores_downward_motion => ("Mouse:WheelUp", [0.0, -1.0], 0.0);
    wheel_down_reads_negative_vertical_motion => ("Mouse:WheelDown", [0.0, -0.25], 0.25);
    wheel_down_ignores_upward_motion => ("Mouse:WheelDown", [0.0, 1.0], 0.0);
    wheel_right_reads_positive_horizontal_motion => ("Mouse:WheelRight", [0.75, 0.0], 0.75);
    wheel_left_reads_negative_horizontal_motion => ("Mouse:WheelLeft", [-0.3, 0.0], 0.3);
    wheel_left_ignores_vertical_motion => ("Mouse:WheelLeft", [0.0, 5.0], 0.0);
}

#[test]
fn wheel_actions_are_silent_when_the_window_is_unfocused() {
    let mut m = mapper(&[("scroll", &["Mouse:WheelUp"])]);
    let frame = m.sample(&RawInput {
        wheel: [0.0, 2.0],
        focused: false,
        ..Default::default()
    });
    assert!(!frame.held("scroll"));
    assert_eq!(frame.wheel, [0.0, 0.0]);
}

// ---- gamepad axes -----------------------------------------------------------------------

fn axis_value(token: &str, axis: &str, raw: f32, expected: f32) {
    let mut m = mapper(&[("move", &[token])]);
    let frame = m.sample(&pad(&[(axis, raw)]));
    assert!(
        near(frame.value("move"), expected),
        "{token} {raw} -> {}",
        frame.value("move")
    );
}

table! { axis_value;
    axis_inside_the_deadzone_is_zero => ("Gamepad:LeftXPositive", "LeftX", 0.1, 0.0);
    axis_exactly_at_the_deadzone_is_zero => ("Gamepad:LeftXPositive", "LeftX", 0.2, 0.0);
    axis_just_past_the_deadzone_is_rescaled => ("Gamepad:LeftXPositive", "LeftX", 0.21, 0.0125);
    axis_mid_range_is_rescaled => ("Gamepad:LeftXPositive", "LeftX", 0.6, 0.5);
    axis_full_deflection_is_one => ("Gamepad:LeftXPositive", "LeftX", 1.0, 1.0);
    axis_beyond_full_deflection_clamps => ("Gamepad:LeftXPositive", "LeftX", 1.5, 1.0);
    negative_token_reads_negative_deflection => ("Gamepad:LeftXNegative", "LeftX", -0.6, 0.5);
    negative_token_ignores_positive_deflection => ("Gamepad:LeftXNegative", "LeftX", 0.9, 0.0);
    positive_token_ignores_negative_deflection => ("Gamepad:LeftXPositive", "LeftX", -0.9, 0.0);
    vertical_axis_is_read_independently => ("Gamepad:LeftYPositive", "LeftY", 1.0, 1.0);
    right_stick_axis_is_read_independently => ("Gamepad:RightYNegative", "RightY", -1.0, 1.0);
    trigger_is_an_axis_with_the_same_curve => ("Gamepad:RightTrigger", "RightTrigger", 0.9, 0.875);
    nan_axis_reads_as_zero => ("Gamepad:LeftXPositive", "LeftX", f32::NAN, 0.0);
    infinite_axis_reads_as_zero => ("Gamepad:LeftXPositive", "LeftX", f32::INFINITY, 0.0);
}

#[test]
fn axis_for_another_stick_does_not_leak() {
    let mut m = mapper(&[("move", &["Gamepad:LeftXPositive"])]);
    assert_eq!(m.sample(&pad(&[("RightX", 1.0)])).value("move"), 0.0);
}

#[test]
fn custom_deadzone_changes_the_rescaling() {
    let mut m = mapper(&[("move", &["Gamepad:LeftXPositive"])]);
    m.set_deadzone(0.5).unwrap();
    assert!(near(m.sample(&pad(&[("LeftX", 0.75)])).value("move"), 0.5));
}

#[test]
fn zero_deadzone_passes_the_raw_axis() {
    let mut m = mapper(&[("move", &["Gamepad:LeftXPositive"])]);
    m.set_deadzone(0.0).unwrap();
    assert!(near(m.sample(&pad(&[("LeftX", 0.05)])).value("move"), 0.05));
}

#[test]
fn analog_press_edge_happens_when_the_axis_leaves_the_deadzone() {
    let mut m = mapper(&[("move", &["Gamepad:LeftXPositive"])]);
    assert!(!m.sample(&pad(&[("LeftX", 0.15)])).pressed("move"));
    assert!(m.sample(&pad(&[("LeftX", 0.4)])).pressed("move"));
    assert!(!m.sample(&pad(&[("LeftX", 0.9)])).pressed("move"));
    assert!(m.sample(&pad(&[("LeftX", 0.1)])).released("move"));
}

#[test]
fn opposite_stick_directions_are_independent_actions() {
    let mut m = mapper(&[
        ("left", &["Gamepad:LeftXNegative"]),
        ("right", &["Gamepad:LeftXPositive"]),
    ]);
    let frame = m.sample(&pad(&[("LeftX", -1.0)]));
    assert!(frame.held("left") && !frame.held("right"));
}

#[test]
fn keyboard_and_stick_share_an_action_by_taking_the_larger_value() {
    let mut m = mapper(&[("right", &["Key:D", "Gamepad:LeftXPositive"])]);
    let mut raw = pad(&[("LeftX", 0.6)]);
    raw.down.insert("Key:D".into());
    assert_eq!(m.sample(&raw).value("right"), 1.0);
}

#[test]
fn gamepad_bindings_are_ignored_while_disconnected() {
    let mut m = mapper(&[("move", &["Gamepad:LeftXPositive", "Gamepad:A"])]);
    let mut raw = pad(&[("LeftX", 1.0)]);
    raw.down.insert("Gamepad:A".into());
    raw.gamepad_connected = false;
    assert!(m.sample(&raw).held.is_empty());
}

#[test]
fn gamepad_buttons_work_while_connected() {
    let mut m = mapper(&[("jump", &["Gamepad:A"])]);
    let mut raw = pad(&[]);
    raw.down.insert("Gamepad:A".into());
    assert!(m.sample(&raw).pressed("jump"));
}

#[test]
fn keyboard_works_without_a_gamepad() {
    let mut m = mapper(&[("jump", &["Key:Space", "Gamepad:A"])]);
    assert!(m.sample(&raw_down(&["Key:Space"])).pressed("jump"));
}

// ---- gamepad hot plug -----------------------------------------------------------------------

#[test]
fn first_sample_with_a_connected_held_button_counts_as_a_press() {
    let mut m = mapper(&[("jump", &["Gamepad:A"])]);
    let mut raw = pad(&[]);
    raw.down.insert("Gamepad:A".into());
    assert!(m.sample(&raw).pressed("jump"));
}

#[test]
fn button_held_through_a_reconnect_is_quarantined_until_release() {
    let mut m = mapper(&[("jump", &["Gamepad:A"])]);
    let mut raw = pad(&[]);
    m.sample(&raw);
    raw.gamepad_connected = false;
    m.sample(&raw);
    raw.gamepad_connected = true;
    raw.down.insert("Gamepad:A".into());
    assert!(!m.sample(&raw).held("jump"));
    assert!(!m.sample(&raw).held("jump"));
    raw.down.clear();
    m.sample(&raw);
    raw.down.insert("Gamepad:A".into());
    assert!(m.sample(&raw).pressed("jump"));
}

#[test]
fn disconnecting_while_holding_releases_the_action() {
    let mut m = mapper(&[("jump", &["Gamepad:A"])]);
    let mut raw = pad(&[]);
    raw.down.insert("Gamepad:A".into());
    m.sample(&raw);
    raw.gamepad_connected = false;
    let frame = m.sample(&raw);
    assert!(frame.released("jump") && !frame.held("jump"));
}

#[test]
fn keyboard_actions_survive_a_gamepad_reconnect() {
    let mut m = mapper(&[("jump", &["Key:Space", "Gamepad:A"])]);
    let mut raw = raw_down(&["Key:Space"]);
    m.sample(&raw);
    raw.gamepad_connected = true;
    let frame = m.sample(&raw);
    assert!(frame.held("jump") && !frame.pressed("jump"));
}

#[test]
fn axis_deflected_at_reconnect_stays_quiet_until_it_returns_to_rest() {
    let mut m = mapper(&[("move", &["Gamepad:LeftXPositive"])]);
    m.sample(&RawInput::default());
    let mut raw = pad(&[("LeftX", 1.0)]);
    assert!(!m.sample(&raw).held("move"));
    raw.axes.insert("LeftX".into(), 0.0);
    m.sample(&raw);
    raw.axes.insert("LeftX".into(), 0.8);
    assert!(m.sample(&raw).pressed("move"));
}

// ---- focus ------------------------------------------------------------------------------------

#[test]
fn losing_focus_releases_held_actions_immediately() {
    let mut m = mapper(&[("move", &["Key:A"])]);
    let mut raw = raw_down(&["Key:A"]);
    m.sample(&raw);
    raw.focused = false;
    let frame = m.sample(&raw);
    assert!(frame.released("move") && !frame.held("move"));
    assert_eq!(frame.value("move"), 0.0);
}

#[test]
fn key_held_through_focus_return_stays_quiet_until_released() {
    let mut m = mapper(&[("move", &["Key:A"])]);
    let mut raw = raw_down(&["Key:A"]);
    m.sample(&raw);
    raw.focused = false;
    m.sample(&raw);
    raw.focused = true;
    assert!(!m.sample(&raw).held("move"));
    assert!(!m.sample(&raw).pressed("move"));
    raw.down.clear();
    m.sample(&raw);
    raw.down.insert("Key:A".into());
    assert!(m.sample(&raw).pressed("move"));
}

#[test]
fn key_pressed_while_unfocused_is_ignored_even_after_focus_returns() {
    let mut m = mapper(&[("move", &["Key:A"])]);
    let mut raw = raw_down(&["Key:A"]);
    raw.focused = false;
    assert!(!m.sample(&raw).held("move"));
    raw.focused = true;
    assert!(!m.sample(&raw).held("move"));
    raw.down.clear();
    m.sample(&raw);
    raw.down.insert("Key:A".into());
    assert!(m.sample(&raw).pressed("move"));
}

#[test]
fn unfocused_frames_never_report_pressed() {
    let mut m = mapper(&[("a", &["Key:A"]), ("b", &["Key:B"])]);
    let frames: Vec<_> = (0..5)
        .map(|_| {
            m.sample(&RawInput {
                focused: false,
                down: names(&["Key:A", "Key:B"]),
                ..Default::default()
            })
        })
        .collect();
    assert!(
        frames
            .iter()
            .all(|f| f.pressed.is_empty() && f.held.is_empty())
    );
}

#[test]
fn first_focused_frame_quarantines_a_key_that_is_already_down() {
    let mut m = mapper(&[("move", &["Key:A"])]);
    m.sample(&RawInput {
        focused: false,
        ..Default::default()
    });
    assert!(!m.sample(&raw_down(&["Key:A"])).held("move"));
}

#[test]
fn key_pressed_a_frame_after_focus_returns_registers_normally() {
    let mut m = mapper(&[("move", &["Key:A"])]);
    m.sample(&RawInput {
        focused: false,
        ..Default::default()
    });
    m.sample(&RawInput::default());
    assert!(m.sample(&raw_down(&["Key:A"])).pressed("move"));
}

#[test]
fn only_actions_held_at_focus_loss_are_quarantined() {
    let mut m = mapper(&[("a", &["Key:A"]), ("b", &["Key:B"])]);
    let mut raw = raw_down(&["Key:A"]);
    m.sample(&raw);
    raw.focused = false;
    m.sample(&raw);
    raw.focused = true;
    m.sample(&raw);
    raw.down.insert("Key:B".into());
    let frame = m.sample(&raw);
    assert!(frame.pressed("b") && !frame.held("a"));
}

#[test]
fn repeated_focus_flicker_never_produces_stuck_input() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    let mut raw = raw_down(&["Key:A"]);
    for i in 0..20 {
        raw.focused = i % 2 == 1;
        let frame = m.sample(&raw);
        assert!(!frame.held("a"), "frame {i}");
    }
}

// ---- rebinding -----------------------------------------------------------------------------------

#[test]
fn rebinding_a_held_action_quarantines_it_until_release() {
    let mut m = mapper(&[("move", &["Key:A"])]);
    let mut raw = raw_down(&["Key:A"]);
    m.sample(&raw);
    assert!(
        m.rebind(bind(&[("move", &["Key:A", "Key:B"])]), false)
            .errors
            .is_empty()
    );
    assert!(!m.sample(&raw).held("move"));
    raw.down.clear();
    m.sample(&raw);
    raw.down.insert("Key:B".into());
    assert!(m.sample(&raw).pressed("move"));
}

#[test]
fn rebinding_moves_an_action_to_a_new_key_and_silences_the_old_one() {
    let mut m = mapper(&[("jump", &["Key:Space"])]);
    m.rebind(bind(&[("jump", &["Key:J"])]), false);
    assert!(!m.sample(&raw_down(&["Key:Space"])).held("jump"));
    assert!(m.sample(&raw_down(&["Key:J"])).pressed("jump"));
}

#[test]
fn rebinding_an_idle_action_is_effective_immediately() {
    let mut m = mapper(&[("jump", &["Key:Space"])]);
    m.sample(&RawInput::default());
    m.rebind(bind(&[("jump", &["Key:J"])]), false);
    assert!(m.sample(&raw_down(&["Key:J"])).pressed("jump"));
}

#[test]
fn invalid_rebind_reports_errors_and_keeps_the_old_bindings() {
    let mut m = mapper(&[("jump", &["Key:Space"])]);
    let result = m.rebind(bind(&[("jump", &["Key:Nope"])]), false);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(m.bindings()["jump"], vec!["Key:Space"]);
    assert!(m.sample(&raw_down(&["Key:Space"])).pressed("jump"));
}

#[test]
fn invalid_rebind_does_not_quarantine_held_actions() {
    let mut m = mapper(&[("jump", &["Key:Space"])]);
    let raw = raw_down(&["Key:Space"]);
    m.sample(&raw);
    m.rebind(bind(&[("jump", &[])]), false);
    assert!(m.sample(&raw).held("jump"));
}

#[test]
fn strict_rebind_refuses_conflicting_keys() {
    let mut m = mapper(&[("a", &["Key:A"]), ("b", &["Key:B"])]);
    let result = m.rebind(bind(&[("a", &["Key:A"]), ("b", &["Key:A"])]), true);
    assert_eq!(result.errors, vec!["Binding conflict: Key:A"]);
    assert_eq!(m.bindings()["b"], vec!["Key:B"]);
}

#[test]
fn lenient_rebind_applies_conflicts_and_reports_them() {
    let mut m = mapper(&[("a", &["Key:A"]), ("b", &["Key:B"])]);
    let result = m.rebind(bind(&[("a", &["Key:A"]), ("b", &["Key:A"])]), false);
    assert!(result.errors.is_empty());
    assert_eq!(result.conflicts["Key:A"], vec!["a", "b"]);
    assert_eq!(m.bindings()["b"], vec!["Key:A"]);
}

#[test]
fn rebinding_can_remove_an_action_entirely() {
    let mut m = mapper(&[("a", &["Key:A"]), ("b", &["Key:B"])]);
    m.rebind(bind(&[("a", &["Key:A"])]), false);
    let frame = m.sample(&raw_down(&["Key:B"]));
    assert!(frame.held.is_empty());
    assert!(!frame.values.contains_key("b"));
}

#[test]
fn rebinding_can_add_a_new_action() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    m.rebind(bind(&[("a", &["Key:A"]), ("b", &["Key:B"])]), false);
    assert!(m.sample(&raw_down(&["Key:B"])).pressed("b"));
}

#[test]
fn clear_forgets_history_so_a_held_key_presses_again() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    let raw = raw_down(&["Key:A"]);
    m.sample(&raw);
    m.clear();
    assert!(m.sample(&raw).pressed("a"));
}

#[test]
fn clear_drops_quarantines() {
    let mut m = mapper(&[("a", &["Key:A"])]);
    let mut raw = raw_down(&["Key:A"]);
    m.sample(&raw);
    raw.focused = false;
    m.sample(&raw);
    m.clear();
    raw.focused = true;
    assert!(m.sample(&raw).pressed("a"));
}

// ---- injected pulses --------------------------------------------------------------------------------

fn merge(
    physical: &ActionFrame,
    injected: &[&str],
    previous: &mut BTreeSet<String>,
) -> ActionFrame {
    merge_input(physical, &names(injected), previous)
}

#[test]
fn injected_action_pulses_for_one_tick() {
    let mut previous = BTreeSet::new();
    let physical = ActionFrame::default();
    let first = merge(&physical, &["interact"], &mut previous);
    assert!(first.pressed("interact") && first.held("interact"));
    assert_eq!(first.value("interact"), 1.0);
    let second = merge(&physical, &[], &mut previous);
    assert!(second.released("interact") && !second.held("interact"));
    let third = merge(&physical, &[], &mut previous);
    assert!(third.released.is_empty());
}

#[test]
fn continuous_injection_presses_once_and_stays_held() {
    let mut previous = BTreeSet::new();
    let physical = ActionFrame::default();
    let frames: Vec<_> = (0..4)
        .map(|_| merge(&physical, &["go"], &mut previous))
        .collect();
    assert!(frames[0].pressed("go"));
    assert!(frames[1..].iter().all(|f| !f.pressed("go") && f.held("go")));
}

#[test]
fn injection_on_top_of_a_physical_hold_adds_no_press() {
    let mut previous = BTreeSet::new();
    let physical = ActionFrame {
        held: names(&["move"]),
        pressed: names(&["move"]),
        ..Default::default()
    };
    let first = merge(&physical, &[], &mut previous);
    assert!(first.pressed("move"));
    let held_only = ActionFrame {
        held: names(&["move"]),
        ..Default::default()
    };
    let second = merge(&held_only, &["move"], &mut previous);
    assert!(!second.pressed("move") && second.held("move"));
}

#[test]
fn physical_hold_outlives_the_injected_pulse() {
    let mut previous = BTreeSet::new();
    let held = ActionFrame {
        held: names(&["move"]),
        ..Default::default()
    };
    merge(&held, &["move"], &mut previous);
    let after = merge(&held, &[], &mut previous);
    assert!(after.held("move") && !after.released("move"));
}

#[test]
fn injection_overrides_the_analog_value() {
    let mut previous = BTreeSet::new();
    let physical = ActionFrame {
        held: names(&["move"]),
        values: BTreeMap::from([("move".to_string(), 0.3)]),
        ..Default::default()
    };
    assert_eq!(
        merge(&physical, &["move"], &mut previous).value("move"),
        1.0
    );
}

#[test]
fn merge_preserves_pointer_wheel_and_focus() {
    let mut previous = BTreeSet::new();
    let physical = ActionFrame {
        pointer: [7.0, 8.0],
        wheel: [0.0, 1.0],
        focused: false,
        ..Default::default()
    };
    let merged = merge(&physical, &["x"], &mut previous);
    assert_eq!(
        (merged.pointer, merged.wheel, merged.focused),
        ([7.0, 8.0], [0.0, 1.0], false)
    );
}

#[test]
fn merge_of_nothing_is_empty_and_clears_history() {
    let mut previous = names(&["old"]);
    let merged = merge(&ActionFrame::default(), &[], &mut previous);
    assert!(merged.released("old"));
    assert!(previous.is_empty());
}

#[test]
fn merge_tracks_multiple_injected_actions_independently() {
    let mut previous = BTreeSet::new();
    let physical = ActionFrame::default();
    merge(&physical, &["a", "b"], &mut previous);
    let next = merge(&physical, &["b", "c"], &mut previous);
    assert_eq!(next.pressed, names(&["c"]));
    assert_eq!(next.released, names(&["a"]));
    assert_eq!(next.held, names(&["b", "c"]));
}
