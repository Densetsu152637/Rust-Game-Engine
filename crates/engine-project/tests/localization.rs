#![recursion_limit = "512"]
//! Localization in the game UI: `string_table` / `locale_config` assets, `@key`
//! text references, bindings with message keys and plural arguments, the
//! runtime language switch, per-locale fonts and asset overrides, pseudo
//! localization, diagnostics and validation errors.
use engine_project::{
    ActionFrame, Project, Runtime,
    localization::{LOCALE_CHANGED_EVENT, Localization},
    ui_tree::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, sync::Mutex};

const UBUNTU: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;
const EMOJI: &[u8] = epaint_default_fonts::NOTO_EMOJI_REGULAR;

const EN: &str = "\
# Greeting on the title screen.
greeting = Hello
farewell = Goodbye
button-ok = OK
tab-one = First
tab-two = Second
welcome = Welcome
    .tooltip = Opens the hub
rich = [b]Bold[/b] text
items = { $count ->
    [one] { $count } item
   *[other] { $count } items
}
";

const FR: &str = "\
greeting = Bonjour
farewell = Au revoir
button-ok = D'accord
tab-one = Premier
tab-two = Second
welcome = Bienvenue
    .tooltip = Ouvre le hub
rich = [b]Gras[/b] texte
items = { $count ->
    [one] { $count } objet
   *[other] { $count } objets
}
";

const RU: &str = "\
greeting = Привет
farewell = До свидания
items = { $count ->
    [one] { $count } предмет
    [few] { $count } предмета
    [many] { $count } предметов
   *[other] { $count } предмета
}
";

const FR_CA: &str = "farewell = À la prochaine\n";

fn node(n: UiNode) -> UiNode {
    n.with_style(|s| {
        s.font = "ui".into();
        s.padding = 0.0;
    })
}

fn sized(n: UiNode, x: f32, y: f32, w: f32, h: f32) -> UiNode {
    node(n)
        .with_size(UiLength::Pixels(w), UiLength::Pixels(h))
        .with_style(|s| s.offset = [x, y])
}

fn config(locales: &[&str], extra: Value) -> Value {
    let resources: Vec<Value> = locales
        .iter()
        .map(|l| json!({"locale": l, "paths": [format!("strings/{l}/ui.ftl")]}))
        .collect();
    let mut value = json!({
        "schema": 1, "default_locale": "en",
        "available_locales": locales, "resources": resources
    });
    for (k, v) in extra.as_object().into_iter().flatten() {
        value[k] = v.clone();
    }
    value
}

fn table_assets(locales: &[&str]) -> Vec<Value> {
    locales
        .iter()
        .map(|l| {
            json!({"id": format!("strings-{l}"), "kind": "string_table",
                "path": format!("strings/{l}/ui.ftl")})
        })
        .collect()
}

