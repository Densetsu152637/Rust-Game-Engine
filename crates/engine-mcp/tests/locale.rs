//! Localization tools: string-table reads, pseudo-localization preview and
//! validated `.ftl` edits, with write gating, confinement and no partial writes.

mod common;
use common::*;
use serde_json::{Value, json};
use std::{fs, path::Path};

const EN: &str = "\
# Shown on the title screen.
greeting = Hello, { $name }!
    .tooltip = Say hi
farewell = Goodbye
items = { $count ->
    [one] { $count } item
   *[other] { $count } items
 }
rich = [b]Bold[/b] text
";

const FR: &str = "\
greeting = Bonjour, { $name } !
farewell = Au revoir
";

const RU: &str = "\
items = { $count ->
    [one] { $count } предмет
    [few] { $count } предмета
    [many] { $count } предметов
   *[other] { $count } предмета
 }
";

fn put(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn bytes(root: &Path, relative: &str) -> Vec<u8> {
    fs::read(root.join(relative)).unwrap()
}

fn config(locales: &[&str]) -> Value {
    let resources: Vec<Value> = locales
        .iter()
        .map(|l| json!({"locale": l, "paths": [format!("strings/{l}.ftl")]}))
        .collect();
    json!({"schema":1,"default_locale":"en","available_locales":locales,"resources":resources})
}

/// A project with en (default), fr (partial) and ru string tables.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(root, "strings/en.ftl", EN);
    put(root, "strings/fr.ftl", FR);
    put(root, "strings/ru.ftl", RU);
    put(
        root,
        "locale.json",
        &config(&["en", "fr", "ru"]).to_string(),
    );
    put(
        root,
        "scenes/start.json",
        &json!({"schema":1,"scene_id":"start","entities":[]}).to_string(),
    );
    put(
        root,
        "project.json",
        &json!({"schema":1,"name":"Locale Fixture","startup_scene":"scenes/start.json","assets":[
            {"id":"l10n","kind":"locale_config","path":"locale.json"},
            {"id":"strings-en","kind":"string_table","path":"strings/en.ftl"},
            {"id":"strings-fr","kind":"string_table","path":"strings/fr.ftl"},
            {"id":"strings-ru","kind":"string_table","path":"strings/ru.ftl"}]})
        .to_string(),
    );
    dir
}

fn locale<'a>(listing: &'a Value, name: &str) -> &'a Value {
    listing["locales"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["locale"] == name)
        .unwrap_or_else(|| panic!("no locale {name}: {listing}"))
}

fn assert_project_loads(root: &Path) {
    engine_project::Project::load(root.join("project.json")).unwrap();
}

// ---- reads ------------------------------------------------------------------

