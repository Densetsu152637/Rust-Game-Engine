//! `network_inspect` and `network_settings_set`, plus generic `component_set` editing of the
//! network components.
mod common;
use common::*;
use serde_json::{Value, json};
use std::fs;

const START: &str = "scenes/start.json";

fn rev(root: &std::path::Path) -> String {
    revision(root, START)
}
fn set(root: &std::path::Path, mut args: Value) -> Value {
    args["scene"] = json!(START);
    args["expected_revision"] = json!(rev(root));
    write(root, "network_settings_set", args)
}

fn report() -> Value {
    json!({"network": {
        "role": "server", "mode": "delta", "transport": "quic", "security": "mutual_tls13",
        "connected": true, "peers": [1], "peer_count": 1, "last_error": null,
        "accepted_inputs": 3, "rejected_inputs": 1,
        "links": [{"peer_id": 1, "rtt_ms": 310.0, "sent_packets": 400, "lost_packets": 60,
            "received_packets": 380, "dropped_unreliable": 4, "sent_bytes": 1, "received_bytes": 1,
            "congestion_window_bytes": 12000, "queued_bytes": 0}],
        "replication": {"schema_hash": "00ff00ff00ff00ff", "tick": 90, "entities": 12, "peers": [
            {"peer": 1, "entities_deferred": 5, "oversize_blocks": 1, "rpc_rejected": 2,
             "rpc_packets_refused": 1, "compression_ratio": 7.5, "bytes_total": 4096}],
            "peers_truncated": false, "client": null},
        "counters": {"malformed_packets": 2, "events_dropped": 0},
        "conditions": {"loss": 0.1, "latency_ms": 30},
        "voice": null, "lag_comp": {"queries": 4}
    }})
}

#[test]
fn inspect_summarizes_a_run_report_with_findings() {
    let dir = fixture();
    let result = data(dir.path(), "network_inspect", json!({"report": report()}));
    assert_eq!(result["ok"], true, "{result}");
    assert!(result["authored"].is_null());
    let session = &result["session"];
    assert_eq!(session["present"], true);
    assert_eq!(session["role"], "server");
    assert_eq!(session["mode"], "delta");
    assert_eq!(session["transport"], "quic");
    assert_eq!(session["security"], "mutual_tls13");
    assert_eq!(session["peer_count"], 1);
    assert_eq!(session["links"][0]["rtt_ms"], 310.0);
    assert_eq!(session["replication"]["peers"][0]["compression_ratio"], 7.5);
    assert_eq!(session["inputs"], json!({"accepted": 3, "rejected": 1}));
    let codes: Vec<_> = session["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    for code in [
        "packet_loss",
        "high_rtt",
        "dropped_unreliable",
        "oversize_blocks",
        "deferred_entities",
        "rpc_rejections",
        "rpc_backpressure",
        "malformed_packets",
        "conditions_active",
    ] {
        assert!(codes.contains(&code), "{code} missing from {codes:?}");
    }
    // The network object itself and a run report without a session are understood too.
    let bare = data(
        dir.path(),
        "network_inspect",
        json!({"report": report()["network"].clone()}),
    );
    assert_eq!(bare["session"]["role"], "server");
    let none = data(
        dir.path(),
        "network_inspect",
        json!({"report": {"ticks": 3}}),
    );
    assert_eq!(none["session"]["present"], false);
    assert_eq!(none["session"]["findings"][0]["code"], "no_network_session");
}

