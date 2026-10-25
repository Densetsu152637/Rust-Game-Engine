#![recursion_limit = "512"]
//! Captions: the `caption_track` asset, runtime playback, presentation on the
//! caption layer, accessibility settings, localization, the screen-reader live
//! region, the WebVTT/SRT importer and the `engine.CaptionPlayer` component.
use engine_project::{
    ActionFrame, ComponentCatalog, CustomValue, Project, Result, Runtime,
    accessibility::{AccessibilityControl, AccessibilitySettings},
    captions::{
        AdHocCue, CAPTION_LAYER, CaptionCategory, CaptionClock, CaptionSettings, CaptionSpeaker,
        Captions, PlayOptions, build_root, cue_markup, import::import_captions, load_caption_track,
        parse_caption_track, visible,
    },
    runtime::{Draw, DrawSpace},
    ui_tree::*,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

const UBUNTU: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;

fn track(cues: Value) -> Value {
    json!({"version":1,
        "speakers":[
            {"id":"ana","name":"Ana","color":[1.0,0.5,0.0,1.0]},
            {"id":"bo","name":"@speaker-bo","color":[0.0,0.5,1.0,1.0]}],
        "cues":cues})
}

fn base_cues() -> Value {
    json!([
        {"id":"a","start":0.0,"end":2.0,"speaker":"ana","text":"Hello there"},
        {"id":"b","start":1.0,"end":3.0,"speaker":"bo","text":"@greeting"},
        {"id":"c","start":1.5,"end":2.5,"text":"door creaks","category":"sound_effect","priority":5}
    ])
}

fn write(dir: &Path, relative: &str, content: impl AsRef<[u8]>) {
    let path = dir.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

struct Setup {
    cues: Value,
    entities: Vec<Value>,
    localized: bool,
}

impl Setup {
    fn new() -> Self {
        Self {
            cues: base_cues(),
            entities: vec![],
            localized: false,
        }
    }

    fn runtime(self) -> (tempfile::TempDir, Runtime) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "ui.ttf", UBUNTU);
        write(root, "intro.json", track(self.cues).to_string());
        let mut assets = vec![
            json!({"id":"ui","kind":"font_face","path":"ui.ttf"}),
            json!({"id":"intro","kind":"caption_track","path":"intro.json"}),
        ];
        if self.localized {
            write(
                root,
                "strings/en/ui.ftl",
                "greeting = Hello\nspeaker-bo = Bo\n",
            );
            write(
                root,
                "strings/fr/ui.ftl",
                "greeting = Bonjour\nspeaker-bo = Beau\n",
            );
            write(
                root,
                "locale.json",
                json!({"schema":1,"default_locale":"en","available_locales":["en","fr"],
                    "resources":[{"locale":"en","paths":["strings/en/ui.ftl"]},
                                 {"locale":"fr","paths":["strings/fr/ui.ftl"]}]})
                .to_string(),
            );
            assets.push(json!({"id":"l10n","kind":"locale_config","path":"locale.json"}));
            assets.push(json!({"id":"s-en","kind":"string_table","path":"strings/en/ui.ftl"}));
            assets.push(json!({"id":"s-fr","kind":"string_table","path":"strings/fr/ui.ftl"}));
        }
        write(
            root,
            "scene.json",
            json!({"schema":1,"scene_id":"main","entities":self.entities}).to_string(),
        );
        write(
            root,
            "project.json",
            json!({"schema":1,"name":"caps","startup_scene":"scene.json","assets":assets})
                .to_string(),
        );
        let project = Project::load(root.join("project.json")).unwrap();
        project.validate().unwrap();
        let mut runtime = Runtime::new(project).unwrap();
        runtime.start().unwrap();
        (dir, runtime)
    }
}

fn player(extra: Value) -> Value {
    let mut data = json!({"version":1,"track":"intro","font":"ui"});
    for (k, v) in extra.as_object().unwrap() {
        data[k] = v.clone();
    }
    json!({"id":"player","name":"player","components":{"custom":{
        "engine.CaptionPlayer":{"version":1,"data":data}}}})
}

fn tick(runtime: &mut Runtime, dt: f32) {
    runtime
        .tick(
            dt,
            ActionFrame {
                focused: true,
                ..Default::default()
            },
        )
        .unwrap();
}

