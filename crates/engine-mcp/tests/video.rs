//! Video catalogue metadata and generic component edits never execute a decoder.
mod common;
use common::*;
use serde_json::json;
#[test]
fn video_catalogue_and_component_authoring_do_not_require_decoder_tools() {
    let dir = fixture();
    let root = dir.path();
    std::fs::write(
        root.join("assets/intro.mkv"),
        b"catalogue-only bytes, deliberately not decodable media",
    )
    .unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("project.json")).unwrap()).unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"asset:intro","kind":"video","path":"assets/intro.mkv"}));
    std::fs::write(root.join("project.json"), manifest.to_string()).unwrap();
    let list = data(root, "asset_list", json!({"kind":"video"}));
    assert_eq!(list["count"], 1, "{list}");
    assert_eq!(list["assets"][0]["id"], "asset:intro");
    let info = data(root, "asset_info", json!({"asset":"asset:intro"}));
    assert_eq!(info["asset"]["kind"], "video", "{info}");
    assert_eq!(info["details"], json!({}));
    let scene = "scenes/start.json";
    let set = write(
        root,
        "component_set",
        json!({"scene":scene,"expected_revision":revision(root,scene),"entity":"player","component":"engine.VideoPlayer","value":{"version":1,"data":{"asset":"asset:intro","stream":"movie","autoplay":true}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let rejected = write(
        root,
        "component_set",
        json!({"scene":scene,"expected_revision":revision(root,scene),"entity":"player","component":"engine.VideoPlayer","value":{"version":1,"data":{"asset":"asset:intro","stream":"invalid/name"}}}),
    );
    assert_eq!(rejected["ok"], false, "{rejected}");
}