#[test]
fn locale_list_reports_counts_coverage_and_missing_keys() {
    let dir = project();
    let listing = data(dir.path(), "locale_list", json!({}));
    assert_eq!(listing["ok"], true, "{listing}");
    assert_eq!(listing["configured"], true);
    assert_eq!(listing["default_locale"], "en");
    assert_eq!(listing["config"]["asset"], "l10n");
    let en = locale(&listing, "en");
    assert_eq!(en["is_default"], true);
    // greeting, greeting.tooltip, farewell, items, rich
    assert_eq!(en["keys"], 5);
    assert_eq!(en["files"][0]["path"], "strings/en.ftl");
    assert_eq!(en["files"][0]["entries"], 4);
    assert_eq!(
        en["files"][0]["hash"],
        engine_mcp_hash(&bytes(dir.path(), "strings/en.ftl"))
    );
    let fr = locale(&listing, "fr");
    assert_eq!(fr["keys"], 2);
    assert_eq!(fr["total"], 5);
    assert_eq!(fr["translated"], 2);
    assert_eq!(fr["missing_count"], 3);
    assert_eq!(fr["missing"], json!(["greeting.tooltip", "items", "rich"]));
    assert_eq!(fr["extra_count"], 0);
    assert!((fr["coverage"].as_f64().unwrap() - 0.4).abs() < 1e-9);
    let ru = locale(&listing, "ru");
    assert_eq!(ru["keys"], 1);
    assert_eq!(ru["missing_count"], 4);
    // max_missing bounds the lists but not the counts.
    let bounded = data(dir.path(), "locale_list", json!({"max_missing": 1}));
    let ru = locale(&bounded, "ru");
    assert_eq!(ru["missing_count"], 4);
    assert_eq!(ru["missing"].as_array().unwrap().len(), 1);
    assert_eq!(ru["missing_truncated"], true);
    let none = data(dir.path(), "locale_list", json!({"max_missing": 0}));
    assert!(
        locale(&none, "ru")["missing"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

fn engine_mcp_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[test]
fn locale_list_without_a_config_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    put(
        dir.path(),
        "scenes/start.json",
        &json!({"schema":1,"scene_id":"start","entities":[]}).to_string(),
    );
    put(
        dir.path(),
        "project.json",
        &json!({"schema":1,"name":"Plain","startup_scene":"scenes/start.json","assets":[]})
            .to_string(),
    );
    let listing = data(dir.path(), "locale_list", json!({}));
    assert_eq!(listing["ok"], true);
    assert_eq!(listing["configured"], false);
    assert!(listing["locales"].as_array().unwrap().is_empty());
    let messages = data(dir.path(), "locale_messages", json!({"locale": "en"}));
    assert_eq!(error_code(&messages), "locale_not_configured");
}

#[test]
fn locale_messages_returns_exact_source_pages_and_filters() {
    let dir = project();
    let all = data(dir.path(), "locale_messages", json!({"locale": "en"}));
    assert_eq!(all["total"], 4, "{all}");
    let messages = all["messages"].as_array().unwrap();
    assert_eq!(messages[0]["id"], "greeting");
    assert_eq!(
        messages[0]["source"],
        "greeting = Hello, { $name }!\n    .tooltip = Say hi"
    );
    assert_eq!(messages[0]["comment"], "# Shown on the title screen.");
    assert_eq!(messages[0]["file"], "strings/en.ftl");
    assert_eq!(messages[0]["line"], 2);
    assert!(
        messages[2]["source"]
            .as_str()
            .unwrap()
            .contains("*[other] { $count } items")
    );
    let page = data(
        dir.path(),
        "locale_messages",
        json!({"locale": "en", "limit": 2}),
    );
    assert_eq!(page["returned"], 2);
    assert_eq!(page["truncated"], true);
    assert_eq!(page["next_offset"], 2);
    let tail = data(
        dir.path(),
        "locale_messages",
        json!({"locale": "en", "offset": 2, "limit": 5}),
    );
    assert_eq!(tail["returned"], 2);
    assert_eq!(tail["truncated"], false);
    let prefix = data(
        dir.path(),
        "locale_messages",
        json!({"locale": "en", "prefix": "f"}),
    );
    assert_eq!(prefix["total"], 1);
    assert_eq!(prefix["messages"][0]["id"], "farewell");
    let unknown = data(dir.path(), "locale_messages", json!({"locale": "de"}));
    assert_eq!(error_code(&unknown), "locale_unknown");
    let invalid = data(
        dir.path(),
        "locale_messages",
        json!({"locale": "not a locale"}),
    );
    assert_eq!(error_code(&invalid), "invalid_argument");
}

#[test]
fn locale_messages_previews_plurals_and_fallback() {
    let dir = project();
    let ru = |count: u64| {
        data(
            dir.path(),
            "locale_messages",
            json!({"locale": "ru", "key": "items", "args": {"count": count}}),
        )["preview"]
            .clone()
    };
    assert_eq!(ru(1)["text"], "1 предмет");
    assert_eq!(ru(2)["text"], "2 предмета");
    assert_eq!(ru(5)["text"], "5 предметов");
    assert_eq!(ru(21)["text"], "21 предмет");
    assert_eq!(ru(5)["fell_back"], false);
    let en = data(
        dir.path(),
        "locale_messages",
        json!({"locale": "en", "key": "items", "args": {"count": 1}}),
    );
    assert_eq!(en["preview"]["text"], "1 item");
    assert_eq!(en["total"], 1, "`key` selects that message");
    // fr lacks `items`: the preview falls back to the default locale.
    let fr = data(
        dir.path(),
        "locale_messages",
        json!({"locale": "fr", "key": "items", "args": {"count": 3}}),
    );
    assert_eq!(fr["preview"]["text"], "3 items");
    assert_eq!(fr["preview"]["resolved_locale"], "en");
    assert_eq!(fr["preview"]["fell_back"], true);
    assert_eq!(fr["total"], 0);
    // Attribute keys and a missing variable.
    let tooltip = data(
        dir.path(),
        "locale_messages",
        json!({"locale": "en", "key": "greeting.tooltip"}),
    );
    assert_eq!(tooltip["preview"]["text"], "Say hi");
    let missing_var = data(
        dir.path(),
        "locale_messages",
        json!({"locale": "en", "key": "greeting"}),
    );
    assert_eq!(missing_var["preview"]["defined"], true);
    assert!(
        !missing_var["preview"]["format_errors"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let nowhere = data(
        dir.path(),
        "locale_messages",
        json!({"locale": "en", "key": "nope"}),
    );
    assert_eq!(nowhere["preview"]["defined"], false);
    assert_eq!(nowhere["preview"]["text"], "{nope}");
    let bad_args = data(
        dir.path(),
        "locale_messages",
        json!({"locale": "en", "key": "items", "args": {"count": [1]}}),
    );
    assert_eq!(error_code(&bad_args), "invalid_argument");
}

#[test]
fn pseudo_preview_expands_keeps_placeables_and_markup() {
    let dir = project();
    let text = "Hello, { $name }! [b]Bold[/b] and [color=#f00]red[/color].";
    let result = data(dir.path(), "locale_pseudo_preview", json!({"text": text}));
    assert_eq!(result["ok"], true, "{result}");
    let pseudo = result["pseudo"].as_str().unwrap();
    for kept in ["{ $name }", "[b]", "[/b]", "[color=#f00]", "[/color]"] {
        assert!(pseudo.contains(kept), "{kept} in {pseudo}");
    }
    assert!(pseudo.starts_with('\u{AB}') && pseudo.ends_with('\u{BB}'));
    assert!(pseudo.contains('~'));
    assert!(result["pseudo_chars"].as_u64().unwrap() > result["original_chars"].as_u64().unwrap());
    assert!(result["growth_percent"].as_f64().unwrap() > 5.0);
    // Options override the authored ones.
    let plain = data(
        dir.path(),
        "locale_pseudo_preview",
        json!({"text": "abc", "options": {"accents": false, "expansion_percent": 0,
            "prefix": "<", "suffix": ">"}}),
    );
    assert_eq!(plain["pseudo"], "<abc>");
    let rtl = data(
        dir.path(),
        "locale_pseudo_preview",
        json!({"text": "abc", "options": {"rtl": true}}),
    );
    assert!(rtl["pseudo"].as_str().unwrap().starts_with('\u{202E}'));
    // By key: formatted first, then transformed.
    let keyed = data(
        dir.path(),
        "locale_pseudo_preview",
        json!({"key": "farewell", "locale": "fr"}),
    );
    assert_eq!(keyed["original"], "Au revoir");
    assert!(keyed["pseudo"].as_str().unwrap().contains("Áú"));
    let rich = data(dir.path(), "locale_pseudo_preview", json!({"key": "rich"}));
    assert!(rich["pseudo"].as_str().unwrap().contains("[b]"));
    // Exactly one source; bounded options.
    assert_eq!(
        error_code(&data(dir.path(), "locale_pseudo_preview", json!({}))),
        "invalid_argument"
    );
    assert_eq!(
        error_code(&data(
            dir.path(),
            "locale_pseudo_preview",
            json!({"text": "a", "key": "farewell"})
        )),
        "invalid_argument"
    );
    assert_eq!(
        error_code(&data(
            dir.path(),
            "locale_pseudo_preview",
            json!({"text": "a", "options": {"expansion_percent": 301}})
        )),
        "invalid_argument"
    );
    assert_eq!(
        error_code(&data(
            dir.path(),
            "locale_pseudo_preview",
            json!({"text": "a", "options": {"prefix": "123456789"}})
        )),
        "locale.invalid_pseudo"
    );
}

// ---- gating and schemas -------------------------------------------------------

const WRITE_LOCALE_TOOLS: [&str; 3] = [
    "locale_set_message",
    "locale_remove_message",
    "locale_add_locale",
];

fn locale_args(tool: &str) -> Value {
    match tool {
        "locale_set_message" => json!({"locale": "fr", "ftl": "extra = Extra"}),
        "locale_remove_message" => json!({"locale": "fr", "id": "farewell"}),
        _ => json!({"locale": "de", "path": "strings/de.ftl"}),
    }
}

#[test]
fn read_only_servers_hide_and_reject_the_write_tools_but_keep_the_reads() {
    let dir = project();
    let before: Vec<Vec<u8>> = ["project.json", "locale.json", "strings/fr.ftl"]
        .iter()
        .map(|p| bytes(dir.path(), p))
        .collect();
    let responses = exchange(
        dir.path(),
        read_only(),
        &[modern(1, "tools/list", json!({}))],
    );
    let names: Vec<String> = responses[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    for read in ["locale_list", "locale_messages", "locale_pseudo_preview"] {
        assert!(names.contains(&read.to_owned()), "{read}");
    }
    for tool in WRITE_LOCALE_TOOLS {
        assert!(!names.contains(&tool.to_owned()), "{tool} must be hidden");
        let responses = exchange(
            dir.path(),
            read_only(),
            &[modern(
                1,
                "tools/call",
                json!({"name": tool, "arguments": locale_args(tool)}),
            )],
        );
        assert_eq!(responses[0]["error"]["code"], -32602, "{tool}");
        let message = responses[0]["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("disabled") && message.contains("write"),
            "{message}"
        );
    }
    let after: Vec<Vec<u8>> = ["project.json", "locale.json", "strings/fr.ftl"]
        .iter()
        .map(|p| bytes(dir.path(), p))
        .collect();
    assert_eq!(before, after, "rejected calls write nothing");
}

#[test]
fn locale_tool_schemas_are_closed_described_and_accurately_annotated() {
    let dir = project();
    let responses = exchange(
        dir.path(),
        writable(),
        &[modern(1, "tools/list", json!({}))],
    );
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    for name in [
        "locale_list",
        "locale_messages",
        "locale_pseudo_preview",
        "locale_set_message",
        "locale_remove_message",
        "locale_add_locale",
    ] {
        let tool = tools.iter().find(|t| t["name"] == name).unwrap();
        assert!(tool["description"].as_str().unwrap().len() > 80, "{name}");
        assert!(tool["outputSchema"].is_object(), "{name}");
        let schema = &tool["inputSchema"];
        assert_eq!(schema["additionalProperties"], false, "{name}");
        for (property, definition) in schema["properties"].as_object().unwrap() {
            assert!(
                definition["description"]
                    .as_str()
                    .is_some_and(|d| d.len() > 8),
                "{name}.{property} needs a description"
            );
            assert!(definition["type"].is_string(), "{name}.{property}");
        }
        for required in schema["required"].as_array().unwrap() {
            assert!(schema["properties"][required.as_str().unwrap()].is_object());
        }
        let read_only = !WRITE_LOCALE_TOOLS.contains(&name);
        assert_eq!(tool["annotations"]["readOnlyHint"], read_only, "{name}");
        assert_eq!(tool["annotations"]["openWorldHint"], false, "{name}");
    }
}

// ---- edits --------------------------------------------------------------------

fn file_hash(root: &Path, relative: &str) -> String {
    engine_mcp_hash(&bytes(root, relative))
}

#[test]
fn set_message_adds_and_replaces_in_place_and_keeps_everything_else() {
    let dir = project();
    let root = dir.path();
    // Add a new message to fr.
    let added = write(
        root,
        "locale_set_message",
        json!({"locale": "fr", "ftl": "rich = [b]Gras[/b] texte"}),
    );
    assert_eq!(added["ok"], true, "{added}");
    assert_eq!(added["action"], "created");
    assert_eq!(added["path"], "strings/fr.ftl");
    assert_eq!(added["asset"], "strings-fr");
    assert_eq!(added["keys"], 3);
    assert_eq!(added["missing_in"], json!(["ru"]));
    assert_eq!(
        String::from_utf8(bytes(root, "strings/fr.ftl")).unwrap(),
        format!("{FR}\nrich = [b]Gras[/b] texte\n")
    );
    assert_eq!(added["hash"], file_hash(root, "strings/fr.ftl"));
    // Replace a message with a plural selector spanning lines, in place.
    let plural =
        "items = { $count ->\n    [one] { $count } objet\n   *[other] { $count } objets\n }";
    let replaced = write(
        root,
        "locale_set_message",
        json!({"locale": "fr", "ftl": plural}),
    );
    assert_eq!(replaced["ok"], true, "{replaced}");
    assert_eq!(replaced["action"], "created");
    let replaced = write(
        root,
        "locale_set_message",
        json!({"locale": "fr", "ftl": "farewell = À bientôt"}),
    );
    assert_eq!(replaced["action"], "replaced");
    assert_eq!(replaced["previous"], "farewell = Au revoir");
    let text = String::from_utf8(bytes(root, "strings/fr.ftl")).unwrap();
    assert!(text.starts_with("greeting = Bonjour, { $name } !\nfarewell = À bientôt\n"));
    assert!(text.contains("*[other] { $count } objets"));
    // The edit is visible through the read tools and the project still loads.
    let preview = data(
        root,
        "locale_messages",
        json!({"locale": "fr", "key": "items", "args": {"count": 5}}),
    );
    assert_eq!(preview["preview"]["text"], "5 objets");
    assert_eq!(preview["preview"]["fell_back"], false);
    assert_project_loads(root);
    // A replaced entry keeps its comment unless the new text has one.
    let kept = write(
        root,
        "locale_set_message",
        json!({"locale": "en", "ftl": "greeting = Hi, { $name }!\n    .tooltip = Wave"}),
    );
    assert_eq!(kept["action"], "replaced");
    let en = String::from_utf8(bytes(root, "strings/en.ftl")).unwrap();
    assert!(en.starts_with(
        "# Shown on the title screen.\ngreeting = Hi, { $name }!\n    .tooltip = Wave\nfarewell = Goodbye\n"
    ));
}

#[test]
fn set_message_rejects_invalid_fluent_without_touching_the_file() {
    let dir = project();
    let root = dir.path();
    let before = bytes(root, "strings/fr.ftl");
    let manifest = bytes(root, "project.json");
    for (ftl, code) in [
        ("broken = { $x", "locale.parse"),
        ("two = 1\nthree = 2", "locale.ftl.shape"),
        ("just some text", "locale.ftl.shape"),
        ("", "locale.ftl.shape"),
        ("bad = { $a -> [one] x }", "locale.parse"),
        ("deep = {{{{{{{{{ $a }}}}}}}}}", "locale.too_deep"),
    ] {
        let result = write(
            root,
            "locale_set_message",
            json!({"locale": "fr", "ftl": ftl}),
        );
        assert_eq!(error_code(&result), code, "{ftl:?}: {result}");
        assert_eq!(bytes(root, "strings/fr.ftl"), before, "{ftl:?}");
    }
    assert_eq!(bytes(root, "project.json"), manifest);
    // Unknown locale, unknown file, stale hash: also no change.
    for (args, code) in [
        (json!({"locale": "de", "ftl": "a = 1"}), "locale_unknown"),
        (
            json!({"locale": "fr", "ftl": "a = 1", "file": "strings/en.ftl"}),
            "locale_table_missing",
        ),
        (
            json!({"locale": "fr", "ftl": "a = 1", "file": "../outside.ftl"}),
            "locale_table_missing",
        ),
        (
            json!({"locale": "fr", "ftl": "a = 1", "expected_hash": "stale"}),
            "revision_conflict",
        ),
        (json!({"locale": "??", "ftl": "a = 1"}), "invalid_argument"),
    ] {
        let result = write(root, "locale_set_message", args.clone());
        assert_eq!(error_code(&result), code, "{args}: {result}");
        assert_eq!(bytes(root, "strings/fr.ftl"), before);
    }
    // The right hash is accepted.
    let ok = write(
        root,
        "locale_set_message",
        json!({"locale": "fr", "ftl": "a = 1", "expected_hash": file_hash(root, "strings/fr.ftl")}),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    assert_ne!(bytes(root, "strings/fr.ftl"), before);
}

#[test]
fn set_message_refuses_duplicates_across_a_locales_files() {
    let dir = project();
    let root = dir.path();
    put(root, "strings/fr-more.ftl", "extra = Extra\n");
    let mut manifest: Value = serde_json::from_slice(&bytes(root, "project.json")).unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"strings-fr-more","kind":"string_table","path":"strings/fr-more.ftl"}));
    put(root, "project.json", &manifest.to_string());
    let mut cfg = config(&["en", "fr", "ru"]);
    cfg["resources"][1]["paths"] = json!(["strings/fr.ftl", "strings/fr-more.ftl"]);
    put(root, "locale.json", &cfg.to_string());
    assert_project_loads(root);
    let before = (
        bytes(root, "strings/fr.ftl"),
        bytes(root, "strings/fr-more.ftl"),
    );
    // `farewell` lives in fr.ftl; forcing it into the second file would duplicate it.
    let result = write(
        root,
        "locale_set_message",
        json!({"locale": "fr", "ftl": "farewell = Salut", "file": "strings-fr-more"}),
    );
    assert_eq!(error_code(&result), "locale.duplicate_message", "{result}");
    // Without `file` the defining file is edited; a new id goes to the first file.
    let result = write(
        root,
        "locale_set_message",
        json!({"locale": "fr", "ftl": "extra = Plus"}),
    );
    assert_eq!(result["path"], "strings/fr-more.ftl", "{result}");
    assert_eq!(result["action"], "replaced");
    let result = write(
        root,
        "locale_set_message",
        json!({"locale": "fr", "ftl": "fresh = Neuf"}),
    );
    assert_eq!(result["path"], "strings/fr.ftl");
    assert!(bytes(root, "strings/fr.ftl").len() > before.0.len());
    let listing = data(root, "locale_list", json!({}));
    assert_eq!(locale(&listing, "fr")["files"].as_array().unwrap().len(), 2);
    assert_eq!(locale(&listing, "fr")["keys"], 4);
}

#[test]
fn remove_message_edits_the_file_and_reports_other_locales() {
    let dir = project();
    let root = dir.path();
    let removed = write(
        root,
        "locale_remove_message",
        json!({"locale": "en", "id": "greeting"}),
    );
    assert_eq!(removed["ok"], true, "{removed}");
    assert_eq!(
        removed["removed"],
        "greeting = Hello, { $name }!\n    .tooltip = Say hi"
    );
    assert_eq!(removed["still_defined_in"], json!(["fr"]));
    let en = String::from_utf8(bytes(root, "strings/en.ftl")).unwrap();
    assert!(en.starts_with("farewell = Goodbye\nitems = "), "{en}");
    assert!(
        !en.contains("Shown on the title screen"),
        "the comment goes too"
    );
    assert_project_loads(root);
    // fr now has an extra key relative to the default locale.
    let listing = data(root, "locale_list", json!({}));
    assert_eq!(locale(&listing, "fr")["extra"], json!(["greeting"]));
    // Missing id and stale hash leave the file alone.
    let before = bytes(root, "strings/en.ftl");
    let missing = write(
        root,
        "locale_remove_message",
        json!({"locale": "en", "id": "nope"}),
    );
    assert_eq!(error_code(&missing), "locale_message_not_found");
    let stale = write(
        root,
        "locale_remove_message",
        json!({"locale": "en", "id": "farewell", "expected_hash": "x"}),
    );
    assert_eq!(error_code(&stale), "revision_conflict");
    assert_eq!(bytes(root, "strings/en.ftl"), before);
    // Removing the last entry leaves a valid, empty table.
    for id in ["farewell", "items", "rich"] {
        let result = write(
            root,
            "locale_remove_message",
            json!({"locale": "en", "id": id}),
        );
        assert_eq!(result["ok"], true, "{result}");
    }
    assert_eq!(bytes(root, "strings/en.ftl"), b"");
    assert_project_loads(root);
}

#[test]
fn add_locale_creates_the_file_and_wires_the_project() {
    let dir = project();
    let root = dir.path();
    let manifest_before = String::from_utf8(bytes(root, "project.json")).unwrap();
    let result = write(
        root,
        "locale_add_locale",
        json!({"locale": "de", "path": "strings/de/ui.ftl", "copy_from": "en"}),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["asset"]["id"], "strings-de");
    assert_eq!(result["entries"], 4);
    assert_eq!(result["copied_from"], "en");
    assert_eq!(result["config"]["created"], false);
    let seeded = String::from_utf8(bytes(root, "strings/de/ui.ftl")).unwrap();
    assert!(seeded.starts_with("# Strings for de, seeded from en"));
    assert!(seeded.contains("farewell = Goodbye"));
    // The manifest only gained the asset: nothing else was reformatted.
    let manifest = String::from_utf8(bytes(root, "project.json")).unwrap();
    assert!(manifest.len() > manifest_before.len());
    assert!(manifest.contains(r#""id":"strings-de""#));
    let project = engine_project::Project::load(root.join("project.json")).unwrap();
    assert_eq!(project.assets["strings-de"].kind, "string_table");
    let config: Value = serde_json::from_slice(&bytes(root, "locale.json")).unwrap();
    assert_eq!(config["schema"], 1);
    assert_eq!(config["available_locales"], json!(["en", "fr", "ru", "de"]));
    assert_eq!(
        config["resources"][3]["paths"],
        json!(["strings/de/ui.ftl"])
    );
    let listing = data(root, "locale_list", json!({}));
    let de = locale(&listing, "de");
    assert_eq!(de["keys"], 5);
    assert_eq!(de["missing_count"], 0);
    // Edit the new locale, then refuse to add it twice.
    let set = write(
        root,
        "locale_set_message",
        json!({"locale": "de", "ftl": "farewell = Auf Wiedersehen"}),
    );
    assert_eq!(set["action"], "replaced", "{set}");
    let again = write(
        root,
        "locale_add_locale",
        json!({"locale": "de", "path": "strings/de2.ftl"}),
    );
    assert_eq!(error_code(&again), "locale_exists");
    let info = data(root, "asset_info", json!({"asset": "strings-de"}));
    assert_eq!(info["details"]["locales"], json!(["de"]), "{info}");
}

#[test]
fn add_locale_bootstraps_a_project_without_localization() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(
        root,
        "scenes/start.json",
        &json!({"schema":1,"scene_id":"start","entities":[]}).to_string(),
    );
    put(
        root,
        "project.json",
        "{\n  \"schema\": 1,\n  \"name\": \"Bare\",\n  \"startup_scene\": \"scenes/start.json\",\n  \"assets\": []\n}\n",
    );
    let result = write(
        root,
        "locale_add_locale",
        json!({"locale": "en-GB", "path": "strings/en-GB.ftl"}),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["config"]["created"], true);
    assert_eq!(result["config"]["path"], "localization.json");
    let project = engine_project::Project::load(root.join("project.json")).unwrap();
    assert_eq!(project.assets["localization"].kind, "locale_config");
    assert_eq!(project.assets["strings-en-GB"].kind, "string_table");
    let listing = data(root, "locale_list", json!({}));
    assert_eq!(listing["default_locale"], "en-GB");
    let set = write(
        root,
        "locale_set_message",
        json!({"locale": "en-GB", "ftl": "colour = Colour"}),
    );
    assert_eq!(set["action"], "created", "{set}");
    assert_eq!(
        data(
            root,
            "locale_messages",
            json!({"locale": "en-GB", "key": "colour"})
        )["preview"]["text"],
        "Colour"
    );
}

#[test]
fn add_locale_is_confined_never_overwrites_and_leaves_no_partial_state() {
    let dir = project();
    let root = dir.path();
    let manifest = bytes(root, "project.json");
    let config = bytes(root, "locale.json");
    for (args, code) in [
        (
            json!({"locale": "de", "path": "../escape.ftl"}),
            "path_invalid",
        ),
        (
            json!({"locale": "de", "path": "/etc/de.ftl"}),
            "path_invalid",
        ),
        (
            json!({"locale": "de", "path": ".git/de.ftl"}),
            "path_invalid",
        ),
        (
            json!({"locale": "de", "path": "target/de.ftl"}),
            "path_protected",
        ),
        (
            json!({"locale": "de", "path": "strings/de.txt"}),
            "invalid_argument",
        ),
        (
            json!({"locale": "de", "path": "strings/en.ftl"}),
            "path_exists",
        ),
        (
            json!({"locale": "de", "path": "strings/de.ftl", "asset_id": "strings-en"}),
            "asset_exists",
        ),
        (
            json!({"locale": "de", "path": "strings/de.ftl", "copy_from": "xx"}),
            "locale_unknown",
        ),
        (
            json!({"locale": "fr", "path": "strings/fr2.ftl"}),
            "locale_exists",
        ),
        (
            json!({"locale": "bad locale", "path": "strings/x.ftl"}),
            "invalid_argument",
        ),
    ] {
        let result = write(root, "locale_add_locale", args.clone());
        assert_eq!(error_code(&result), code, "{args}: {result}");
    }
    assert_eq!(bytes(root, "project.json"), manifest);
    assert_eq!(bytes(root, "locale.json"), config);
    assert!(!root.join("strings/de.ftl").exists());
    assert!(!root.join("escape.ftl").exists());
}

#[test]
fn new_asset_kinds_appear_in_asset_list_and_asset_info() {
    let dir = project();
    let root = dir.path();
    let tables = data(root, "asset_list", json!({"kind": "string_table"}));
    assert_eq!(tables["count"], 3, "{tables}");
    let configs = data(root, "asset_list", json!({"kind": "locale_config"}));
    assert_eq!(configs["count"], 1);
    let info = data(root, "asset_info", json!({"asset": "strings-en"}));
    assert_eq!(info["details"]["entries"], 4, "{info}");
    assert_eq!(info["details"]["locales"], json!(["en"]));
    let info = data(root, "asset_info", json!({"asset": "l10n"}));
    assert_eq!(info["details"]["default_locale"], "en", "{info}");
    assert_eq!(
        info["details"]["available_locales"],
        json!(["en", "fr", "ru"])
    );
    assert_eq!(info["details"]["string_tables"], 3);
}

#[test]
fn project_validate_reports_broken_string_tables() {
    let dir = project();
    let root = dir.path();
    put(root, "strings/fr.ftl", "broken = {\n");
    let result = data(root, "project_validate", json!({}));
    assert_eq!(result["ok"], false, "{result}");
    assert!(
        result.to_string().contains("locale.parse"),
        "typed code: {result}"
    );
    // The read tools fail with the same code instead of panicking.
    let listing = data(root, "locale_list", json!({}));
    assert_eq!(error_code(&listing), "locale.parse");
}
