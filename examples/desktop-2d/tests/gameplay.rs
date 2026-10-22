//! Headless behavioural tests for the lab's interactive controller. They drive the
//! real `engine_project::Runtime` with injected actions: no window, GPU or device.
use desktop_2d_gameplay::{bindings, configure};
use engine_audio::{AudioConfig, AudioEngine, Bus};
use engine_project::{
    ActionFrame, Project, Runtime,
    runtime::{ControlLocks, GameState, InputSettings, Persistence},
    ui::UiModel,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

const DT: f32 = 1.0 / 60.0;
const SPEED: f32 = 3.0;

fn manifest(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(name)
}
fn user_data() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/example-test-data");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}
fn frame(actions: &[&str]) -> ActionFrame {
    let mut input = ActionFrame::default();
    for action in actions {
        input.pressed.insert((*action).into());
        input.held.insert((*action).into());
        input.values.insert((*action).into(), 1.0);
    }
    input
}
fn held(actions: &[(&str, f32)]) -> ActionFrame {
    let mut input = ActionFrame::default();
    for (action, value) in actions {
        input.held.insert((*action).into());
        input.values.insert((*action).into(), *value);
    }
    input
}

struct Lab {
    runtime: Runtime,
    audio: Option<AudioEngine>,
}
impl Lab {
    fn new(root: &Path) -> Self {
        Self::build("project.json", root, false)
    }
    fn with_audio(root: &Path) -> Self {
        Self::build("project.json", root, true)
    }
    fn build(project: &str, root: &Path, audio: bool) -> Self {
        let mut runtime = Runtime::new(Project::load(manifest(project)).unwrap()).unwrap();
        runtime.set_user_data_root(root.into()).unwrap();
        configure(runtime.app_mut()).unwrap();
        let audio = audio.then(|| {
            let engine = AudioEngine::offline(AudioConfig::default()).unwrap();
            runtime.set_audio(engine.control()).unwrap();
            engine
        });
        let mut lab = Self { runtime, audio };
        lab.idle(1);
        lab
    }
    fn act(&mut self, actions: &[&str]) {
        self.runtime.tick(DT, frame(actions)).unwrap();
    }
    fn hold(&mut self, actions: &[(&str, f32)]) {
        self.runtime.tick(DT, held(actions)).unwrap();
    }
    fn idle(&mut self, ticks: usize) {
        for _ in 0..ticks {
            self.runtime.tick(DT, ActionFrame::default()).unwrap();
        }
    }
    fn pos(&self) -> [f32; 3] {
        self.runtime.position("actor:player").unwrap()
    }
    fn teleport(&mut self, id: &str, x: f32, y: f32) {
        let entity = self.runtime.entity(id).unwrap();
        let mut transform = self
            .runtime
            .app_mut()
            .world_mut()
            .get_mut::<engine_project::Transform>(entity)
            .unwrap();
        transform.position[0] = x;
        transform.position[1] = y;
        // The next tick synchronises colliders; a second makes overlaps observable.
        self.idle(2);
    }
    fn state(&self) -> Value {
        Value::Object(self.runtime.app().world().resource::<GameState>().0.clone())
    }
    fn panels(&self) -> Vec<(String, String)> {
        self.runtime
            .app()
            .world()
            .resource::<UiModel>()
            .panels()
            .iter()
            .map(|p| (p.options.id.clone(), p.options.text.clone()))
            .collect()
    }
    fn panel_text(&self, id: &str) -> Option<String> {
        self.panels().into_iter().find(|p| p.0 == id).map(|p| p.1)
    }
    fn modal_open(&self) -> bool {
        self.runtime
            .app()
            .world()
            .resource::<UiModel>()
            .modal_active()
    }
    fn interact_at(&mut self, id: &str, x: f32, y: f32) {
        self.teleport(id, x, y);
        self.act(&["interact"]);
    }
    fn dismiss(&mut self) {
        self.act(&["ui_confirm"]);
        self.idle(2);
    }
    fn has_room_b(&self) -> bool {
        self.runtime.entity("room:B").is_some()
    }
    fn logs(&self) -> Vec<String> {
        self.runtime.snapshot().logs
    }
}

fn moved(lab: &mut Lab, actions: &[(&str, f32)]) -> [f32; 3] {
    let before = lab.pos();
    lab.hold(actions);
    let after = lab.pos();
    [
        after[0] - before[0],
        after[1] - before[1],
        after[2] - before[2],
    ]
}
fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

// ---------------------------------------------------------------------------
// Movement
// ---------------------------------------------------------------------------

#[test]
fn no_input_leaves_the_player_where_it_started() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let start = lab.pos();
    lab.idle(30);
    assert_eq!(lab.pos(), start);
    assert_eq!(start, [-4.0, 0.0, 0.0]);
}
#[test]
fn cardinal_right_moves_three_units_per_second() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let d = moved(&mut lab, &[("move_right", 1.0)]);
    assert!(close(d[0], SPEED * DT) && d[1] == 0.0);
}
#[test]
fn cardinal_left_moves_negative_x() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let d = moved(&mut lab, &[("move_left", 1.0)]);
    assert!(close(d[0], -SPEED * DT) && d[1] == 0.0);
}
#[test]
fn cardinal_up_moves_positive_y() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let d = moved(&mut lab, &[("move_up", 1.0)]);
    assert!(close(d[1], SPEED * DT) && d[0] == 0.0);
}
#[test]
fn cardinal_down_moves_negative_y() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let d = moved(&mut lab, &[("move_down", 1.0)]);
    assert!(close(d[1], -SPEED * DT) && d[0] == 0.0);
}
#[test]
fn opposite_inputs_cancel_each_other() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let start = lab.pos();
    lab.hold(&[("move_left", 1.0), ("move_right", 1.0)]);
    lab.hold(&[("move_up", 1.0), ("move_down", 1.0)]);
    assert_eq!(lab.pos(), start);
}
#[test]
fn diagonal_input_is_normalised() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let d = moved(&mut lab, &[("move_right", 1.0), ("move_up", 1.0)]);
    let length = d[0].hypot(d[1]);
    assert!(close(length, SPEED * DT), "length {length}");
    assert!(close(d[0], d[1]) && d[0] > 0.0);
}
#[test]
fn diagonal_speed_equals_cardinal_speed_for_every_pairing() {
    let dir = user_data();
    for pair in [
        [("move_left", 1.0), ("move_up", 1.0)],
        [("move_left", 1.0), ("move_down", 1.0)],
        [("move_right", 1.0), ("move_down", 1.0)],
    ] {
        let mut lab = Lab::new(dir.path());
        let d = moved(&mut lab, &pair);
        assert!(close(d[0].hypot(d[1]), SPEED * DT), "{pair:?}");
    }
}
#[test]
fn partial_stick_deflection_scales_speed() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let d = moved(&mut lab, &[("move_right", 0.5)]);
    assert!(close(d[0], 0.5 * SPEED * DT), "{d:?}");
}
#[test]
fn z_is_preserved_while_moving() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let entity = lab.runtime.entity("actor:player").unwrap();
    lab.runtime
        .app_mut()
        .world_mut()
        .get_mut::<engine_project::Transform>(entity)
        .unwrap()
        .position[2] = 5.5;
    for _ in 0..20 {
        lab.hold(&[("move_right", 1.0), ("move_up", 1.0)]);
    }
    assert_eq!(lab.pos()[2], 5.5);
}
#[test]
fn identical_input_sequences_produce_identical_positions() {
    let dir = user_data();
    let run = |root: &Path| {
        let mut lab = Lab::new(root);
        for step in 0..90 {
            match step % 3 {
                0 => lab.hold(&[("move_right", 1.0), ("move_up", 1.0)]),
                1 => lab.hold(&[("move_down", 1.0)]),
                _ => lab.idle(1),
            }
        }
        lab.pos().map(f32::to_bits)
    };
    let second = user_data();
    assert_eq!(run(dir.path()), run(second.path()));
}
#[test]
fn solid_boundary_stops_the_player() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.teleport("actor:player", 7.0, -2.0);
    for _ in 0..120 {
        lab.hold(&[("move_right", 1.0)]);
    }
    // The right wall is centred at x=8 with half width 0.25; the player half width is 0.325.
    assert!(lab.pos()[0] < 7.5, "x {}", lab.pos()[0]);
    assert!(lab.pos()[0] > 7.0);
}
#[test]
fn raised_obstacle_blocks_upward_motion() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.teleport("actor:player", 0.0, -1.0);
    for _ in 0..120 {
        lab.hold(&[("move_up", 1.0)]);
    }
    // Obstacle spans y 0.85..2.35, so the player cannot pass its lower edge.
    assert!(lab.pos()[1] < 0.85, "y {}", lab.pos()[1]);
}
#[test]
fn modal_panel_blocks_movement_beneath_it() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["journal"]);
    assert!(lab.modal_open());
    let start = lab.pos();
    for _ in 0..10 {
        lab.hold(&[("move_right", 1.0)]);
    }
    assert_eq!(lab.pos(), start);
}
#[test]
fn authored_scene_control_lock_blocks_movement_until_cancelled() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["scene_demo"]);
    assert!(
        lab.runtime
            .app()
            .world()
            .resource::<ControlLocks>()
            .0
            .contains("authored-scene")
    );
    let start = lab.pos();
    lab.hold(&[("move_right", 1.0)]);
    assert_eq!(lab.pos(), start);
    lab.act(&["ui_back"]);
    lab.idle(2);
    assert!(
        lab.runtime
            .app()
            .world()
            .resource::<ControlLocks>()
            .0
            .is_empty()
    );
    assert!(moved(&mut lab, &[("move_right", 1.0)])[0] > 0.0);
}
#[test]
fn followers_trail_the_moving_player() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let before = lab.runtime.position("actor:dove1").unwrap();
    for _ in 0..90 {
        lab.hold(&[("move_right", 1.0)]);
    }
    let after = lab.runtime.position("actor:dove1").unwrap();
    assert!(after[0] > before[0] + 2.0, "{before:?} -> {after:?}");
}

