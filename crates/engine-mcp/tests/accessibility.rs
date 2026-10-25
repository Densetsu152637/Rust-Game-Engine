//! Accessibility through the generic MCP tools: `engine.AccessibilitySettings`
//! is an ordinary component for `component_set`, and `engine.UiRoot` accepts
//! the authored `accessibility` field. No dedicated tool exists.
mod common;
use common::*;
use serde_json::json;

const START: &str = "scenes/start.json";

#[test]
fn accessibility_settings_are_editable_through_component_set() {
    let dir = fixture();
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"engine.AccessibilitySettings"}),
    );
    let component = &schema["components"][0];
    assert_eq!(
        component["name"], "engine.AccessibilitySettings",
        "{schema}"
    );
    let fields = &component["schema"]["properties"]["data"]["properties"];
    assert_eq!(fields["uiScale"]["maximum"], 4.0);
    assert_eq!(fields["textScale"]["minimum"], 0.5);
    assert_eq!(component["example"]["data"]["textScale"], 1.0);

    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
            "component":"engine.AccessibilitySettings",
            "value":{"version":1,"data":{"version":1,"uiScale":1.5,"textScale":1.25,
                "reducedMotion":true,"focusThickness":3,"highContrastTheme":"high-contrast"}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let stored = &set["entity"]["components"]["custom"]["engine.AccessibilitySettings"]["data"];
    assert_eq!(stored["uiScale"], 1.5);
    assert_eq!(stored["highContrastTheme"], "high-contrast");

    for bad in [
        json!({"version":1,"uiScale":10}),
        json!({"version":1,"textScale":0.1}),
        json!({"version":1,"highContrastTheme":"not valid"}),
        json!({"version":1,"colour":1}),
    ] {
        let rejected = write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
                "component":"engine.AccessibilitySettings","value":{"version":1,"data":bad}}),
        );
        assert_eq!(rejected["ok"], false, "{bad}: {rejected}");
    }
}

#[test]
fn ui_nodes_accept_the_accessibility_field_through_component_set() {
    let dir = fixture();
    let root = |accessibility: serde_json::Value| {
        json!({"version":1,"data":{"version":1,"tree":{"nodes":[
            {"id":"go","widget":{"kind":"button","text":"Go"},"accessibility":accessibility}]}}})
    };
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
            "component":"engine.UiRoot",
            "value":root(json!({"label":"Start the game","role":"link","live":"polite"}))}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let stored = &set["entity"]["components"]["custom"]["engine.UiRoot"]["data"]["tree"]["nodes"]
        [0]["accessibility"];
    assert_eq!(stored["label"], "Start the game");
    let rejected = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"player",
            "component":"engine.UiRoot","value":root(json!({"role":"window"}))}),
    );
    assert_eq!(rejected["ok"], false, "{rejected}");
}

#[test]
fn caption_tracks_and_the_player_are_editable_through_generic_tools() {
    let dir = fixture();
    let root = dir.path();
    std::fs::write(
        root.join("assets/intro.captions.json"),
        json!({"version":1,"speakers":[{"id":"ana","name":"Ana"}],"cues":[
            {"id":"a","start":0.5,"end":2.0,"speaker":"ana","text":"Hello"},
            {"id":"b","start":1.0,"end":4.0,"text":"wind howls","category":"sound_effect"}]})
        .to_string(),
    )
    .unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("project.json")).unwrap()).unwrap();
    let mut manifest = manifest;
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"intro","kind":"caption_track","path":"assets/intro.captions.json"}));
    std::fs::write(root.join("project.json"), manifest.to_string()).unwrap();

    let list = data(root, "asset_list", json!({"kind":"caption_track"}));
    assert_eq!(list["count"], 1, "{list}");
    let info = data(root, "asset_info", json!({"asset":"intro"}));
    assert_eq!(info["details"]["cues"], 2, "{info}");
    assert_eq!(info["details"]["speakers"], 1);
    assert_eq!(info["details"]["duration"], 4.0);

    let set = write(
        root,
        "component_set",
        json!({"scene":START,"expected_revision":revision(root, START),"entity":"player",
            "component":"engine.CaptionPlayer",
            "value":{"version":1,"data":{"version":1,"track":"intro","loop":true,"offset":0.5,
                "bottomMargin":64,"maxWidth":0.7}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let stored = &set["entity"]["components"]["custom"]["engine.CaptionPlayer"]["data"];
    assert_eq!(stored["track"], "intro");
    assert_eq!(stored["loop"], true);
    let rejected = write(
        root,
        "component_set",
        json!({"scene":START,"expected_revision":revision(root, START),"entity":"player",
            "component":"engine.CaptionPlayer","value":{"version":1,"data":{"version":1,"maxWidth":3}}}),
    );
    assert_eq!(rejected["ok"], false, "{rejected}");

    let set = write(
        root,
        "component_set",
        json!({"scene":START,"expected_revision":revision(root, START),"entity":"player",
            "component":"engine.AccessibilitySettings",
            "value":{"version":1,"data":{"version":1,"captionMaxLines":2,"captionSoundEffects":false,
                "captionBackgroundOpacity":0.8}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
}