fn write(dir: &Path, relative: &str, content: impl AsRef<[u8]>) {
    let path = dir.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

struct Setup {
    roots: Vec<(&'static str, UiRoot)>,
    locales: Vec<(&'static str, &'static str)>,
    config: Option<Value>,
    assets: Vec<Value>,
    tables: bool,
    files: Vec<(&'static str, Vec<u8>)>,
}

impl Setup {
    fn new(roots: Vec<(&'static str, UiRoot)>) -> Self {
        Self {
            roots,
            locales: vec![("en", EN), ("fr", FR), ("ru", RU)],
            config: None,
            assets: vec![],
            tables: true,
            files: vec![],
        }
    }

    fn build(self) -> (tempfile::TempDir, Result<Project, engine_project::Error>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "ui.ttf", UBUNTU);
        write(root, "emoji.ttf", EMOJI);
        let names: Vec<&str> = self.locales.iter().map(|(l, _)| *l).collect();
        let mut assets = vec![
            json!({"id":"ui","kind":"font_face","path":"ui.ttf"}),
            json!({"id":"emoji","kind":"font_face","path":"emoji.ttf"}),
            json!({"id":"l10n","kind":"locale_config","path":"locale.json"}),
        ];
        if self.tables {
            assets.extend(table_assets(&names));
        }
        assets.extend(self.assets);
        for (locale, text) in &self.locales {
            write(root, &format!("strings/{locale}/ui.ftl"), text);
        }
        for (path, bytes) in &self.files {
            write(root, path, bytes);
        }
        let config = self.config.unwrap_or_else(|| config(&names, json!({})));
        write(root, "locale.json", config.to_string());
        let entities: Vec<Value> = self
            .roots
            .iter()
            .map(|(id, ui)| {
                json!({"id": id, "name": id,
                    "components": {"custom": {"engine.UiRoot": {"version": 1, "data": ui}}}})
            })
            .collect();
        write(
            root,
            "scene.json",
            json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
        );
        write(
            root,
            "project.json",
            json!({"schema":1,"name":"l10n","startup_scene":"scene.json","assets":assets})
                .to_string(),
        );
        let project = Project::load(root.join("project.json"));
        (dir, project)
    }

    fn runtime(self) -> (tempfile::TempDir, Runtime) {
        let (dir, project) = self.build();
        let mut runtime = Runtime::new(project.unwrap()).unwrap();
        runtime.start().unwrap();
        (dir, runtime)
    }
}

fn root(nodes: Vec<UiNode>) -> UiRoot {
    UiRoot {
        tree: UiTree { nodes },
        ..Default::default()
    }
}

fn tick(runtime: &mut Runtime) {
    runtime
        .tick(
            1.0 / 60.0,
            ActionFrame {
                focused: true,
                ..Default::default()
            },
        )
        .unwrap();
}

/// Records every text the UI lays out while delegating to a real backend.
struct Recorder<'a> {
    inner: &'a dyn UiTextBackend,
    seen: Mutex<Vec<String>>,
}

impl UiTextBackend for Recorder<'_> {
    fn measure(&self, request: &UiTextRequest<'_>) -> engine_project::Result<UiTextMetrics> {
        self.inner.measure(request)
    }
    fn layout(&self, request: &UiTextRequest<'_>) -> engine_project::Result<UiTextLayout> {
        self.seen.lock().unwrap().push(request.text.to_owned());
        self.inner.layout(request)
    }
    fn begin_frame(&self) {
        self.inner.begin_frame();
    }
}

/// Texts drawn by the UI roots of `runtime` right now.
fn drawn(runtime: &Runtime) -> Vec<String> {
    let text = runtime.ui_text();
    let recorder = Recorder {
        inner: &text,
        seen: Mutex::new(vec![]),
    };
    extract_world_with(
        runtime.app().world(),
        [800, 600],
        &recorder,
        &BTreeMap::new(),
    )
    .unwrap();
    recorder.seen.into_inner().unwrap()
}

fn stored_root(runtime: &Runtime, id: &str) -> UiRoot {
    let snapshot = runtime.try_snapshot().unwrap();
    let entity = snapshot.scene.entities.iter().find(|e| e.id == id).unwrap();
    serde_json::from_value(entity.components.custom["engine.UiRoot"].data.clone()).unwrap()
}

fn label_roots() -> Vec<(&'static str, UiRoot)> {
    vec![(
        "hud",
        root(vec![
            sized(UiNode::label("title", "@greeting"), 0.0, 0.0, 300.0, 30.0),
            sized(
                UiNode::label("literal", "@@greeting"),
                0.0,
                40.0,
                300.0,
                30.0,
            ),
            sized(
                UiNode::label("spaced", "@ greeting"),
                0.0,
                80.0,
                300.0,
                30.0,
            ),
            sized(
                UiNode::label("plain", "Hello @greeting"),
                0.0,
                120.0,
                300.0,
                30.0,
            ),
        ]),
    )]
}

// ---- assets ----------------------------------------------------------------

#[test]
fn project_with_en_fr_tables_and_config_loads_and_validates() {
    let (_dir, project) = Setup::new(label_roots()).build();
    let project = project.unwrap();
    project.validate().unwrap();
    assert_eq!(project.assets["strings-fr"].kind, "string_table");
    assert_eq!(project.assets["l10n"].kind, "locale_config");
    let runtime = Runtime::new(project).unwrap();
    let localization = runtime.localization();
    assert!(localization.active());
    assert_eq!(localization.locale(), "en");
    let status = localization.status().unwrap();
    assert_eq!(status.locales, ["en", "fr", "ru"]);
    assert_eq!(status.default_locale, "en");
    assert!(localization.with(|l| l.contains("greeting") && l.contains("welcome.tooltip")));
    // The report (and so the snapshot) carries the locale.
    assert_eq!(
        runtime.snapshot().localization.as_ref().unwrap().locale,
        "en"
    );
}

#[test]
fn project_without_localization_is_inactive_and_leaves_at_text_alone() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "ui.ttf", UBUNTU);
    let ui = root(vec![sized(
        UiNode::label("a", "@greeting"),
        0.0,
        0.0,
        200.0,
        30.0,
    )]);
    write(
        dir.path(),
        "scene.json",
        json!({"schema":1,"scene_id":"main","entities":[{"id":"hud","name":"hud","components":
            {"custom":{"engine.UiRoot":{"version":1,"data":ui}}}}]})
        .to_string(),
    );
    write(
        dir.path(),
        "project.json",
        json!({"schema":1,"name":"plain","startup_scene":"scene.json",
            "assets":[{"id":"ui","kind":"font_face","path":"ui.ttf"}]})
        .to_string(),
    );
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    tick(&mut runtime);
    assert!(!runtime.localization().active());
    assert!(drawn(&runtime).contains(&"@greeting".to_string()));
    assert!(runtime.snapshot().localization.is_none());
    let error = runtime.set_locale("fr").unwrap_err();
    assert_eq!(error.diagnostics[0].code, "locale.unavailable");
}

// ---- text references -------------------------------------------------------

#[test]
fn at_key_renders_en_then_fr_after_set_locale_without_reloading() {
    let (_dir, mut runtime) = Setup::new(label_roots()).runtime();
    tick(&mut runtime);
    let en = drawn(&runtime);
    assert!(en.contains(&"Hello".to_string()), "{en:?}");
    // Non-keys are literal text.
    assert!(en.contains(&"@ greeting".to_string()), "{en:?}");
    assert!(en.contains(&"Hello @greeting".to_string()), "{en:?}");

    runtime.set_locale("fr").unwrap();
    assert_eq!(runtime.locale(), "fr");
    tick(&mut runtime);
    let fr = drawn(&runtime);
    assert!(fr.contains(&"Bonjour".to_string()), "{fr:?}");
    assert!(!fr.contains(&"Hello".to_string()));

    // The stored component keeps the key; the escape is not rewritten.
    let stored = stored_root(&runtime, "hud");
    let texts: Vec<_> = stored
        .tree
        .nodes
        .iter()
        .map(|n| match &n.widget {
            UiWidget::Label { text } => text.as_str(),
            _ => "",
        })
        .collect();
    assert_eq!(
        texts,
        ["@greeting", "@@greeting", "@ greeting", "Hello @greeting"]
    );

    runtime.set_locale("en").unwrap();
    tick(&mut runtime);
    assert!(drawn(&runtime).contains(&"Hello".to_string()));
}

#[test]
fn escaped_at_text_loses_one_at_sign() {
    let (_dir, mut runtime) = Setup::new(label_roots()).runtime();
    tick(&mut runtime);
    let texts = drawn(&runtime);
    let at_greeting = texts.iter().filter(|t| t.as_str() == "@greeting").count();
    assert_eq!(at_greeting, 1, "`@@greeting` shows `@greeting`: {texts:?}");
}