fn captions(runtime: &mut Runtime) -> bevy_ecs::prelude::Mut<'_, Captions> {
    runtime.app_mut().world_mut().resource_mut::<Captions>()
}

fn settings(runtime: &mut Runtime, edit: impl FnOnce(&mut AccessibilitySettings)) {
    let mut control = runtime
        .app_mut()
        .world_mut()
        .resource_mut::<AccessibilityControl>();
    let mut next = control.settings.clone();
    edit(&mut next);
    control.set(next).unwrap();
}

fn active_ids(runtime: &mut Runtime) -> Vec<String> {
    captions(runtime)
        .active()
        .iter()
        .map(|c| c.cue.clone())
        .collect()
}

fn caption_draws(runtime: &Runtime) -> Vec<Draw> {
    runtime
        .extract_frame()
        .unwrap()
        .draws
        .into_iter()
        .filter(|d| d.layer == CAPTION_LAYER)
        .collect()
}

fn code(result: Result<impl std::fmt::Debug>) -> String {
    result.unwrap_err().diagnostics[0].code.clone()
}

// ---- the asset --------------------------------------------------------------

#[test]
fn a_valid_track_loads_and_overlapping_cues_are_allowed() {
    let parsed = parse_caption_track(track(base_cues()).to_string().as_bytes()).unwrap();
    assert_eq!(parsed.cues.len(), 3);
    assert_eq!(parsed.speakers.len(), 2);
    assert_eq!(parsed.duration(), 3.0);
    assert_eq!(parsed.cues[2].category, CaptionCategory::SoundEffect);
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "t.json", track(base_cues()).to_string());
    assert_eq!(
        load_caption_track(&dir.path().join("t.json")).unwrap(),
        parsed
    );
}