// ---------------------------------------------------------------------------
// Dialogue, quest and progression flow
// ---------------------------------------------------------------------------

#[test]
fn book_marker_shows_the_locked_journal() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.interact_at("actor:player", -2.0, 0.0);
    let text = lab.panel_text("journal").expect("journal panel");
    assert!(text.starts_with("UNREVEALED"));
    assert!(!text.contains("Original placeholder"));
}
#[test]
fn journal_hotkey_opens_page_without_standing_on_a_marker() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["journal"]);
    assert!(lab.panel_text("journal").is_some());
    lab.act(&["ui_back"]);
    lab.idle(1);
    assert!(lab.panel_text("journal").is_none());
}
#[test]
fn story_demo_reveals_the_passage_once_with_story_provenance() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["story_demo"]);
    lab.act(&["story_demo"]);
    let state = lab.state();
    assert_eq!(state["reveals"].as_array().unwrap().len(), 1);
    assert_eq!(
        state["provenance"]["passage:sample"],
        serde_json::json!(["story:sample"])
    );
    lab.act(&["journal"]);
    assert!(
        lab.panel_text("journal")
            .unwrap()
            .contains("Original placeholder")
    );
}
#[test]
fn quest_marker_offers_accept_and_later() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.interact_at("actor:player", 2.0, 0.0);
    assert!(lab.panel_text("quest").unwrap().contains("DEVELOPER QUEST"));
    let panels = lab.runtime.app().world().resource::<UiModel>();
    let quest = panels
        .panels()
        .iter()
        .find(|p| p.options.id == "quest")
        .unwrap();
    assert_eq!(quest.options.choices, ["Accept", "Later"]);
}
#[test]
fn accepting_the_quest_activates_it_and_records_choice_one() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.interact_at("actor:player", 2.0, 0.0);
    lab.act(&["ui_confirm"]);
    lab.idle(1);
    let state = lab.state();
    assert_eq!(state["quests"]["sample"], "active");
    assert_eq!(state["acceptedChoice"], 1);
    assert!(lab.panel_text("quest").is_none());
}
#[test]
fn choosing_later_keeps_the_quest_available_and_records_choice_two() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.interact_at("actor:player", 2.0, 0.0);
    lab.act(&["ui_down"]);
    lab.act(&["ui_confirm"]);
    lab.idle(1);
    let state = lab.state();
    assert_eq!(state["quests"]["sample"], "available");
    assert_eq!(state["acceptedChoice"], 2);
}
#[test]
fn variant_picker_switches_between_male_and_female() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    assert_eq!(lab.state()["variant"], "male");
    lab.act(&["choose_variant"]);
    lab.act(&["ui_down"]);
    lab.act(&["ui_confirm"]);
    lab.idle(1);
    assert_eq!(lab.state()["variant"], "female");
    lab.act(&["choose_variant"]);
    lab.act(&["ui_confirm"]);
    lab.idle(1);
    assert_eq!(lab.state()["variant"], "male");
}
#[test]
fn reward_marker_before_completion_only_offers_the_quest() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["story_demo"]);
    lab.interact_at("actor:player", 2.0, 0.0);
    assert!(lab.panel_text("dialogue").is_none());
    assert_eq!(lab.state()["quests"]["sample"], "available");
}
#[test]
fn full_quest_route_across_both_rooms_awards_one_shared_passage() {
    for variant_steps in [0usize, 1] {
        let dir = user_data();
        let mut lab = Lab::new(dir.path());
        if variant_steps == 1 {
            lab.act(&["choose_variant"]);
            lab.act(&["ui_down"]);
            lab.act(&["ui_confirm"]);
            lab.idle(1);
        }
        lab.interact_at("actor:player", 2.0, 0.0);
        lab.act(&["ui_confirm"]);
        lab.idle(1);
        assert_eq!(lab.state()["quests"]["sample"], "active");

        lab.interact_at("actor:player", 6.0, 0.0);
        lab.idle(3);
        assert!(lab.has_room_b(), "exit did not reach room B");
        lab.interact_at("actor:player", 1.0, -2.0);
        assert!(
            lab.panel_text("dialogue")
                .unwrap()
                .contains("OBJECTIVE OBSERVED")
        );
        lab.dismiss();
        assert_eq!(lab.state()["quests"]["sample"], "completed");

        lab.interact_at("actor:player", 6.0, 0.0);
        lab.idle(3);
        assert!(!lab.has_room_b(), "exit did not return to room A");
        lab.interact_at("actor:player", 2.0, 0.0);
        assert!(
            lab.panel_text("dialogue")
                .unwrap()
                .contains("REWARD RECORDED")
        );
        lab.dismiss();
        // Interacting again must not award or reveal anything a second time.
        lab.interact_at("actor:player", 2.0, 0.0);
        lab.dismiss();
        let state = lab.state();
        assert_eq!(state["quests"]["sample"], "reward_recorded");
        assert_eq!(state["reveals"], serde_json::json!(["passage:sample"]));
        assert_eq!(
            state["provenance"]["passage:sample"],
            serde_json::json!(["quest:sample"])
        );
        assert_eq!(state["followers"], 2);
        lab.act(&["story_demo"]);
        assert_eq!(
            lab.state()["provenance"]["passage:sample"],
            serde_json::json!(["quest:sample", "story:sample"])
        );
        assert_eq!(
            lab.state()["variant"],
            if variant_steps == 1 { "female" } else { "male" }
        );
    }
}
#[test]
fn exit_marker_moves_to_room_b_retaining_the_player_entity() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let player = lab.runtime.entity("actor:player").unwrap();
    lab.interact_at("actor:player", 6.0, 0.0);
    lab.idle(3);
    assert!(lab.has_room_b());
    assert_eq!(lab.runtime.entity("actor:player"), Some(player));
    assert_eq!(lab.state()["room"], "room:B");
}
#[test]
fn missing_scene_request_is_rejected_and_the_room_is_unchanged() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let error = lab
        .runtime
        .request_scene("asset:missing-room", "entry", "actor:player");
    assert!(error.is_err());
    lab.idle(3);
    assert!(!lab.has_room_b());
    assert_eq!(lab.state()["room"], "room:A");
}
#[test]
fn ordinary_defeat_enters_retry_and_never_heals() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["defeat_demo"]);
    let state = lab.state();
    assert_eq!(state["scene"], "retry");
    assert_eq!(state["healCompletions"], 0);
    lab.idle(240);
    assert_eq!(lab.state()["scene"], "retry");
    assert_eq!(lab.state()["healCompletions"], 0);
}
#[test]
fn cancelling_the_authored_scene_awards_no_healing() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["scene_demo"]);
    assert_eq!(lab.state()["scene"], "injury");
    lab.act(&["ui_back"]);
    lab.idle(2);
    let state = lab.state();
    assert_eq!(state["scene"], "idle");
    assert_eq!(state["healCompletions"], 0);
    assert!(
        !state["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "scene:healed")
    );
}
#[test]
fn authored_scene_heals_exactly_once_after_three_seconds() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["scene_demo"]);
    lab.idle(170);
    assert_eq!(lab.state()["scene"], "injury", "before three seconds");
    assert_eq!(lab.state()["healCompletions"], 0);
    lab.idle(30);
    assert_eq!(lab.state()["scene"], "idle");
    assert_eq!(lab.state()["healCompletions"], 1);
    assert_eq!(lab.state()["followers"], 2);
    // Replaying the demo after completion cannot repeat the award.
    lab.act(&["scene_demo"]);
    lab.idle(240);
    assert_eq!(lab.state()["healCompletions"], 1);
}
#[test]
fn defeat_after_scene_completion_does_not_reset_the_healing_record() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["scene_demo"]);
    lab.idle(200);
    lab.act(&["defeat_demo"]);
    assert_eq!(lab.state()["scene"], "retry");
    assert_eq!(lab.state()["healCompletions"], 1);
}
#[test]
fn new_game_resets_progress_and_returns_to_room_a() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["story_demo"]);
    lab.act(&["defeat_demo"]);
    assert_eq!(lab.state()["scene"], "retry");
    lab.act(&["new_game"]);
    lab.idle(3);
    let state = lab.state();
    assert_eq!(state["scene"], "idle");
    assert!(state["reveals"].as_array().unwrap().is_empty());
    assert_eq!(state["room"], "room:A");
}

