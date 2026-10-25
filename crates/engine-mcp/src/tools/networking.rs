//! Networking tools. The logic lives in `engine_project::networking`; these wrappers never
//! open a socket, never execute game code and never accept keys or certificates (those stay in
//! native configuration, see `docs/networking.md`).
use super::{Spec, edit, scene, tool};
use crate::{
    config::AccessLevel,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::{
    EditOperation, SceneEntity,
    networking::{self, SESSION_COMPONENT},
};
use serde_json::{Map, Value, json};

/// Entity that carries the session component when the scene has none yet.
const DEFAULT_CARRIER: &str = "network_session";

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    let number = |min: f64, max: f64, description: &str| json!({"type":"number","minimum":min,"maximum":max,"description":description});
    let conditions = schema::object(
        vec![
            (
                "loss",
                number(0.0, 1.0, "Probability an unreliable item is dropped."),
            ),
            (
                "latency_ms",
                schema::integer("Fixed one-way delay.", 0, 10_000, None),
            ),
            (
                "jitter_ms",
                schema::integer("Extra uniform delay in [0, jitter_ms].", 0, 10_000, None),
            ),
            (
                "duplicate",
                number(
                    0.0,
                    1.0,
                    "Probability an unreliable item is delivered twice.",
                ),
            ),
            (
                "reorder",
                number(
                    0.0,
                    1.0,
                    "Probability an item is held back so later ones overtake it.",
                ),
            ),
            (
                "bandwidth_bytes_per_second",
                schema::integer("Simulated link rate.", 1, 1 << 40, None),
            ),
            (
                "seed",
                schema::integer("Deterministic simulation seed.", 0, i64::MAX >> 11, None),
            ),
        ],
        &[],
    );
    let voice = schema::object(
        vec![
            (
                "enabled",
                schema::boolean("Create the voice router and receiver.", false),
            ),
            (
                "sample_rate",
                schema::integer("Raw PCM sample rate.", 8000, 48000, None),
            ),
            (
                "frame_ms",
                schema::integer("Raw PCM frame duration.", 5, 60, None),
            ),
            (
                "routing",
                schema::string_enum(
                    "Who hears a speaker.",
                    &["global", "channel", "proximity", "channel_proximity"],
                ),
            ),
            (
                "max_distance",
                number(0.01, 1.0e6, "Hearing distance of the proximity routings."),
            ),
        ],
        &[],
    );
    let described = |mut schema: Value, description: &str| {
        schema["description"] = json!(description);
        schema
    };
    let conditions = described(
        conditions,
        "Link simulation fields to overlay on the existing conditions.",
    );
    let voice = described(
        voice,
        "Voice defaults to overlay on the existing voice settings.",
    );
    let scene_arg = || {
        schema::string(
            "Project-relative scene path (see scene_list). Defaults to the startup scene.",
        )
    };
    vec![
        tool(
            Spec {
                name: "network_inspect",
                title: "Inspect network setup or session",
                description: "Two read-only views of networking. `scene` (default: the startup scene) summarizes the authored setup: the engine.NetworkSession defaults, server replicas and remote templates with their engine.NetworkReplication rules, hitbox and voice entities, and findings such as delta mode without server replicas. `report` takes the result of project_run_headless, a runtime snapshot or its `network` object and returns the session role, mode, transport, security, peers, per-peer link statistics (rtt, sent, received, lost, dropped unreliable), replication telemetry (bytes per tick, compression ratio, relevant/sent/deferred entities, RPC rejections), voice and lag-compensation counters, the active link simulation and findings. Opens no socket and runs no game code.",
                input: schema::object(
                    vec![
                        ("scene", scene_arg()),
                        (
                            "report",
                            json!({"type":"object","description":"A project_run_headless result, runtime snapshot or its `network` object (at most 256 KiB)."}),
                        ),
                    ],
                    &[],
                ),
                output: schema::output(vec![
                    (
                        "authored",
                        json!({"type":["object","null"],"description":"Authored setup of the scene; null when only a report was requested."}),
                    ),
                    (
                        "session",
                        json!({"type":["object","null"],"description":"Analysis of the supplied report; null when none was supplied."}),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            inspect,
        ),
        tool(
            Spec {
                name: "network_settings_set",
                title: "Set network session settings",
                description: "Write the project-level network defaults to the singleton engine.NetworkSession component, creating the carrying entity when the scene has none. Supplied fields overlay the existing component (conditions and voice field by field); `clear_conditions` removes the link simulation. These are read by NetworkSettings::from_scene, so headless and native runs pick them up without code changes: delta or snapshot mode, loss/latency/jitter/duplicate/reorder/bandwidth simulation, per-peer bandwidth budget, default view radius, RPC rate cap, smoothing, lag-compensation window and voice defaults. Keys, certificates and addresses are never accepted here. Validated and saved atomically like component_set.",
                input: schema::object(
                    vec![
                        (
                            "scene",
                            schema::string("Project-relative scene path (see scene_list)."),
                        ),
                        (
                            "expected_revision",
                            schema::string(
                                "Revision from scene_inspect; a stale value is rejected.",
                            ),
                        ),
                        (
                            "dry_run",
                            schema::boolean("Validate and report without saving.", false),
                        ),
                        (
                            "entity",
                            schema::string(
                                "Entity carrying engine.NetworkSession. Defaults to the entity that already has it, else network_session.",
                            ),
                        ),
                        (
                            "mode",
                            schema::string_enum(
                                "snapshot: legacy whole snapshots over TCP; delta: engine-net-replication over any transport.",
                                &["snapshot", "delta"],
                            ),
                        ),
                        ("conditions", conditions),
                        (
                            "clear_conditions",
                            schema::boolean("Remove the link simulation first.", false),
                        ),
                        (
                            "bytes_per_second",
                            schema::integer(
                                "Per-peer replication bandwidth budget.",
                                1,
                                1 << 32,
                                None,
                            ),
                        ),
                        (
                            "view_radius",
                            number(0.0, 1.0e6, "Default interest radius of a peer."),
                        ),
                        (
                            "max_entities",
                            schema::integer(
                                "Most replicated entities; identical on every endpoint.",
                                1,
                                65_536,
                                None,
                            ),
                        ),
                        (
                            "rpc_calls_per_second",
                            schema::integer(
                                "Per-peer client-to-server RPC rate cap.",
                                1,
                                10_000,
                                None,
                            ),
                        ),
                        (
                            "property_events",
                            schema::boolean(
                                "Also emit network.property_changed for pose properties.",
                                false,
                            ),
                        ),
                        (
                            "remote_smoothing_hz",
                            number(0.0, 1000.0, "Client pose smoothing rate; 0 snaps."),
                        ),
                        (
                            "lag_rewind_ticks",
                            schema::integer(
                                "Oldest tick a shooter may claim to see.",
                                1,
                                4096,
                                None,
                            ),
                        ),
                        (
                            "history_ticks",
                            schema::integer("Ticks of hitbox history retained.", 1, 4096, None),
                        ),
                        ("voice", voice),
                    ],
                    &["scene", "expected_revision"],
                ),
                output: schema::output(vec![
                    ("scene", json!({"type":"string"})),
                    ("saved", json!({"type":"boolean"})),
                    ("revision", json!({"type":"string"})),
                    ("previous_revision", json!({"type":"string"})),
                    (
                        "entity",
                        json!({"type":"object","description":"The entity carrying engine.NetworkSession after the edit."}),
                    ),
                    (
                        "settings",
                        json!({"type":"object","description":"The resulting engine.NetworkSession data."}),
                    ),
                ]),
                annotations: Annotations::mutation(true, true),
                access: AccessLevel::Write,
            },
            settings_set,
        ),
    ]
}

fn inspect(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let report = a.optional_object("report")?;
    let want_scene = a.optional_string("scene")?.is_some() || report.is_none();
    let authored = if want_scene {
        let loaded = if args.get("scene").is_some_and(|v| !v.is_null()) {
            scene::load(cx, args)?
        } else {
            let project = cx.project()?;
            let mut with_scene = args.clone();
            with_scene.insert("scene".into(), json!(super::slash(&project.startup_scene)));
            scene::load(cx, &with_scene)?
        };
        Some(networking::inspect_scene(&loaded.scene).map_err(ToolFailure::from)?)
    } else {
        None
    };
    let session = match report {
        Some(report) => Some(
            networking::inspect_report(&Value::Object(report.clone()))
                .map_err(ToolFailure::from)?,
        ),
        None => None,
    };
    Ok(ToolResult::new(
        json!({"authored": authored, "session": session}),
    ))
}

/// Copies `source[key]` into `target[key]` (an object merge for `fields`).
fn overlay(target: &mut Map<String, Value>, source: &Map<String, Value>, keys: &[&str]) {
    for key in keys {
        if let Some(value) = source.get(*key).filter(|v| !v.is_null()) {
            target.insert((*key).into(), value.clone());
        }
    }
}

fn settings_set(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let requested = a.optional_string("entity")?.map(str::to_owned);
    let clear = a.boolean("clear_conditions", false)?;
    let conditions = a.optional_object("conditions")?.cloned();
    let voice = a.optional_object("voice")?.cloned();
    let touched = [
        "mode",
        "bytes_per_second",
        "view_radius",
        "max_entities",
        "rpc_calls_per_second",
        "property_events",
        "remote_smoothing_hz",
        "lag_rewind_ticks",
        "history_ticks",
    ]
    .iter()
    .any(|key| args.get(*key).is_some_and(|v| !v.is_null()));
    if !touched && !clear && conditions.is_none() && voice.is_none() {
        return Err(schema::invalid(
            "settings",
            "at least one settings field, conditions, voice or clear_conditions",
        ));
    }
    let resulting = std::cell::RefCell::new(Value::Null);
    let outcome = edit::apply_edit(cx, args, |loaded| {
        let carrier = requested
            .clone()
            .or_else(|| {
                loaded
                    .scene
                    .entities
                    .iter()
                    .find(|e| e.components.custom.contains_key(SESSION_COMPONENT))
                    .map(|e| e.id.clone())
            })
            .unwrap_or_else(|| DEFAULT_CARRIER.to_owned());
        let existing = loaded.scene.entities.iter().find(|e| e.id == carrier);
        let mut data = existing
            .and_then(|e| e.components.custom.get(SESSION_COMPONENT))
            .and_then(|v| v.data.as_object().cloned())
            .unwrap_or_default();
        overlay(
            &mut data,
            args,
            &[
                "mode",
                "bytes_per_second",
                "view_radius",
                "max_entities",
                "rpc_calls_per_second",
                "property_events",
                "remote_smoothing_hz",
                "lag_rewind_ticks",
                "history_ticks",
            ],
        );
        let nested = |data: &mut Map<String, Value>, key: &str| -> Map<String, Value> {
            data.get(key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
        };
        if clear {
            data.remove("conditions");
        }
        if let Some(patch) = &conditions {
            let mut merged = nested(&mut data, "conditions");
            overlay(
                &mut merged,
                patch,
                &[
                    "loss",
                    "latency_ms",
                    "jitter_ms",
                    "duplicate",
                    "reorder",
                    "bandwidth_bytes_per_second",
                    "seed",
                ],
            );
            data.insert("conditions".into(), Value::Object(merged));
        }
        if let Some(patch) = &voice {
            let mut merged = nested(&mut data, "voice");
            overlay(
                &mut merged,
                patch,
                &[
                    "enabled",
                    "sample_rate",
                    "frame_ms",
                    "routing",
                    "max_distance",
                ],
            );
            data.insert("voice".into(), Value::Object(merged));
        }
        let data = Value::Object(data);
        networking::validate_component(SESSION_COMPONENT, &data).map_err(ToolFailure::from)?;
        *resulting.borrow_mut() = data.clone();
        let component = json!({"version": 1, "data": data});
        let operation = if existing.is_some() {
            EditOperation::SetComponent {
                entity_id: carrier.clone(),
                component: SESSION_COMPONENT.into(),
                value: component,
                merge: false,
            }
        } else {
            let entity: SceneEntity = serde_json::from_value(json!({
                "id": carrier, "name": carrier,
                "components": {"custom": {SESSION_COMPONENT: component}}
            }))
            .map_err(|e| schema::invalid("entity", &e.to_string()))?;
            EditOperation::CreateEntity { entity }
        };
        Ok((operation, Some(carrier)))
    })?;
    let mut result = outcome;
    if let Some(object) = result.data.as_object_mut() {
        object.insert("settings".into(), resulting.into_inner());
    }
    Ok(result)
}