fn widget_roots() -> Vec<(&'static str, UiRoot)> {
    let mut dd_selected =
        UiNode::dropdown("dd1", vec!["@greeting".into(), "@farewell".into()], Some(1));
    let mut dd_empty = UiNode::dropdown("dd2", vec!["@greeting".into()], None);
    if let UiWidget::Dropdown { placeholder, .. } = &mut dd_empty.widget {
        *placeholder = "@tab-one".into();
    }
    if let UiWidget::Dropdown { placeholder, .. } = &mut dd_selected.widget {
        *placeholder = "@tab-two".into();
    }
    let mut dialog = UiNode::dialog("dlg", "@welcome", vec!["@button-ok".into()]);
    if let UiWidget::ModalDialog { open, .. } = &mut dialog.widget {
        *open = true;
    }
    let tabs = UiNode::tabs("tabs", vec!["@tab-one".into(), "@tab-two".into()]);
    let input = UiNode::text_input("inp", "@greeting", 32);
    vec![(
        "hud",
        root(vec![
            sized(
                UiNode::button("ok", "@button-ok", "ok"),
                0.0,
                0.0,
                160.0,
                30.0,
            )
            .with_tooltip("@welcome.tooltip", 0.0),
            sized(
                UiNode::toggle("tg", "@farewell", false),
                0.0,
                40.0,
                200.0,
                30.0,
            ),
            sized(UiNode::rich_label("rich", "@rich"), 0.0, 80.0, 300.0, 30.0),
            sized(tabs, 0.0, 120.0, 300.0, 60.0),
            sized(
                UiNode::container("p1", UiLayout::Column),
                0.0,
                0.0,
                10.0,
                10.0,
            )
            .child_of("tabs"),
            sized(
                UiNode::container("p2", UiLayout::Column),
                0.0,
                0.0,
                10.0,
                10.0,
            )
            .child_of("tabs"),
            sized(dd_selected, 0.0, 200.0, 200.0, 30.0),
            sized(dd_empty, 0.0, 240.0, 200.0, 30.0),
            sized(input, 0.0, 280.0, 200.0, 30.0),
            sized(dialog, 100.0, 100.0, 400.0, 200.0),
        ]),
    )]
}

#[test]
fn every_text_slot_is_localized_and_markup_survives_translation() {
    let (_dir, mut runtime) = Setup::new(widget_roots()).runtime();
    tick(&mut runtime);
    let en = drawn(&runtime);
    for expected in [
        "OK",
        "Goodbye",
        "[b]Bold[/b] text",
        "First",
        "Second",
        "Hello",
    ] {
        assert!(en.contains(&expected.to_string()), "en {expected}: {en:?}");
    }
    assert!(!en.iter().any(|t| t.starts_with('@')), "{en:?}");

    runtime.set_locale("fr").unwrap();
    tick(&mut runtime);
    let fr = drawn(&runtime);
    for expected in [
        "D'accord",
        "Au revoir",
        "[b]Gras[/b] texte",
        "Premier",
        "Second",
        "Bonjour",
        "Bienvenue",
    ] {
        assert!(fr.contains(&expected.to_string()), "fr {expected}: {fr:?}");
    }
    assert!(!fr.iter().any(|t| t.starts_with('@')), "{fr:?}");
}

#[test]
fn tooltips_resolve_while_the_stored_tooltip_keeps_its_key() {
    let roots = vec![(
        "hud",
        root(vec![
            sized(
                UiNode::button("ok", "@button-ok", "ok"),
                0.0,
                0.0,
                160.0,
                30.0,
            )
            .with_tooltip("@welcome.tooltip", 0.0),
        ]),
    )];
    let (_dir, mut runtime) = Setup::new(roots).runtime();
    runtime.set_locale("fr").unwrap();
    let hover = ActionFrame {
        focused: true,
        pointer: [10.0, 10.0],
        ..Default::default()
    };
    runtime.tick(0.1, hover.clone()).unwrap();
    runtime.tick(0.1, hover).unwrap();
    let ui = runtime.app().world().resource::<GameUi>();
    let (_, text, _) = ui.tooltip().expect("tooltip is visible");
    assert_eq!(text, "Ouvre le hub");
    let stored = stored_root(&runtime, "hud");
    let tooltip = stored.tree.nodes[0].tooltip.clone().unwrap();
    assert_eq!(tooltip.text, "@welcome.tooltip");
}

#[test]
fn interaction_changes_state_but_never_writes_translations_back() {
    let roots = vec![(
        "hud",
        root(vec![sized(
            UiNode::toggle("tg", "@farewell", false),
            0.0,
            0.0,
            200.0,
            30.0,
        )]),
    )];
    let (_dir, mut runtime) = Setup::new(roots).runtime();
    runtime.set_locale("fr").unwrap();
    let frame = |pressed: bool| {
        let mut frame = ActionFrame {
            focused: true,
            pointer: [20.0, 10.0],
            ..Default::default()
        };
        if pressed {
            frame.pressed.insert("ui_click".into());
            frame.held.insert("ui_click".into());
        } else {
            frame.released.insert("ui_click".into());
        }
        frame
    };
    runtime.tick(1.0 / 60.0, frame(true)).unwrap();
    runtime.tick(1.0 / 60.0, frame(false)).unwrap();
    let stored = stored_root(&runtime, "hud");
    match &stored.tree.nodes[0].widget {
        UiWidget::Toggle { text, checked, .. } => {
            assert!(*checked, "the toggle changed");
            assert_eq!(text, "@farewell", "the key, not 'Au revoir'");
        }
        other => panic!("{other:?}"),
    }
    assert!(drawn(&runtime).contains(&"Au revoir".to_string()));
}

// ---- bindings and plurals --------------------------------------------------