#[test]
fn invalid_tracks_are_rejected_with_typed_errors() {
    let bad = |cues: Value| code(parse_caption_track(track(cues).to_string().as_bytes()));
    let cue = |extra: Value| {
        let mut c = json!({"id":"x","start":0.0,"end":1.0,"text":"hi"});
        for (k, v) in extra.as_object().unwrap() {
            c[k] = v.clone();
        }
        json!([c])
    };
    assert_eq!(
        bad(cue(json!({"start":2.0,"end":1.0}))),
        "captions.cue.range"
    );
    assert_eq!(
        bad(cue(json!({"start":1.0,"end":1.0}))),
        "captions.cue.range"
    );
    assert_eq!(
        bad(cue(json!({"speaker":"ghost"}))),
        "captions.speaker.unknown"
    );
    assert_eq!(bad(cue(json!({"start":-1.0}))), "captions.cue.time");
    assert_eq!(bad(cue(json!({"end":2.0e9}))), "captions.cue.time");
    assert_eq!(bad(cue(json!({"text":""}))), "captions.cue.text");
    assert_eq!(bad(cue(json!({"text":"[b]open"}))), "captions.cue.markup");
    assert_eq!(
        bad(cue(json!({"text":"x".repeat(1025)}))),
        "captions.cue.text"
    );
    assert_eq!(bad(cue(json!({"id":"bad id"}))), "captions.cue.id");
    assert_eq!(
        bad(json!([{"id":"x","start":0,"end":1,"text":"a"},
                   {"id":"x","start":1,"end":2,"text":"b"}])),
        "captions.cue.id"
    );
    let many: Vec<Value> = (0..20_001)
        .map(|i| json!({"id":format!("c{i}"),"start":0,"end":1,"text":"a"}))
        .collect();
    assert_eq!(bad(Value::Array(many)), "captions.track.bounds");
    assert_eq!(
        code(parse_caption_track(br#"{"cues":[],"surprise":1}"#)),
        "captions.track.parse"
    );
    assert_eq!(
        code(parse_caption_track(br#"{"version":2,"cues":[]}"#)),
        "captions.track.version"
    );
    assert_eq!(
        code(parse_caption_track(&vec![b' '; 4 * 1024 * 1024 + 1])),
        "captions.file.too_large"
    );
    let speakers = json!({"version":1,"cues":[],"speakers":[
        {"id":"a","name":"A"},{"id":"a","name":"B"}]});
    assert_eq!(
        code(parse_caption_track(speakers.to_string().as_bytes())),
        "captions.speaker.id"
    );
}

#[test]
fn project_validation_rejects_a_broken_caption_track_asset() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "bad.json",
        r#"{"cues":[{"id":"a","start":3,"end":1,"text":"x"}]}"#,
    );
    write(
        dir.path(),
        "scene.json",
        json!({"schema":1,"scene_id":"main","entities":[]}).to_string(),
    );
    write(
        dir.path(),
        "project.json",
        json!({"schema":1,"name":"p","startup_scene":"scene.json",
            "assets":[{"id":"bad","kind":"caption_track","path":"bad.json"}]})
        .to_string(),
    );
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "captions.cue.range");
}

#[test]
fn cues_are_visible_from_start_inclusive_to_end_exclusive() {
    let parsed = parse_caption_track(track(base_cues()).to_string().as_bytes()).unwrap();
    let ids = |t: f64| -> Vec<&str> { parsed.active_at(t).iter().map(|c| c.id.as_str()).collect() };
    assert_eq!(ids(0.0), ["a"]);
    assert_eq!(ids(0.999), ["a"]);
    assert_eq!(ids(1.0), ["a", "b"]);
    assert_eq!(ids(1.75), ["a", "b", "c"]);
    assert_eq!(ids(2.0), ["b", "c"], "a ends exactly at 2.0 (exclusive)");
    assert_eq!(ids(3.0), Vec::<&str>::new(), "b ends at 3.0 (exclusive)");
}

// ---- the controller -----------------------------------------------------------

fn controller() -> Captions {
    let mut captions = Captions::default();
    captions
        .insert_track(
            "intro",
            parse_caption_track(track(base_cues()).to_string().as_bytes()).unwrap(),
        )
        .unwrap();
    captions
}

#[test]
fn playback_follows_the_clock_and_emits_start_and_end_events() {
    let mut captions = controller();
    let id = captions.play("intro", PlayOptions::default()).unwrap();
    let events = captions.advance(0.0);
    assert_eq!(events.len(), 1);
    assert!(events[0].started && events[0].cue.cue == "a");
    assert_eq!(events[0].name(), "captions.cue_started");
    let events = captions.advance(2.0);
    // t = 2.0: a ended; b and c are active (b started at 1.0, c at 1.5).
    let names: Vec<_> = events
        .iter()
        .map(|e| (e.started, e.cue.cue.clone()))
        .collect();
    assert!(names.contains(&(false, "a".into())));
    assert!(names.contains(&(true, "b".into())));
    assert!(names.contains(&(true, "c".into())));
    assert_eq!(captions.position(id), Some(2.0));
    captions.advance(1.5);
    assert!(captions.active().is_empty());
    assert!(captions.playing().is_empty(), "a finished track leaves");
}

#[test]
fn seek_jumps_to_a_position_and_loop_restarts() {
    let mut captions = controller();
    let id = captions.play("intro", PlayOptions::default()).unwrap();
    captions.advance(0.0);
    captions.seek(id, 1.6).unwrap();
    captions.advance(0.0);
    let ids: Vec<_> = captions.active().iter().map(|c| c.cue.as_str()).collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&"a") && ids.contains(&"b") && ids.contains(&"c"));
    captions.seek(id, 5.0).unwrap_or(());
    captions.stop(id);
    assert_eq!(code(captions.seek(id, 1.0)), "captions.playback.unknown");
    let looping = captions
        .play(
            "intro",
            PlayOptions {
                looping: true,
                ..Default::default()
            },
        )
        .unwrap();
    captions.advance(3.5);
    assert_eq!(captions.position(looping), Some(0.5));
    assert_eq!(captions.active().len(), 1, "cue a again after the wrap");
    assert_eq!(
        code(captions.play("missing", PlayOptions::default())),
        "captions.track.unknown"
    );
}

#[test]
fn offset_starts_part_way_and_a_custom_clock_drives_the_position() {
    let mut captions = controller();
    captions
        .play(
            "intro",
            PlayOptions {
                offset: 2.0,
                ..Default::default()
            },
        )
        .unwrap();
    captions.advance(0.0);
    let ids: Vec<_> = captions.active().iter().map(|c| c.cue.as_str()).collect();
    assert!(!ids.contains(&"a") && ids.contains(&"b"));
    captions.clear();
    let position = Arc::new(Mutex::new(Some(0.2)));
    let source = position.clone();
    let clock: CaptionClock = Arc::new(move || *source.lock().unwrap());
    captions
        .play(
            "intro",
            PlayOptions {
                clock: Some(clock),
                ..Default::default()
            },
        )
        .unwrap();
    captions.advance(10.0);
    assert_eq!(captions.active().len(), 1, "dt is ignored with a clock");
    *position.lock().unwrap() = Some(1.2);
    captions.advance(0.0);
    assert_eq!(captions.active().len(), 2);
    *position.lock().unwrap() = None;
    captions.advance(5.0);
    assert_eq!(captions.active().len(), 2, "a clock returning None holds");
}