// ---------------------------------------------------------------------------
// Saves, settings and restore
// ---------------------------------------------------------------------------

#[test]
fn resting_on_the_marker_writes_a_durable_save_and_confirms() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.teleport("actor:player", -4.0, 2.0);
    lab.act(&["rest"]);
    assert!(
        lab.panel_text("dialogue")
            .unwrap()
            .starts_with("SAVE CONFIRMED")
    );
    let saved = lab
        .runtime
        .app()
        .world()
        .resource::<Persistence>()
        .0
        .load("desktop-slot", None)
        .unwrap();
    assert_eq!(saved["room"], "room:A");
    assert_eq!(saved["flock"], true);
    assert_eq!(saved["position"]["x"], -4.0);
    assert_eq!(saved["position"]["y"], 2.0);
}
#[test]
fn resting_away_from_the_marker_does_not_save() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["rest"]);
    assert!(lab.panel_text("dialogue").is_none());
    assert!(
        lab.runtime
            .app()
            .world()
            .resource::<Persistence>()
            .0
            .load("desktop-slot", None)
            .is_err()
    );
}
#[test]
fn moving_after_rest_releases_the_gathered_flock() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.teleport("actor:player", -4.0, 2.0);
    lab.act(&["rest"]);
    lab.dismiss();
    assert_eq!(lab.state()["flock"], true);
    lab.hold(&[("move_right", 1.0)]);
    assert_eq!(lab.state()["flock"], false);
}
#[test]
fn new_restore_scenario_restores_progress_in_a_second_process_lifetime() {
    for female in [false, true] {
        let dir = user_data();
        {
            let mut lab = Lab::new(dir.path());
            if female {
                lab.act(&["choose_variant"]);
                lab.act(&["ui_down"]);
                lab.act(&["ui_confirm"]);
                lab.idle(1);
            }
            lab.interact_at("actor:player", 2.0, 0.0);
            lab.act(&["ui_confirm"]);
            lab.idle(1);
            lab.teleport("actor:player", -4.0, 2.0);
            lab.act(&["rest"]);
            lab.runtime.stop().unwrap();
        }
        // A new runtime on the same user-data root starts a fresh session, then
        // loads the durable save deliberately.
        let mut lab = Lab::new(dir.path());
        assert_eq!(lab.state()["quests"]["sample"], "available");
        lab.teleport("actor:player", 1.0, -3.0);
        lab.act(&["load"]);
        lab.idle(2);
        let state = lab.state();
        assert_eq!(state["quests"]["sample"], "active");
        assert_eq!(state["acceptedChoice"], 1);
        assert_eq!(state["variant"], if female { "female" } else { "male" });
        assert_eq!(lab.pos()[..2], [-4.0, 2.0]);
    }
}
#[test]
fn load_without_a_save_reports_failure_and_keeps_current_state() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["story_demo"]);
    let before = lab.state();
    lab.act(&["load"]);
    lab.idle(2);
    assert_eq!(lab.state(), before);
    assert!(lab.panel_text("hud").unwrap().contains("LOAD FAILED"));
}
#[test]
fn corrupt_save_is_rejected_without_replacing_live_progress() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["story_demo"]);
    let before = lab.state();
    let mut bad = before.as_object().unwrap().clone();
    bad.insert("version".into(), 99.into());
    lab.runtime
        .app()
        .world()
        .resource::<Persistence>()
        .0
        .save("desktop-slot", bad, None)
        .unwrap();
    lab.act(&["load"]);
    lab.idle(2);
    assert_eq!(lab.state(), before);
    assert!(lab.panel_text("hud").unwrap().contains("LOAD FAILED"));
}
#[test]
fn saved_position_inside_a_solid_is_refused_on_load() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    let mut doc = lab.state().as_object().unwrap().clone();
    doc.insert("position".into(), serde_json::json!({"x": 0.0, "y": 1.6}));
    lab.runtime
        .app()
        .world()
        .resource::<Persistence>()
        .0
        .save("desktop-slot", doc, None)
        .unwrap();
    let start = lab.pos();
    lab.act(&["load"]);
    lab.idle(2);
    assert_eq!(lab.pos(), start);
    assert!(lab.panel_text("hud").unwrap().contains("LOAD FAILED"));
}
#[test]
fn remapping_saves_the_complete_binding_map_and_audio_settings() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.act(&["remap"]);
    let settings = lab
        .runtime
        .app()
        .world()
        .resource::<Persistence>()
        .0
        .load_settings(None)
        .unwrap();
    assert_eq!(settings["interactRebound"], true);
    assert_eq!(settings["bindings"]["interact"][0], "Key:Q");
    assert_eq!(settings["bindings"]["move_left"][0], "Key:A");
    assert_eq!(
        settings["bindings"].as_object().unwrap().len(),
        bindings::defaults().len()
    );
    assert_eq!(settings["audio"]["music"], 0.25);
    assert_eq!(settings["reducedMotion"], true);
    let live = &lab
        .runtime
        .app()
        .world()
        .resource::<InputSettings>()
        .bindings;
    assert_eq!(live["interact"][0], "Key:Q");
}
#[test]
fn remapped_bindings_and_settings_persist_across_restart() {
    let dir = user_data();
    {
        let mut lab = Lab::new(dir.path());
        lab.act(&["remap"]);
        lab.runtime.stop().unwrap();
    }
    let lab = Lab::new(dir.path());
    let live = &lab
        .runtime
        .app()
        .world()
        .resource::<InputSettings>()
        .bindings;
    assert_eq!(live["interact"], ["Key:Q", "Gamepad:A"]);
    assert_eq!(live["move_right"], bindings::defaults()["move_right"]);
}