fn count_root() -> Vec<(&'static str, UiRoot)> {
    let mut ui = root(vec![sized(
        UiNode::label("count", ""),
        0.0,
        0.0,
        300.0,
        30.0,
    )]);
    ui.bindings.push(
        UiBinding::one_way("count", UiBindProperty::Text, "count")
            .with_message("items", [("count".to_string(), "count".to_string())]),
    );
    vec![("hud", ui)]
}

fn set_count(runtime: &mut Runtime, n: u32) {
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiDataContext>()
        .set("count", json!(n))
        .unwrap();
    tick(runtime);
}

#[test]
fn binding_plural_arguments_pick_the_right_variant_in_en_and_ru() {
    let (_dir, mut runtime) = Setup::new(count_root()).runtime();
    let shows = |runtime: &Runtime, text: &str| {
        let texts = drawn(runtime);
        assert!(texts.contains(&text.to_string()), "{text}: {texts:?}");
    };
    set_count(&mut runtime, 1);
    shows(&runtime, "1 item");
    set_count(&mut runtime, 5);
    shows(&runtime, "5 items");

    runtime.set_locale("ru").unwrap();
    tick(&mut runtime);
    // The same data re-resolves in Russian on the locale change alone.
    shows(&runtime, "5 предметов");
    set_count(&mut runtime, 1);
    shows(&runtime, "1 предмет");
    set_count(&mut runtime, 2);
    shows(&runtime, "2 предмета");
    set_count(&mut runtime, 21);
    shows(&runtime, "21 предмет");
    set_count(&mut runtime, 11);
    shows(&runtime, "11 предметов");

    runtime.set_locale("fr").unwrap();
    set_count(&mut runtime, 1);
    shows(&runtime, "1 objet");
    set_count(&mut runtime, 5);
    shows(&runtime, "5 objets");
}

#[test]
fn bound_text_is_never_taken_for_a_message_reference() {
    // A player name that looks like a key must stay literal.
    let mut ui = root(vec![sized(
        UiNode::label("name", ""),
        0.0,
        0.0,
        300.0,
        30.0,
    )]);
    ui.bindings
        .push(UiBinding::one_way("name", UiBindProperty::Text, "player"));
    let (_dir, mut runtime) = Setup::new(vec![("hud", ui)]).runtime();
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiDataContext>()
        .set("player", json!("@greeting"))
        .unwrap();
    tick(&mut runtime);
    assert!(drawn(&runtime).contains(&"@greeting".to_string()));
}

// ---- missing keys and fallback ---------------------------------------------

#[test]
fn missing_key_shows_the_marker_and_is_counted_in_diagnostics() {
    let mut ui = root(vec![
        sized(UiNode::label("a", "@no-such-key"), 0.0, 0.0, 300.0, 30.0),
        sized(UiNode::label("b", ""), 0.0, 40.0, 300.0, 30.0),
    ]);
    ui.bindings
        .push(UiBinding::one_way("b", UiBindProperty::Text, "x").with_message("missing-bound", []));
    let (_dir, mut runtime) = Setup::new(vec![("hud", ui)]).runtime();
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiDataContext>()
        .set("x", json!(1))
        .unwrap();
    tick(&mut runtime);
    let texts = drawn(&runtime);
    assert!(texts.contains(&"{no-such-key}".to_string()), "{texts:?}");
    assert!(texts.contains(&"{missing-bound}".to_string()), "{texts:?}");
    let localization = runtime.localization();
    assert!(localization.stats().missing_messages >= 2);
    let missing: Vec<String> = localization
        .diagnostics()
        .into_iter()
        .filter(|d| d.kind == engine_locale::DiagnosticKind::MissingMessage)
        .map(|d| d.key)
        .collect();
    assert!(missing.contains(&"no-such-key".to_string()), "{missing:?}");
    assert!(
        missing.contains(&"missing-bound".to_string()),
        "{missing:?}"
    );
    assert_eq!(
        runtime.snapshot().localization.unwrap().missing_messages,
        localization.stats().missing_messages
    );
}

#[test]
fn regional_locale_falls_back_to_its_language_then_the_default() {
    let mut setup = Setup::new(vec![(
        "hud",
        root(vec![
            sized(UiNode::label("a", "@greeting"), 0.0, 0.0, 300.0, 30.0),
            sized(UiNode::label("b", "@farewell"), 0.0, 40.0, 300.0, 30.0),
            sized(UiNode::label("c", "@button-ok"), 0.0, 80.0, 300.0, 30.0),
        ]),
    )]);
    setup.locales.push(("fr-CA", FR_CA));
    let (_dir, mut runtime) = setup.runtime();
    runtime.set_locale("fr-CA").unwrap();
    tick(&mut runtime);
    let texts = drawn(&runtime);
    assert!(
        texts.contains(&"Bonjour".to_string()),
        "fr fallback: {texts:?}"
    );
    assert!(
        texts.contains(&"À la prochaine".to_string()),
        "own message: {texts:?}"
    );
    assert!(texts.contains(&"D'accord".to_string()), "{texts:?}");
    let chain: Vec<String> = runtime
        .localization()
        .with(|l| l.fallback_chain().iter().map(ToString::to_string).collect());
    assert_eq!(chain, ["fr-CA", "fr", "en"]);
    // Falling back is reported (not an error).
    assert!(
        runtime
            .localization()
            .diagnostics()
            .iter()
            .any(|d| d.kind == engine_locale::DiagnosticKind::FellBack && d.key == "greeting")
    );
    // A russian message that russian lacks falls back to english.
    runtime.set_locale("ru").unwrap();
    tick(&mut runtime);
    assert!(drawn(&runtime).contains(&"OK".to_string()));
}