#[test]
fn ad_hoc_cues_expire_and_clear_removes_everything() {
    let mut captions = Captions::default();
    captions
        .push(AdHocCue::new("Look out!", 1.0).spoken(CaptionSpeaker {
            id: "x".into(),
            name: "Guard".into(),
            color: [1.0; 4],
        }))
        .unwrap();
    let events = captions.advance(0.0);
    assert_eq!(events.len(), 1);
    captions.advance(0.5);
    assert_eq!(captions.active().len(), 1);
    let events = captions.advance(0.5);
    assert!(captions.active().is_empty(), "expired after its duration");
    assert!(!events[0].started);
    captions.push(AdHocCue::new("again", 10.0)).unwrap();
    captions.advance(0.0);
    captions.clear();
    captions.advance(0.0);
    assert!(captions.active().is_empty());
    assert_eq!(
        code(captions.push(AdHocCue::new("x", 0.0))),
        "captions.cue.range"
    );
    assert_eq!(
        code(captions.push(AdHocCue::new("[b]", 1.0))),
        "captions.cue.markup"
    );
}

fn show(captions: &Captions, settings: &CaptionSettings) -> Vec<String> {
    visible(captions, settings)
        .iter()
        .map(|c| c.cue.clone())
        .collect()
}

#[test]
fn overlapping_cues_stack_oldest_first_within_max_lines_by_priority() {
    let mut captions = controller();
    captions.play("intro", PlayOptions::default()).unwrap();
    captions.advance(0.0);
    captions.advance(1.0);
    captions.advance(0.5); // t = 1.5: a, b, c
    let mut settings = CaptionSettings::default();
    assert_eq!(show(&captions, &settings), ["a", "b", "c"]);
    settings.max_lines = 2;
    // c has priority 5 and survives; of a and b the more recent (b) stays.
    assert_eq!(show(&captions, &settings), ["b", "c"]);
    settings.max_lines = 1;
    assert_eq!(show(&captions, &settings), ["c"]);
}

#[test]
fn sound_effect_captions_are_bracketed_and_can_be_turned_off() {
    let mut captions = controller();
    captions.play("intro", PlayOptions::default()).unwrap();
    captions.advance(1.6);
    let mut settings = CaptionSettings::default();
    let cues = visible(&captions, &settings);
    let sfx = cues.iter().find(|c| c.cue == "c").unwrap();
    assert_eq!(cue_markup(sfx, &settings, None), "\\[door creaks\\]");
    settings.sound_effects = false;
    assert_eq!(show(&captions, &settings), ["a", "b"]);
}

// ---- presentation --------------------------------------------------------------

#[test]
fn captions_draw_on_the_caption_layer_with_background_opacity_in_alpha() {
    let (_dir, mut runtime) = Setup {
        entities: vec![player(json!({}))],
        ..Setup::new()
    }
    .runtime();
    tick(&mut runtime, 0.1);
    let draws = caption_draws(&runtime);
    assert!(!draws.is_empty(), "captions produced draws");
    assert!(
        draws
            .iter()
            .all(|d| d.layer == CAPTION_LAYER && d.space == DrawSpace::Ui)
    );
    const { assert!(CAPTION_LAYER > 0 && CAPTION_LAYER < UI_DIALOG_LAYER) };
    let alphas: Vec<f32> = draws
        .iter()
        .filter(|d| d.texture.is_none())
        .flat_map(|d| d.mesh.vertices.iter().map(|v| v.color[3]))
        .filter(|a| *a > 0.0)
        .collect();
    assert!(!alphas.is_empty(), "a background quad exists");
    assert!(alphas.iter().all(|a| (a - 0.6).abs() < 1e-4), "{alphas:?}");
    settings(&mut runtime, |s| s.caption_background_opacity = 0.25);
    tick(&mut runtime, 0.1);
    let alphas: Vec<f32> = caption_draws(&runtime)
        .iter()
        .filter(|d| d.texture.is_none())
        .flat_map(|d| d.mesh.vertices.iter().map(|v| v.color[3]))
        .filter(|a| *a > 0.0)
        .collect();
    assert!(alphas.iter().all(|a| (a - 0.25).abs() < 1e-4), "{alphas:?}");
}