#[test]
fn inspect_reports_the_authored_setup_of_a_scene() {
    let dir = fixture();
    let plain = data(dir.path(), "network_inspect", json!({}));
    assert_eq!(plain["ok"], true, "{plain}");
    assert_eq!(plain["authored"]["server_replicas"], 0);
    assert_eq!(
        plain["authored"]["findings"][0]["code"],
        "no_session_component"
    );

    let revision = rev(dir.path());
    let created = write(
        dir.path(),
        "entity_create",
        json!({"scene":START,"expected_revision":revision,"id":"hero","components":{
            "Transform":{"position":[0,0,0]},
            "custom":{
                "engine.NetworkReplica":{"version":1,"data":{"authority":"server"}},
                "engine.NetworkReplication":{"version":1,"data":{"priority":4,"relevance":"spatial"}},
                "engine.NetworkHitboxes":{"version":1,"data":{"hitboxes":[{"shape":"capsule","radius":0.4}]}},
                "engine.NetworkVoice":{"version":1,"data":{"channel":2}}}}}),
    );
    assert_eq!(created["ok"], true, "{created}");
    let set = set(
        dir.path(),
        json!({"mode": "delta", "voice": {"enabled": true}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let scene = data(dir.path(), "network_inspect", json!({"scene": START}));
    let authored = &scene["authored"];
    assert_eq!(authored["server_replicas"], 1);
    assert_eq!(authored["hitbox_entities"], 1);
    assert_eq!(authored["voice_entities"], 1);
    assert_eq!(
        authored["replicas"][0]["replication"]["relevance"],
        "spatial"
    );
    assert_eq!(authored["session"]["config"]["mode"], "delta");
    assert_eq!(authored["findings"], json!([]));
}

#[test]
fn settings_set_creates_then_overlays_the_session_component() {
    let dir = fixture();
    let first = set(
        dir.path(),
        json!({"mode": "delta", "conditions": {"loss": 0.25, "latency_ms": 80, "seed": 5},
            "bytes_per_second": 64000, "view_radius": 30}),
    );
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["saved"], true);
    assert_eq!(first["entity"]["id"], "network_session");
    let data_ = &first["settings"];
    assert_eq!(data_["mode"], "delta");
    assert_eq!(data_["conditions"]["loss"], 0.25);
    assert_eq!(data_["bytes_per_second"], 64000);
    let stored = fs::read_to_string(dir.path().join(START)).unwrap();
    assert!(stored.contains("engine.NetworkSession"));

    // Later calls overlay field by field and reuse the same carrier entity.
    let second = set(
        dir.path(),
        json!({"conditions": {"jitter_ms": 12}, "voice": {"enabled": true, "routing": "global"}}),
    );
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(second["entity"]["id"], "network_session");
    let merged = &second["settings"];
    assert_eq!(merged["conditions"]["loss"], 0.25);
    assert_eq!(merged["conditions"]["jitter_ms"], 12);
    assert_eq!(merged["voice"]["routing"], "global");
    assert_eq!(merged["mode"], "delta");
    let entities = data(dir.path(), "scene_inspect", json!({"scene": START}))["entities"]
        .as_array()
        .unwrap()
        .len();
    let again = set(dir.path(), json!({"clear_conditions": true}));
    assert_eq!(again["ok"], true, "{again}");
    assert!(again["settings"].get("conditions").is_none());
    assert_eq!(
        data(dir.path(), "scene_inspect", json!({"scene": START}))["entities"]
            .as_array()
            .unwrap()
            .len(),
        entities
    );

    // dry runs validate without saving
    let before = fs::read(dir.path().join(START)).unwrap();
    let dry = set(dir.path(), json!({"mode": "snapshot", "dry_run": true}));
    assert_eq!(dry["saved"], false);
    assert_eq!(fs::read(dir.path().join(START)).unwrap(), before);
}

#[test]
fn settings_set_rejects_invalid_input_and_secrets() {
    let dir = fixture();
    let before = fs::read(dir.path().join(START)).unwrap();
    let args = |extra: Value| {
        let mut base = json!({"scene": START, "expected_revision": rev(dir.path())});
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        base
    };
    for bad in [
        json!({}),
        json!({"mode": "udp"}),
        json!({"conditions": {"loss": 1.5}}),
        json!({"bytes_per_second": 0}),
        json!({"voice": {"frame_ms": 1000}}),
    ] {
        let response = write(dir.path(), "network_settings_set", args(bad.clone()));
        assert_eq!(response["ok"], false, "{bad} -> {response}");
    }
    // Keys, certificates and addresses are not arguments of any network tool.
    for secret in [
        json!({"private_key": "-----BEGIN PRIVATE KEY-----"}),
        json!({"certificate": "x"}),
        json!({"address": "10.0.0.1:7777"}),
    ] {
        let responses = exchange(
            dir.path(),
            writable(),
            &[modern(
                1,
                "tools/call",
                json!({"name":"network_settings_set","arguments":args(secret)}),
            )],
        );
        assert_eq!(responses[0]["error"]["code"], -32602, "{responses:?}");
    }
    assert_eq!(fs::read(dir.path().join(START)).unwrap(), before);
    let stale = write(
        dir.path(),
        "network_settings_set",
        json!({"scene": START, "expected_revision": "stale", "mode": "delta"}),
    );
    assert_eq!(error_code(&stale), "revision_conflict");
    // The tool is write-gated.
    let hidden = exchange(
        dir.path(),
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"network_settings_set","arguments":{"scene": START, "expected_revision": "x"}}),
        )],
    );
    assert_eq!(hidden[0]["error"]["code"], -32602, "{hidden:?}");
    assert!(
        hidden[0]["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("write"))
    );
}

#[test]
fn component_set_edits_and_schema_validates_network_components() {
    let dir = fixture();
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component": "engine.NetworkReplication"}),
    );
    assert_eq!(schema["ok"], true, "{schema}");
    assert_eq!(
        schema["components"][0]["schema"]["properties"]["data"]["properties"]["relevance"]["enum"],
        json!(["always", "spatial", "owner_only"])
    );
    let prepared = write(
        dir.path(),
        "entity_update",
        json!({"scene":START,"expected_revision":rev(dir.path()),"id":"player","components":{
            "Transform":{"position":[0,0,0]},
            "custom":{"engine.NetworkReplica":{"version":1,"data":{"authority":"server"}}}}}),
    );
    assert_eq!(prepared["ok"], true, "{prepared}");
    let ok = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"player",
            "component":"engine.NetworkReplication",
            "value":{"version":1,"data":{"priority":2,"relevance":"owner_only","max_update_hz":20}}}),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    let before = fs::read(dir.path().join(START)).unwrap();
    for (component, data) in [
        ("engine.NetworkReplication", json!({"priority": 50})),
        ("engine.NetworkReplication", json!({"relevance": "never"})),
        (
            "engine.NetworkHitboxes",
            json!({"hitboxes": [{"shape":"box","half_extents":[0,1,1]}]}),
        ),
        ("engine.NetworkVoice", json!({"proximity_distance": -1})),
        (
            "engine.NetworkSession",
            json!({"conditions": {"latency_ms": 99999}}),
        ),
    ] {
        let response = write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"player",
                "component":component,"value":{"version":1,"data":data}}),
        );
        assert_eq!(response["ok"], false, "{component}: {response}");
    }
    assert_eq!(fs::read(dir.path().join(START)).unwrap(), before);
}