#[test]
fn unknown_locales_fall_back_to_the_default_and_invalid_ids_are_rejected() {
    let (_dir, mut runtime) = Setup::new(label_roots()).runtime();
    runtime.set_locale("fr").unwrap();
    // A locale without its own catalog falls back along its chain to the default.
    runtime.set_locale("xx-YY").unwrap();
    assert_eq!(runtime.locale(), "xx-YY");
    tick(&mut runtime);
    assert!(drawn(&runtime).contains(&"Hello".to_string()));
    runtime.set_locale("fr").unwrap();
    let error = runtime.set_locale("not a locale").unwrap_err();
    assert_eq!(error.diagnostics[0].code, "locale.invalid_id");
}

// ---- pseudo localization ---------------------------------------------------

#[test]
fn pseudo_mode_expands_drawn_text_keeps_markup_and_toggles_at_runtime() {
    let roots = vec![(
        "hud",
        root(vec![
            sized(UiNode::label("a", "@greeting"), 0.0, 0.0, 300.0, 30.0),
            sized(UiNode::rich_label("r", "@rich"), 0.0, 40.0, 300.0, 30.0),
        ]),
    )];
    let (_dir, mut runtime) = Setup::new(roots).runtime();
    tick(&mut runtime);
    let plain = drawn(&runtime);
    runtime.set_pseudo_localization(true).unwrap();
    tick(&mut runtime);
    let pseudo = drawn(&runtime);
    assert_ne!(plain, pseudo);
    let hello = pseudo
        .iter()
        .find(|t| t.contains('\u{AB}') && !t.contains('['))
        .unwrap();
    assert!(
        hello.chars().count() > "Hello".chars().count() + 2,
        "{hello}"
    );
    assert!(hello.contains('~'), "expansion padding: {hello}");
    let rich = pseudo
        .iter()
        .find(|t| t.contains("[b]"))
        .expect("markup kept");
    assert!(rich.contains("[/b]"), "{rich}");
    assert!(rich.chars().count() > "[b]Bold[/b] text".chars().count());
    runtime.set_pseudo_localization(false).unwrap();
    tick(&mut runtime);
    assert_eq!(drawn(&runtime), plain);
}

// ---- generation-driven re-resolution ---------------------------------------

#[test]
fn text_re_resolves_only_when_the_generation_changes() {
    let (_dir, mut runtime) = Setup::new(widget_roots()).runtime();
    tick(&mut runtime);
    drawn(&runtime);
    let localization = runtime.localization();
    let baseline = localization.stats();
    assert!(
        baseline.resolutions >= 8,
        "first frame resolved the keys: {baseline:?}"
    );
    for _ in 0..30 {
        tick(&mut runtime);
        drawn(&runtime);
    }
    let steady = localization.stats();
    assert_eq!(
        steady.resolutions, baseline.resolutions,
        "no Fluent call without a generation change"
    );
    assert!(steady.cache_hits > baseline.cache_hits);

    runtime.set_locale("fr").unwrap();
    tick(&mut runtime);
    drawn(&runtime);
    let switched = localization.stats();
    assert!(switched.generation > steady.generation);
    let delta = switched.resolutions - steady.resolutions;
    assert!(
        (8..=14).contains(&delta),
        "each distinct key resolves once after the switch, got {delta}"
    );
    for _ in 0..10 {
        tick(&mut runtime);
        drawn(&runtime);
    }
    assert_eq!(localization.stats().resolutions, switched.resolutions);
}

#[test]
fn binding_messages_re_resolve_once_per_locale_change_not_per_tick() {
    let (_dir, mut runtime) = Setup::new(count_root()).runtime();
    set_count(&mut runtime, 3);
    let localization = runtime.localization();
    let after_first = localization.stats().resolutions;
    for _ in 0..25 {
        tick(&mut runtime);
    }
    assert_eq!(localization.stats().resolutions, after_first);
    runtime.set_locale("fr").unwrap();
    for _ in 0..5 {
        tick(&mut runtime);
    }
    assert_eq!(localization.stats().resolutions, after_first + 1);
    assert!(drawn(&runtime).contains(&"3 objets".to_string()));
}

#[test]
fn locale_changed_event_is_delivered_with_previous_and_current() {
    let (_dir, mut runtime) = Setup::new(label_roots()).runtime();
    tick(&mut runtime);
    runtime.set_locale("fr").unwrap();
    tick(&mut runtime);
    let events = runtime.snapshot().events;
    let event = events
        .iter()
        .find(|e| e.name == LOCALE_CHANGED_EVENT)
        .unwrap_or_else(|| panic!("{events:?}"));
    assert_eq!(event.payload["locale"], "fr");
    assert_eq!(event.payload["previous"], "en");
    // Re-selecting the current locale and toggling pseudo do not announce a change.
    runtime.set_locale("fr").unwrap();
    runtime.set_pseudo_localization(true).unwrap();
    tick(&mut runtime);
    assert!(
        !runtime
            .snapshot()
            .events
            .iter()
            .any(|e| e.name == LOCALE_CHANGED_EVENT)
    );
}

// ---- per-locale fonts and assets -------------------------------------------

fn font_setup(locale_assets: Value) -> Setup {
    let mut setup = Setup::new(label_roots());
    setup.locales.push(("ja", "greeting = Konnichiwa\n"));
    setup.locales.push(("ar", "greeting = Marhaba\n"));
    setup.config = Some(config(
        &["en", "fr", "ru", "ja", "ar"],
        json!({"assets": locale_assets}),
    ));
    setup
}

fn layout_of(runtime: &Runtime, text: &str) -> UiTextLayout {
    runtime
        .text_engine()
        .unwrap()
        .layout(&UiTextRequest {
            text,
            kind: UiTextKind::Plain,
            font: "ui",
            scale: 1.0,
            color: [1.0; 4],
            max_width: None,
        })
        .unwrap()
}