/// Total vertices of the textured (glyph) captions draws.
fn glyph_vertices(runtime: &Runtime) -> usize {
    caption_draws(runtime)
        .iter()
        .filter(|d| d.texture.is_some())
        .map(|d| d.mesh.vertices.len())
        .sum()
}

fn glyph_height(runtime: &Runtime) -> f32 {
    let ys: Vec<f32> = caption_draws(runtime)
        .iter()
        .filter(|d| d.texture.is_some())
        .flat_map(|d| d.mesh.vertices.iter().map(|v| v.position[1]))
        .collect();
    ys.iter().cloned().fold(f32::MIN, f32::max) - ys.iter().cloned().fold(f32::MAX, f32::min)
}

#[test]
fn speaker_names_change_the_drawn_text() {
    let (_dir, mut runtime) = Setup {
        entities: vec![player(json!({}))],
        ..Setup::new()
    }
    .runtime();
    tick(&mut runtime, 0.1);
    let with_names = glyph_vertices(&runtime);
    settings(&mut runtime, |s| s.caption_speaker_names = false);
    tick(&mut runtime, 0.1);
    let without = glyph_vertices(&runtime);
    assert!(
        with_names > without && without > 0,
        "{with_names} vs {without}"
    );
}

#[test]
fn text_scale_increases_glyph_height_and_disabling_removes_the_captions() {
    let (_dir, mut runtime) = Setup {
        entities: vec![player(json!({}))],
        ..Setup::new()
    }
    .runtime();
    tick(&mut runtime, 0.1);
    let normal = glyph_height(&runtime);
    settings(&mut runtime, |s| s.caption_text_scale = 2.0);
    tick(&mut runtime, 0.1);
    let doubled = glyph_height(&runtime);
    assert!(doubled > normal * 1.5, "{normal} -> {doubled}");
    settings(&mut runtime, |s| {
        s.caption_text_scale = 1.0;
        s.text_scale = 2.0;
    });
    tick(&mut runtime, 0.1);
    assert!(
        glyph_height(&runtime) > normal * 1.5,
        "global text scale applies"
    );
    settings(&mut runtime, |s| s.captions_enabled = false);
    tick(&mut runtime, 0.1);
    assert!(caption_draws(&runtime).is_empty());
}

#[test]
fn long_cues_wrap_within_the_maximum_width() {
    let long = "word ".repeat(60);
    let (_dir, mut runtime) = Setup {
        cues: json!([{"id":"long","start":0,"end":5,"text":long.trim()}]),
        entities: vec![player(json!({"maxWidth":0.5}))],
        ..Setup::new()
    }
    .runtime();
    tick(&mut runtime, 0.1);
    let frame = runtime.extract_frame().unwrap();
    let width = frame.camera.logical_size[0] as f32;
    let xs: Vec<f32> = frame
        .draws
        .iter()
        .filter(|d| d.layer == CAPTION_LAYER && d.mesh.vertices.iter().any(|v| v.color[3] > 0.0))
        .flat_map(|d| d.mesh.vertices.iter().map(|v| v.position[0]))
        .collect();
    let span =
        xs.iter().cloned().fold(f32::MIN, f32::max) - xs.iter().cloned().fold(f32::MAX, f32::min);
    assert!(
        span <= width * 0.5 + 0.5,
        "span {span} within half of {width}"
    );
    assert!(span > width * 0.4, "span {span} fills the allowed width");
    assert!(glyph_height(&runtime) > 40.0, "several lines tall");
}