// ---------------------------------------------------------------------------
// Audio lifecycle (offline mixer)
// ---------------------------------------------------------------------------

#[test]
fn music_loop_starts_with_the_scene_and_is_released_on_stop() {
    let dir = user_data();
    let mut lab = Lab::with_audio(dir.path());
    let control = lab.audio.as_ref().unwrap().control();
    assert_eq!(control.voice_count(), 1);
    lab.runtime.stop().unwrap();
    assert_eq!(control.voice_count(), 0);
}
#[test]
fn rest_cue_adds_a_voice_and_mute_silences_the_master_bus() {
    let dir = user_data();
    let mut lab = Lab::with_audio(dir.path());
    let control = lab.audio.as_ref().unwrap().control();
    lab.teleport("actor:player", -4.0, 2.0);
    let voices = control.voice_count();
    lab.act(&["rest"]);
    assert!(control.voice_count() > voices);
    lab.dismiss();
    lab.act(&["mute"]);
    assert_eq!(control.bus_gain(Bus::Master).unwrap(), 0.0);
    lab.act(&["mute"]);
    assert!(control.bus_gain(Bus::Master).unwrap() > 0.0);
    lab.runtime.stop().unwrap();
    assert_eq!(control.voice_count(), 0);
}
#[test]
fn walkthrough_runs_without_audio_hardware_or_runtime_audio() {
    let dir = user_data();
    let mut lab = Lab::new(dir.path());
    lab.teleport("actor:player", -4.0, 2.0);
    lab.act(&["rest"]);
    assert!(
        lab.panel_text("dialogue")
            .unwrap()
            .starts_with("SAVE CONFIRMED")
    );
    assert!(lab.logs().iter().any(|l| l.contains("MUSIC UNAVAILABLE")));
}

