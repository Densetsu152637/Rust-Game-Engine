//! Read-only analysis for tools: authored network setup and run-report network telemetry.
//! Nothing here opens a socket, runs game code or reads secrets.
use super::{
    NetworkSessionConfig, REPLICA_COMPONENT,
    components::{
        HITBOXES_COMPONENT, NetworkHitboxes, NetworkReplication, NetworkVoice,
        REPLICATION_COMPONENT, SESSION_COMPONENT, VOICE_COMPONENT,
    },
    error,
};
use crate::{Result, Scene};
use serde_json::{Value, json};

/// Largest report accepted by [`inspect_report`].
pub const MAX_REPORT_BYTES: usize = 256 * 1024;

fn finding(severity: &str, code: &str, message: impl Into<String>) -> Value {
    json!({"severity": severity, "code": code, "message": message.into()})
}

/// Summarizes the authored network setup of a scene and flags configuration problems.
pub fn inspect_scene(scene: &Scene) -> Result<Value> {
    let mut replicas = Vec::new();
    let mut session = None;
    let (mut server, mut remote, mut hitboxes, mut voices) = (0usize, 0usize, 0usize, 0usize);
    let mut findings = Vec::new();
    for entity in &scene.entities {
        let custom = &entity.components.custom;
        if let Some(value) = custom.get(SESSION_COMPONENT) {
            let config: NetworkSessionConfig = serde_json::from_value(value.data.clone())
                .map_err(|e| error(format!("{SESSION_COMPONENT}: {e}")))?;
            session = Some(json!({"entity": entity.id, "config": config}));
        }
        if let Some(value) = custom.get(REPLICA_COMPONENT) {
            let authority = value.data["authority"].as_str().unwrap_or("server");
            if authority == "server" {
                server += 1;
            } else {
                remote += 1;
            }
            let rule = custom
                .get(REPLICATION_COMPONENT)
                .map(|v| serde_json::from_value::<NetworkReplication>(v.data.clone()))
                .transpose()?;
            replicas.push(json!({"entity": entity.id, "authority": authority,
                "replication": rule}));
        }
        if let Some(value) = custom.get(HITBOXES_COMPONENT) {
            hitboxes += 1;
            let boxes: NetworkHitboxes = serde_json::from_value(value.data.clone())?;
            if !custom.contains_key(REPLICA_COMPONENT) {
                findings.push(finding(
                    "info",
                    "hitboxes_without_replica",
                    format!(
                        "{} records {} hitbox(es) for lag compensation but is not replicated",
                        entity.id,
                        boxes.hitboxes.len()
                    ),
                ));
            }
        }
        if let Some(value) = custom.get(VOICE_COMPONENT) {
            voices += 1;
            serde_json::from_value::<NetworkVoice>(value.data.clone())?;
        }
    }
    let config = session
        .as_ref()
        .and_then(|s| serde_json::from_value::<NetworkSessionConfig>(s["config"].clone()).ok());
    if config.is_none() {
        findings.push(finding(
            "info",
            "no_session_component",
            "No engine.NetworkSession: native defaults apply (snapshot mode, no simulation).",
        ));
    }
    if let Some(config) = &config {
        if config.mode == super::NetworkMode::Delta && server == 0 {
            findings.push(finding(
                "warning",
                "delta_without_server_replicas",
                "Delta mode is configured but no entity is a server replica.",
            ));
        }
        if config.voice.enabled && voices == 0 {
            findings.push(finding(
                "warning",
                "voice_without_participants",
                "Voice is enabled but no entity has engine.NetworkVoice: proximity routing has no positions.",
            ));
        }
        if config.conditions.conditions() != engine_network::NetworkConditions::default() {
            findings.push(finding(
                "info",
                "conditions_active",
                "Link simulation is configured; real traffic will be impaired.",
            ));
        }
    }
    if remote > 0 && server == 0 && hitboxes == 0 {
        findings.push(finding(
            "info",
            "client_only_scene",
            "Only remote templates are authored; this scene is a client view.",
        ));
    }
    Ok(json!({
        "session": session,
        "server_replicas": server,
        "remote_templates": remote,
        "hitbox_entities": hitboxes,
        "voice_entities": voices,
        "replicas": replicas,
        "findings": findings,
    }))
}

/// Locates the `network` object of a run report, snapshot or the object itself.
fn locate(report: &Value) -> Option<&Value> {
    if report.get("role").is_some() {
        return Some(report);
    }
    [
        report.get("network"),
        report.get("scene").and_then(|s| s.get("network")),
    ]
    .into_iter()
    .flatten()
    .find(|v| v.is_object())
}