#[test]
fn the_live_region_carries_the_active_cue_text_in_the_access_tree() {
    let (_dir, mut runtime) = Setup {
        entities: vec![player(json!({}))],
        ..Setup::new()
    }
    .runtime();
    settings(&mut runtime, |s| s.screen_reader = true);
    tick(&mut runtime, 0.1);
    let ui = runtime.app().world().resource::<GameUi>();
    let tree = ui.accessibility_tree().expect("tree");
    let region = tree.find("captions", "captions").expect("caption region");
    assert_eq!(region.live, AccessLive::Polite);
    assert_eq!(region.role, AccessRole::Status);
    let names: Vec<_> = region
        .children
        .iter()
        .filter_map(|id| tree.get(id))
        .map(|n| n.name.clone().unwrap_or_default())
        .collect();
    assert_eq!(names.len(), 1);
    assert!(
        names[0].contains("Ana") && names[0].contains("Hello there"),
        "{names:?}"
    );
    assert!(!names[0].contains('['), "markup is not read out: {names:?}");
}

#[test]
fn localized_cue_text_and_speaker_names_follow_the_locale() {
    let (_dir, mut runtime) = Setup {
        localized: true,
        entities: vec![player(json!({}))],
        ..Setup::new()
    }
    .runtime();
    tick(&mut runtime, 0.1);
    tick(&mut runtime, 1.0); // t = 1.1: a and b
    let texts = |runtime: &Runtime| -> Vec<String> {
        let text = runtime.ui_text();
        struct Recorder<'a>(&'a dyn UiTextBackend, Mutex<Vec<String>>);
        impl UiTextBackend for Recorder<'_> {
            fn measure(&self, r: &UiTextRequest<'_>) -> Result<UiTextMetrics> {
                self.0.measure(r)
            }
            fn layout(&self, r: &UiTextRequest<'_>) -> Result<UiTextLayout> {
                self.1.lock().unwrap().push(r.text.to_owned());
                self.0.layout(r)
            }
            fn begin_frame(&self) {
                self.0.begin_frame();
            }
        }
        let recorder = Recorder(&text, Mutex::new(vec![]));
        extract_world_with(
            runtime.app().world(),
            [800, 600],
            &recorder,
            &BTreeMap::new(),
        )
        .unwrap();
        recorder.1.into_inner().unwrap()
    };
    let english = texts(&runtime).join("|");
    assert!(
        english.contains("Hello") && english.contains("Bo"),
        "{english}"
    );
    runtime.set_locale("fr").unwrap();
    tick(&mut runtime, 0.01);
    let french = texts(&runtime).join("|");
    assert!(
        french.contains("Bonjour") && french.contains("Beau"),
        "{french}"
    );
    assert!(!french.contains("@greeting") && !french.contains("@speaker-bo"));
}

// ---- the component --------------------------------------------------------------

#[test]
fn the_player_component_autoplays_and_emits_events_on_the_runtime() {
    let (_dir, mut runtime) = Setup {
        entities: vec![player(json!({"offset":1.0}))],
        ..Setup::new()
    }
    .runtime();
    tick(&mut runtime, 0.1);
    let ids = active_ids(&mut runtime);
    assert!(
        ids.contains(&"a".to_string()) && ids.contains(&"b".to_string()),
        "{ids:?}"
    );
    // Events queued by a tick are delivered by the next one.
    tick(&mut runtime, 0.1);
    let names: Vec<String> = runtime
        .snapshot()
        .events
        .iter()
        .map(|e| format!("{}:{}", e.name, e.payload["cue"].as_str().unwrap_or("")))
        .collect();
    assert!(
        names.contains(&"captions.cue_started:a".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"captions.cue_started:b".to_string()),
        "{names:?}"
    );
}

#[test]
fn a_player_without_autoplay_stays_silent_and_an_unknown_track_is_reported() {
    let (_dir, mut runtime) = Setup {
        entities: vec![player(json!({"autoplay":false}))],
        ..Setup::new()
    }
    .runtime();
    tick(&mut runtime, 0.1);
    assert!(active_ids(&mut runtime).is_empty());
    captions(&mut runtime)
        .play("intro", PlayOptions::default())
        .unwrap();
    tick(&mut runtime, 0.1);
    assert!(!active_ids(&mut runtime).is_empty());
    let (_dir, mut runtime) = Setup {
        entities: vec![player(json!({"track":"nope"}))],
        ..Setup::new()
    }
    .runtime();
    tick(&mut runtime, 0.1);
    assert!(captions(&mut runtime).errors()[0].contains("nope"));
}

fn catalog() -> ComponentCatalog {
    ComponentCatalog::new(vec![]).unwrap()
}

fn validate(name: &str, data: Value) -> Result<()> {
    catalog().validate(name, &CustomValue { version: 1, data })
}