#[test]
fn per_locale_font_families_change_the_engine_text_fallback_and_language() {
    // The emoji face stands in for the CJK / Arabic fallback fonts.
    let (_dir, mut runtime) = font_setup(json!([
        {"locale": "ja", "fonts": ["emoji"]},
        {"locale": "ar", "fonts": ["emoji", "ui"]}
    ]))
    .runtime();
    let engine = runtime.text_engine().unwrap().clone();
    assert!(engine.fallback().by_language.is_empty());
    let sample = "A\u{1F600}";
    let en = layout_of(&runtime, sample);

    runtime.set_locale("ja").unwrap();
    assert_eq!(engine.fallback().by_language["ja"], ["emoji"]);
    let ja = layout_of(&runtime, sample);
    assert_eq!(ja.glyphs.len(), en.glyphs.len());
    assert_ne!(
        ja.glyphs.last().unwrap().uv,
        en.glyphs.last().unwrap().uv,
        "the emoji glyph now comes from the ja fallback face"
    );

    runtime.set_locale("ar").unwrap();
    assert_eq!(engine.fallback().by_language["ar"], ["emoji", "ui"]);
    assert!(!engine.fallback().by_language.contains_key("ja"));

    runtime.set_locale("en").unwrap();
    assert!(
        engine.fallback().by_language.is_empty(),
        "back to the base chains"
    );
    assert_eq!(
        layout_of(&runtime, sample).glyphs.last().unwrap().uv,
        en.glyphs.last().unwrap().uv
    );
}

#[test]
fn locale_fonts_extend_the_project_font_config_chain_for_that_language() {
    let mut setup = font_setup(json!([{"locale": "ja", "fonts": ["emoji"]}]));
    setup
        .assets
        .push(json!({"id":"fonts","kind":"font_config","path":"fonts.json"}));
    setup.files.push((
        "fonts.json",
        json!({"schema":1,"defaultFamily":"ui","fallback":{"byLanguage":{"ja":["ui"]}}})
            .to_string()
            .into_bytes(),
    ));
    let (_dir, mut runtime) = setup.runtime();
    let engine = runtime.text_engine().unwrap().clone();
    assert_eq!(engine.fallback().by_language["ja"], ["ui"]);
    runtime.set_locale("ja").unwrap();
    assert_eq!(
        engine.fallback().by_language["ja"],
        ["emoji", "ui"],
        "locale fonts first"
    );
    runtime.set_locale("en").unwrap();
    assert_eq!(engine.fallback().by_language["ja"], ["ui"]);
}

/// First existing path of `candidates`, or a printed skip reason.
fn system_font(script: &str, candidates: &[&str]) -> Option<Vec<u8>> {
    for path in candidates {
        if let Ok(bytes) = fs::read(path) {
            return Some(bytes);
        }
    }
    println!("SKIP system-font test for {script}: none of {candidates:?} exists on this machine");
    None
}