fn count(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Analyses the `network` telemetry of a `project_run_headless` result or runtime snapshot:
/// role, transport, peers, link statistics and replication counters, plus findings.
pub fn inspect_report(report: &Value) -> Result<Value> {
    if report.to_string().len() > MAX_REPORT_BYTES {
        return Err(error("Network report exceeds 256 KiB"));
    }
    let Some(net) = locate(report) else {
        return Ok(json!({"present": false, "findings": [finding(
            "info", "no_network_session",
            "The report has no network telemetry: no session was attached during the run.")]}));
    };
    let mut findings = Vec::new();
    if let Some(reason) = net.get("last_error").and_then(Value::as_str) {
        findings.push(finding("warning", "last_error", reason));
    }
    if net["connected"] == json!(false) && net["role"] == json!("client") {
        findings.push(finding(
            "warning",
            "not_connected",
            "The client is not connected.",
        ));
    }
    let links = net["links"].as_array().cloned().unwrap_or_default();
    for link in &links {
        let peer = link["peer_id"].as_u64().unwrap_or(0);
        let (sent, lost) = (count(link, "sent_packets"), count(link, "lost_packets"));
        if sent >= 20 && lost * 20 > sent {
            findings.push(finding(
                "warning",
                "packet_loss",
                format!("Peer {peer}: {lost} of {sent} packets lost (over 5%)."),
            ));
        }
        if link["rtt_ms"].as_f64().is_some_and(|rtt| rtt > 250.0) {
            findings.push(finding(
                "warning",
                "high_rtt",
                format!("Peer {peer}: RTT above 250 ms."),
            ));
        }
        if count(link, "dropped_unreliable") > 0 {
            findings.push(finding(
                "info",
                "dropped_unreliable",
                format!(
                    "Peer {peer}: {} unreliable payloads were discarded.",
                    count(link, "dropped_unreliable")
                ),
            ));
        }
    }
    let replication = &net["replication"];
    for peer in replication["peers"].as_array().into_iter().flatten() {
        let id = peer["peer"].as_u64().unwrap_or(0);
        if count(peer, "oversize_blocks") > 0 {
            findings.push(finding(
                "error",
                "oversize_blocks",
                format!("Peer {id}: entity state exceeds the packet size and is never sent."),
            ));
        }
        if count(peer, "entities_deferred") > 0 {
            findings.push(finding(
                "info",
                "deferred_entities",
                format!(
                    "Peer {id}: {} entities deferred by the bandwidth budget.",
                    count(peer, "entities_deferred")
                ),
            ));
        }
        if count(peer, "rpc_rejected") > 0 {
            findings.push(finding(
                "info",
                "rpc_rejections",
                format!(
                    "Peer {id}: {} RPC calls rejected.",
                    count(peer, "rpc_rejected")
                ),
            ));
        }
        if count(peer, "rpc_packets_refused") > 0 {
            findings.push(finding(
                "warning",
                "rpc_backpressure",
                format!("Peer {id}: RPC packets were refused for backpressure."),
            ));
        }
    }
    let counters = &net["counters"];
    if count(counters, "malformed_packets") > 0 {
        findings.push(finding(
            "warning",
            "malformed_packets",
            "Malformed replication packets were received (schema mismatch or hostile peer).",
        ));
    }
    if count(counters, "events_dropped") > 0 {
        findings.push(finding(
            "warning",
            "events_dropped",
            "Network events were dropped because the engine event queue was full.",
        ));
    }
    if net["conditions"].is_object() {
        findings.push(finding(
            "info",
            "conditions_active",
            "Link simulation was active during the run.",
        ));
    }
    Ok(json!({
        "present": true,
        "role": net["role"],
        "mode": net.get("mode").cloned().unwrap_or_else(|| json!("snapshot")),
        "transport": net["transport"],
        "security": net["security"],
        "connected": net["connected"],
        "peer_count": net.get("peer_count").cloned().unwrap_or_else(|| json!(net["peers"].as_array().map_or(0, Vec::len))),
        "peers": net["peers"],
        "links": links,
        "replication": replication,
        "counters": counters,
        "inputs": {"accepted": net["accepted_inputs"], "rejected": net["rejected_inputs"]},
        "voice": net["voice"],
        "lag_comp": net["lag_comp"],
        "conditions": net["conditions"],
        "findings": findings,
    }))
}