#[test]
fn the_descriptors_accept_samples_and_reject_invalid_input() {
    assert!(catalog().0.contains_key("engine.CaptionPlayer"));
    for good in [
        json!({"version":1}),
        json!({"version":1,"track":"intro","autoplay":false,"loop":true,"offset":2.5,
               "font":"ui","bottomMargin":80,"maxWidth":0.6}),
    ] {
        validate("engine.CaptionPlayer", good).unwrap();
    }
    for bad in [
        json!({"version":2}),
        json!({"version":1,"offset":-1}),
        json!({"version":1,"maxWidth":0.1}),
        json!({"version":1,"bottomMargin":5000}),
        json!({"version":1,"track":"bad id!"}),
        json!({"version":1,"extra":true}),
        json!({"version":1,"loop":"yes"}),
    ] {
        assert!(
            validate("engine.CaptionPlayer", bad.clone()).is_err(),
            "{bad}"
        );
    }
    validate(
        "engine.AccessibilitySettings",
        json!({"version":1,"captionsEnabled":false,"captionTextScale":1.5,
               "captionBackgroundOpacity":0.9,"captionSpeakerNames":false,
               "captionMaxLines":4,"captionSoundEffects":false}),
    )
    .unwrap();
    for bad in [
        json!({"version":1,"captionTextScale":9}),
        json!({"version":1,"captionBackgroundOpacity":1.5}),
        json!({"version":1,"captionMaxLines":0}),
        json!({"version":1,"captionMaxLines":9}),
        json!({"version":1,"captionsEnabled":"yes"}),
    ] {
        assert!(
            validate("engine.AccessibilitySettings", bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn the_generated_root_validates_and_orders_cues() {
    let mut captions = controller();
    captions.play("intro", PlayOptions::default()).unwrap();
    captions.advance(1.6);
    let root = build_root(&captions, &CaptionSettings::default(), None);
    root.validate().unwrap();
    assert_eq!(root.layer, CAPTION_LAYER);
    assert_eq!(root.tree.nodes.len(), 4, "panel plus three cues");
}

// ---- importer ----------------------------------------------------------------------

#[test]
fn webvtt_import_reads_timings_voice_tags_and_formatting() {
    let vtt = "WEBVTT\n\nNOTE a comment\n\n1\n00:00:01.000 --> 00:00:03.500 align:start\n<v Ana>Hello <i>there</i> &amp; [friend]</v>\n\n00:04.000 --> 00:05.250\n[door creaks]\n\n01:00:00.000 --> 01:00:01.000\n<v Bo>Later</v>\n<v Bo>second line</v>\n";
    let track = import_captions(vtt).unwrap();
    assert_eq!(track.cues.len(), 3);
    assert_eq!(track.speakers.len(), 2);
    let first = &track.cues[0];
    assert_eq!((first.start, first.end), (1.0, 3.5));
    assert_eq!(first.text, "Hello [i]there[/i] & \\[friend\\]");
    assert_eq!(
        track
            .speaker(first.speaker.as_deref().unwrap())
            .unwrap()
            .name,
        "Ana"
    );
    let sfx = &track.cues[1];
    assert_eq!((sfx.start, sfx.end), (4.0, 5.25));
    assert_eq!(sfx.category, CaptionCategory::SoundEffect);
    assert_eq!(sfx.text, "door creaks");
    assert_eq!(track.cues[2].start, 3600.0);
    track.validate().unwrap();
}

#[test]
fn srt_import_reads_comma_timings_and_multiple_lines() {
    let srt = "1\r\n00:00:01,200 --> 00:00:02,800\r\nFirst line\r\nsecond line\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\n<b>Bold</b>\r\n";
    let track = import_captions(srt).unwrap();
    assert_eq!(track.cues.len(), 2);
    assert_eq!(track.cues[0].start, 1.2);
    assert_eq!(track.cues[0].end, 2.8);
    assert_eq!(track.cues[0].text, "First line\nsecond line");
    assert_eq!(track.cues[1].text, "[b]Bold[/b]");
    assert_eq!(
        code(import_captions("1\n00:00:xx --> 00:00:02,000\nhi\n")),
        "captions.import.timing"
    );
    assert_eq!(
        code(import_captions("1\n00:00:05,000 --> 00:00:02,000\nhi\n")),
        "captions.cue.range"
    );
}