#[test]
fn system_fonts_supply_arabic_and_japanese_glyphs_per_locale() {
    let arabic = system_font(
        "ar",
        &[
            "C:/Windows/Fonts/segoeui.ttf",
            "C:/Windows/Fonts/tahoma.ttf",
            "C:/Windows/Fonts/arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ],
    );
    let japanese = system_font(
        "ja",
        &[
            "C:/Windows/Fonts/YuGothR.ttc",
            "C:/Windows/Fonts/meiryo.ttc",
            "C:/Windows/Fonts/msgothic.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        ],
    );
    for (script, bytes, locale, sample) in [
        ("ar", arabic, "ar", "\u{645}\u{631}\u{62d}\u{628}\u{627}"),
        (
            "ja",
            japanese,
            "ja",
            "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}",
        ),
    ] {
        let Some(bytes) = bytes else { continue };
        let mut setup = font_setup(json!([{"locale": locale, "fonts": ["sys"]}]));
        setup
            .assets
            .push(json!({"id":"sys","kind":"font_face","path":"sys.font.ttf"}));
        setup.files.push(("sys.font.ttf", bytes));
        let (_dir, project) = setup.build();
        let project = match project {
            Ok(project) => project,
            Err(error) => {
                println!("SKIP {script}: the system font is not usable ({error})");
                continue;
            }
        };
        let mut runtime = Runtime::new(project).unwrap();
        runtime.start().unwrap();
        let before = layout_of(&runtime, sample);
        runtime.set_locale(locale).unwrap();
        let after = layout_of(&runtime, sample);
        let coverage = after
            .glyphs
            .iter()
            .zip(&before.glyphs)
            .filter(|(a, b)| a.uv != b.uv)
            .count();
        assert!(
            coverage > 0 || after.metrics.width != before.metrics.width,
            "{script}: the locale font changed how the text is drawn"
        );
    }
}

fn png(color: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(4, 4, image::Rgba(color))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

#[test]
fn per_locale_asset_override_switches_the_image_texture() {
    let roots = vec![(
        "hud",
        root(vec![
            sized(
                UiNode::image("logo", "logo", UiImageFit::Stretch),
                0.0,
                0.0,
                64.0,
                64.0,
            ),
            sized(
                UiNode::nine_slice("frame", "logo", [1.0; 4]),
                80.0,
                0.0,
                64.0,
                64.0,
            ),
        ]),
    )];
    let mut setup = Setup::new(roots);
    setup.locales.push(("ja", "greeting = Konnichiwa\n"));
    setup.config = Some(config(
        &["en", "fr", "ru", "ja"],
        json!({"assets": [{"locale": "ja", "asset_overrides": {"logo": "logo-ja"}}]}),
    ));
    setup
        .assets
        .push(json!({"id":"logo","kind":"texture","path":"logo.png"}));
    setup
        .assets
        .push(json!({"id":"logo-ja","kind":"texture","path":"logo-ja.png"}));
    setup.files.push(("logo.png", png([255, 0, 0, 255])));
    setup.files.push(("logo-ja.png", png([0, 0, 255, 255])));
    let (_dir, mut runtime) = setup.runtime();
    let first_pixel = |runtime: &Runtime| -> Vec<[u8; 4]> {
        runtime
            .extract_frame()
            .unwrap()
            .draws
            .iter()
            .filter_map(|d| d.texture.as_ref())
            .filter(|t| t.width == 4)
            .map(|t| [t.pixels[0], t.pixels[1], t.pixels[2], t.pixels[3]])
            .collect()
    };
    tick(&mut runtime);
    let en = first_pixel(&runtime);
    assert!(
        !en.is_empty() && en.iter().all(|p| *p == [255, 0, 0, 255]),
        "{en:?}"
    );
    runtime.set_locale("ja").unwrap();
    tick(&mut runtime);
    let ja = first_pixel(&runtime);
    assert_eq!(ja.len(), en.len(), "image and nine-slice both draw");
    assert!(ja.iter().all(|p| *p == [0, 0, 255, 255]), "{ja:?}");
    // The stored image keeps its authored texture key.
    let stored = stored_root(&runtime, "hud");
    assert!(
        matches!(&stored.tree.nodes[0].widget, UiWidget::Image { texture, .. } if texture == "logo")
    );
    runtime.set_locale("fr").unwrap();
    tick(&mut runtime);
    assert!(first_pixel(&runtime).iter().all(|p| *p == [255, 0, 0, 255]));
}

// ---- world text ------------------------------------------------------------

#[test]
fn world_text_references_message_keys() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "ui.ttf", UBUNTU);
    write(root, "strings/en/ui.ftl", EN);
    write(root, "strings/fr/ui.ftl", FR);
    write(
        root,
        "locale.json",
        config(&["en", "fr"], json!({})).to_string(),
    );
    write(
        root,
        "scene.json",
        json!({"schema":1,"scene_id":"main","entities":[
            {"id":"sign","name":"sign","components":{
                "Transform":{"position":[0,0,0],"rotation":[0,0,0,1],"scale":[1,1,1]},
                "custom":{"engine.WorldText":{"version":1,"data":
                    {"version":1,"text":"@farewell","font":"ui","size":0.5}}}}}
        ]})
        .to_string(),
    );
    let mut assets = vec![
        json!({"id":"ui","kind":"font_face","path":"ui.ttf"}),
        json!({"id":"l10n","kind":"locale_config","path":"locale.json"}),
    ];
    assets.extend(table_assets(&["en", "fr"]));
    write(
        root,
        "project.json",
        json!({"schema":1,"name":"wt","startup_scene":"scene.json","assets":assets}).to_string(),
    );
    let mut runtime = Runtime::new(Project::load(root.join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    tick(&mut runtime);
    let glyph_vertices = |runtime: &Runtime| -> usize {
        runtime
            .extract_frame()
            .unwrap()
            .draws
            .iter()
            .filter(|d| d.id.contains("sign"))
            .map(|d| d.mesh.vertices.len())
            .sum()
    };
    let en = glyph_vertices(&runtime);
    assert_eq!(
        en,
        4 * "Goodbye".len(),
        "one quad per letter of the english text"
    );
    runtime.set_locale("fr").unwrap();
    tick(&mut runtime);
    let fr = glyph_vertices(&runtime);
    assert_eq!(fr, 4 * "Au revoir".replace(' ', "").len());
}

// ---- native API ------------------------------------------------------------

#[test]
fn native_code_can_load_strings_without_a_project_config() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "ui.ttf", UBUNTU);
    let ui = root(vec![sized(
        UiNode::label("a", "@hi"),
        0.0,
        0.0,
        200.0,
        30.0,
    )]);
    write(
        dir.path(),
        "scene.json",
        json!({"schema":1,"scene_id":"main","entities":[{"id":"hud","name":"hud","components":
            {"custom":{"engine.UiRoot":{"version":1,"data":ui}}}}]})
        .to_string(),
    );
    write(
        dir.path(),
        "project.json",
        json!({"schema":1,"name":"native","startup_scene":"scene.json",
            "assets":[{"id":"ui","kind":"font_face","path":"ui.ttf"}]})
        .to_string(),
    );
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    tick(&mut runtime);
    assert!(
        drawn(&runtime).contains(&"@hi".to_string()),
        "inactive until strings load"
    );
    let l10n: Localization = runtime.localization();
    l10n.load_ftl("en", "hi = Hey\n").unwrap();
    l10n.load_ftl("de", "hi = Hallo\n").unwrap();
    tick(&mut runtime);
    assert!(drawn(&runtime).contains(&"Hey".to_string()));
    runtime.set_locale("de").unwrap();
    tick(&mut runtime);
    assert!(drawn(&runtime).contains(&"Hallo".to_string()));
    assert_eq!(runtime.snapshot().localization.unwrap().locale, "de");
}

// ---- validation ------------------------------------------------------------

fn load_error(setup: Setup) -> String {
    let (_dir, project) = setup.build();
    project.unwrap_err().diagnostics[0].code.clone()
}

#[test]
fn authoring_mistakes_are_typed_validation_errors() {
    let base = || Setup::new(label_roots());

    let mut setup = base();
    setup.locales[1].1 = "greeting = {\n";
    assert_eq!(load_error(setup), "locale.parse");

    let mut setup = base();
    setup
        .assets
        .push(json!({"id":"extra","kind":"string_table","path":"extra.ftl"}));
    setup.files.push(("extra.ftl", b"x = 1\n".to_vec()));
    assert_eq!(load_error(setup), "asset.string_table.unlisted");

    let mut setup = base();
    setup.tables = false;
    assert_eq!(load_error(setup), "asset.locale_config.unknown_table");

    let mut setup = base();
    setup.config = Some(json!({
        "schema":1,"default_locale":"en","available_locales":["en","fr","ru","de"],
        "resources":[
            {"locale":"en","paths":["strings/en/ui.ftl"]},
            {"locale":"fr","paths":["strings/fr/ui.ftl"]},
            {"locale":"ru","paths":["strings/ru/ui.ftl"]}]
    }));
    assert_eq!(load_error(setup), "asset.locale_config.no_strings");

    let mut setup = base();
    setup.config = Some(config(
        &["en", "fr", "ru"],
        json!({"assets": [{"locale": "fr", "fonts": ["nope"]}]}),
    ));
    assert_eq!(load_error(setup), "asset.locale_config.unknown_font");

    let mut setup = base();
    setup.config = Some(config(
        &["en", "fr", "ru"],
        json!({"assets": [{"locale": "fr", "asset_overrides": {"ui": "missing"}}]}),
    ));
    assert_eq!(load_error(setup), "asset.locale_config.unknown_asset");

    let mut setup = base();
    setup.config = Some(config(&["en", "fr", "ru"], json!({"colour": 1})));
    assert_eq!(load_error(setup), "asset.locale_config.invalid");

    let mut setup = base();
    setup
        .assets
        .push(json!({"id":"l10n2","kind":"locale_config","path":"locale2.json"}));
    setup.files.push(("locale2.json", b"{}".to_vec()));
    assert_eq!(load_error(setup), "asset.locale_config.duplicate");

    let mut setup = base();
    setup.locales[0].1 = "a = 1\n";
    setup
        .assets
        .push(json!({"id":"more","kind":"string_table","path":"strings/en/more.ftl"}));
    setup
        .files
        .push(("strings/en/more.ftl", b"a = 2\n".to_vec()));
    setup.config = Some(json!({
        "schema":1,"default_locale":"en","available_locales":["en","fr","ru"],
        "resources":[
            {"locale":"en","paths":["strings/en/ui.ftl","strings/en/more.ftl"]},
            {"locale":"fr","paths":["strings/fr/ui.ftl"]},
            {"locale":"ru","paths":["strings/ru/ui.ftl"]}]
    }));
    assert_eq!(load_error(setup), "locale.duplicate_message");
}

#[test]
fn string_table_needs_a_config_and_the_ftl_extension() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.ftl", "x = 1\n");
    write(
        dir.path(),
        "scene.json",
        json!({"schema":1,"scene_id":"main","entities":[]}).to_string(),
    );
    write(
        dir.path(),
        "project.json",
        json!({"schema":1,"name":"t","startup_scene":"scene.json",
            "assets":[{"id":"a","kind":"string_table","path":"a.ftl"}]})
        .to_string(),
    );
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "asset.string_table.no_config");
    write(dir.path(), "a.txt", "x = 1\n");
    write(
        dir.path(),
        "project.json",
        json!({"schema":1,"name":"t","startup_scene":"scene.json",
            "assets":[{"id":"a","kind":"string_table","path":"a.txt"}]})
        .to_string(),
    );
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "asset.string_table.extension");
}