// ---------------------------------------------------------------------------
// Authored manifests and registration
// ---------------------------------------------------------------------------

#[test]
fn every_manifest_loads_and_references_existing_rust_sources() {
    let manifests = [
        "project.json",
        "project-pure.json",
        "project-syntax.json",
        "project-test-new-male.json",
        "project-test-new-female.json",
        "project-test-restore-male.json",
        "project-test-restore-female.json",
    ];
    for name in manifests {
        let project = Project::load(manifest(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        for asset in project.assets.values().filter(|a| a.kind == "script") {
            assert_eq!(
                asset.path.extension().and_then(|e| e.to_str()),
                Some("rs"),
                "{name}: {}",
                asset.path.display()
            );
        }
    }
}
#[test]
fn pure_project_passes_its_native_progression_checks() {
    let dir = user_data();
    let lab = Lab::build("project-pure.json", dir.path(), false);
    assert!(lab.logs().iter().any(|l| l == "DESKTOP2D_PURE_PASS"));
}
#[test]
fn registered_scripts_cover_every_scene_script_reference() {
    use engine_project::ScriptRegistry;
    let dir = user_data();
    let lab = Lab::new(dir.path());
    let registry = lab.runtime.app().world().resource::<ScriptRegistry>();
    for name in [
        "asset:player",
        "asset:test-new-male",
        "asset:test-new-female",
        "asset:test-restore-male",
        "asset:test-restore-female",
        "asset:pure-tests",
    ] {
        assert!(registry.factories.contains_key(name), "{name}");
    }
    for entity in &lab.runtime.active_scene().entities {
        if let Some(script) = &entity.components.script {
            assert!(registry.factories.contains_key(&script.asset));
        }
    }
}
#[test]
fn bindings_defaults_match_the_authored_manifest_and_validate() {
    let defaults = bindings::defaults();
    let project = Project::load(manifest("project.json")).unwrap();
    assert_eq!(defaults, project.input_bindings);
    assert!(
        engine_project::input::validate_bindings(&defaults, false)
            .errors
            .is_empty()
    );
    for action in [
        "move_left",
        "move_right",
        "move_up",
        "move_down",
        "interact",
        "ui_confirm",
        "ui_back",
    ] {
        assert!(defaults.contains_key(action), "{action}");
    }
}