#[test]
fn oversized_translations_cannot_invalidate_a_root() {
    let long = "x".repeat(20_000);
    let mut setup = Setup::new(vec![(
        "hud",
        root(vec![
            sized(UiNode::label("a", "@long"), 0.0, 0.0, 300.0, 30.0),
            sized(UiNode::button("b", "@long", "go"), 0.0, 40.0, 300.0, 30.0)
                .with_tooltip("@long", 0.0),
            sized(
                UiNode::tabs("t", vec!["@long".into()]),
                0.0,
                80.0,
                300.0,
                60.0,
            ),
        ]),
    )]);
    setup.locales[0].1 = Box::leak(format!("long = {long}\n").into_boxed_str());
    let (_dir, mut runtime) = setup.runtime();
    tick(&mut runtime);
    let texts = drawn(&runtime);
    assert!(
        texts.iter().any(|t| t.len() == MAX_UI_TEXT),
        "cut to the slot limit"
    );
    assert!(
        texts.iter().any(|t| t.len() == MAX_UI_LABEL),
        "tab labels are short"
    );
}

#[test]
fn authored_pseudo_mode_applies_from_the_first_frame() {
    let mut setup = Setup::new(label_roots());
    setup.config = Some(config(
        &["en", "fr", "ru"],
        json!({"pseudo": {"enabled": true, "expansion_percent": 50, "prefix": "<<", "suffix": ">>"}}),
    ));
    let (_dir, mut runtime) = setup.runtime();
    tick(&mut runtime);
    let texts = drawn(&runtime);
    let hello = texts
        .iter()
        .find(|t| t.starts_with("<<") && t.ends_with(">>"))
        .unwrap_or_else(|| panic!("{texts:?}"));
    assert!(hello.contains("Ĥéļļó"), "accented Hello: {hello}");
    assert!(runtime.snapshot().localization.unwrap().pseudo);
    runtime.set_pseudo_localization(false).unwrap();
    tick(&mut runtime);
    assert!(drawn(&runtime).contains(&"Hello".to_string()));
}

#[test]
fn many_references_to_few_keys_format_once_per_generation() {
    let nodes: Vec<UiNode> = (0..300)
        .map(|i| {
            let key = ["@greeting", "@farewell", "@button-ok"][i % 3];
            sized(
                UiNode::label(format!("l{i}"), key),
                0.0,
                (i * 2) as f32,
                100.0,
                2.0,
            )
        })
        .collect();
    let (_dir, mut runtime) = Setup::new(vec![("hud", root(nodes))]).runtime();
    tick(&mut runtime);
    let started = std::time::Instant::now();
    let texts = drawn(&runtime);
    let first = started.elapsed();
    assert_eq!(texts.iter().filter(|t| t.as_str() == "Hello").count(), 100);
    let localization = runtime.localization();
    assert_eq!(localization.stats().resolutions, 3, "300 labels, 3 keys");
    runtime.set_locale("fr").unwrap();
    tick(&mut runtime);
    drawn(&runtime);
    assert_eq!(localization.stats().resolutions, 6);
    println!("300 localized labels drawn in {first:?} (debug build, includes layout)");
}
