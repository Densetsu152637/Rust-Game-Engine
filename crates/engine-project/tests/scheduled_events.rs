use engine_project::*;
use serde_json::json;

fn fixture() -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({
            "schema":1, "scene_id":"main", "entities":[{"id":"player","name":"Player","components":{
                "Transform":{"position":[0,0,0]},
                "EventBindings":{"bindings":[{"event":"game.increment","handler":"increment"}]}
            }}]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("project.json"),
        r#"{"schema":1,"name":"events","startup_scene":"scene.json","assets":[]}"#,
    )
    .unwrap();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    register_event_handler(runtime.app_mut(), "increment", |world, entity, event| {
        world.get_mut::<Transform>(entity).unwrap().position[0] +=
            event.payload["amount"].as_f64().unwrap() as f32;
        Ok(())
    })
    .unwrap();
    runtime.start().unwrap();
    (dir, runtime)
}
fn make_schedule(value: serde_json::Value) -> EventSchedule {
    EventSchedule::from_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
#[test]
fn scheduled_callback_runs_on_requested_tick_and_report_retains_stable_ids() {
    let (_dir, mut runtime) = fixture();
    let schedule = make_schedule(json!([
        {"tick":1,"name":"game.increment","source":"player","target":"player","payload":{"amount":3}},
        {"tick":1,"name":"game.increment","target":"player","payload":{"amount":4}}
    ]));
    schedule.validate_run(2).unwrap();
    runtime.enqueue_scheduled_events(&schedule, 0).unwrap();
    runtime.tick(1.0 / 60.0, Default::default()).unwrap();
    assert_eq!(runtime.position("player").unwrap()[0], 0.0);
    runtime.enqueue_scheduled_events(&schedule, 1).unwrap();
    runtime.tick(1.0 / 60.0, Default::default()).unwrap();
    assert_eq!(runtime.position("player").unwrap()[0], 7.0);
    runtime.stop().unwrap();
    let report = runtime.try_snapshot().unwrap();
    assert_eq!(report.events.len(), 2);
    assert_eq!(report.events[0].source.as_deref(), Some("player"));
    assert_eq!(report.events[0].target.as_deref(), Some("player"));
    assert_eq!(report.events[0].payload["amount"], 3);
    assert_eq!(report.events[1].payload["amount"], 4);
}
#[test]
fn missing_ids_reject_entire_tick_without_enqueuing_valid_prefix() {
    for missing_field in ["source", "target"] {
        let (_dir, mut runtime) = fixture();
        let mut bad =
            json!({"tick":0,"name":"game.increment","target":"player","payload":{"amount":2}});
        bad[missing_field] = json!("missing");
        let schedule = make_schedule(json!([
            {"tick":0,"name":"game.increment","target":"player","payload":{"amount":1}}, bad
        ]));
        assert!(
            runtime
                .enqueue_scheduled_events(&schedule, 0)
                .unwrap_err()
                .to_string()
                .contains("event.schedule.entity_missing")
        );
        runtime.tick(1.0 / 60.0, Default::default()).unwrap();
        assert_eq!(runtime.position("player").unwrap()[0], 0.0);
        assert!(runtime.try_snapshot().unwrap().events.is_empty());
    }
}
#[test]
fn queue_capacity_and_runtime_payload_limits_reject_batches_atomically() {
    let (_dir, mut runtime) = fixture();
    runtime.app_mut().world_mut().insert_resource(EventLimits {
        queued: 2,
        ..Default::default()
    });
    emit_event(
        runtime.app_mut().world_mut(),
        EngineEvent {
            name: "existing".into(),
            source: None,
            target: None,
            payload: json!(null),
        },
    )
    .unwrap();
    let schedule = make_schedule(json!([
        {"tick":0,"name":"game.increment","target":"player","payload":{"amount":1}},
        {"tick":0,"name":"game.increment","target":"player","payload":{"amount":2}}
    ]));
    assert!(runtime.enqueue_scheduled_events(&schedule, 0).is_err());
    runtime.tick(1.0 / 60.0, Default::default()).unwrap();
    assert_eq!(runtime.try_snapshot().unwrap().events[0].name, "existing");
    assert_eq!(runtime.position("player").unwrap()[0], 0.0);
    runtime.app_mut().world_mut().insert_resource(EventLimits {
        payload_bytes: 2,
        ..Default::default()
    });
    let schedule = make_schedule(json!([
        {"tick":1,"name":"first","payload":0},
        {"tick":1,"name":"second","payload":"too big"}
    ]));
    assert!(runtime.enqueue_scheduled_events(&schedule, 1).is_err());
    runtime.tick(1.0 / 60.0, Default::default()).unwrap();
    assert!(runtime.try_snapshot().unwrap().events.is_empty());
}
#[test]
fn ids_are_resolved_at_delivery_time_and_despawned_targets_are_errors() {
    let (_dir, mut runtime) = fixture();
    let schedule = make_schedule(
        json!([{ "tick":5,"name":"game.increment","target":"player","payload":{"amount":2}}]),
    );
    let player = runtime.entity("player").unwrap();
    runtime.app_mut().world_mut().despawn(player);
    assert!(
        runtime
            .enqueue_scheduled_events(&schedule, 5)
            .unwrap_err()
            .to_string()
            .contains("entity_missing")
    );
}
#[test]
fn schedule_rejects_malformed_unordered_oversized_or_out_of_run_inputs() {
    for invalid in [
        json!([{ "tick":0,"name":"" }]),
        json!([{ "tick":0,"name":"ok","target":"" }]),
        json!([{ "tick":0,"name":"ok","unexpected":1 }]),
        json!([{ "tick":1,"name":"ok" },{ "tick":0,"name":"ok" }]),
        json!([{ "tick":0,"name":"ok","payload":"x".repeat(65536) }]),
        json!(
            (0..1025)
                .map(|_| json!({"tick":0,"name":"ok"}))
                .collect::<Vec<_>>()
        ),
        json!(
            (0..4097)
                .map(|tick| json!({"tick":tick,"name":"ok"}))
                .collect::<Vec<_>>()
        ),
        json!(
            (0..2048)
                .map(|tick| json!({"tick":tick,"name":"ok","payload":"x".repeat(1024)}))
                .collect::<Vec<_>>()
        ),
    ] {
        assert!(EventSchedule::from_json(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
    assert!(
        EventSchedule::from_json(&vec![b' '; scheduled_events::MAX_SCHEDULE_BYTES + 1]).is_err()
    );
    let schedule = make_schedule(json!([{ "tick":2,"name":"ok" }]));
    assert!(schedule.validate_run(2).is_err());
    schedule.validate_run(3).unwrap();
    assert_eq!(schedule.at_tick(2).len(), 1);
    assert!(schedule.at_tick(3).is_empty());
}

#[test]
fn delivered_trace_stops_allocating_at_byte_and_count_caps_but_counts_all_events() {
    let (_dir, mut runtime) = fixture();
    let mut trace = EventTrace::default();
    for tick in 0..20 {
        emit_event(
            runtime.app_mut().world_mut(),
            EngineEvent {
                name: "large".into(),
                source: None,
                target: None,
                payload: json!("x".repeat(60_000)),
            },
        )
        .unwrap();
        runtime.tick(1.0 / 60.0, Default::default()).unwrap();
        trace.capture(&runtime, tick);
    }
    assert_eq!(trace.count, 20);
    assert!(trace.truncated);
    assert!(trace.events.len() < 20);
    let retained = trace.events.len();
    trace.capture(&runtime, 21);
    assert_eq!(trace.count, 21);
    assert_eq!(trace.events.len(), retained);
    let mut trace = EventTrace::default();
    emit_event(
        runtime.app_mut().world_mut(),
        EngineEvent {
            name: "small".into(),
            source: None,
            target: None,
            payload: json!(null),
        },
    )
    .unwrap();
    runtime.tick(1.0 / 60.0, Default::default()).unwrap();
    for tick in 0..4097 {
        trace.capture(&runtime, tick);
    }
    assert_eq!(trace.events.len(), 4096);
    assert_eq!(trace.count, 4097);
    assert!(trace.truncated);
    trace.count = u64::MAX;
    trace.capture(&runtime, 4098);
    assert_eq!(trace.count, u64::MAX);
}

#[test]
fn trace_retains_pre_transition_delivery_without_repeating_it_on_an_empty_tick() {
    let (dir, runtime) = fixture();
    drop(runtime);
    let mut scene: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("scene.json")).unwrap()).unwrap();
    scene["scene_id"] = json!("next");
    std::fs::write(
        dir.path().join("next.json"),
        serde_json::to_vec(&scene).unwrap(),
    )
    .unwrap();
    let mut project: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("project.json")).unwrap()).unwrap();
    project["assets"] = json!([{"id":"next","kind":"scene","path":"next.json"}]);
    std::fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(&project).unwrap(),
    )
    .unwrap();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    register_event_handler(runtime.app_mut(), "increment", |world, entity, event| {
        world.get_mut::<Transform>(entity).unwrap().position[0] +=
            event.payload["amount"].as_f64().unwrap() as f32;
        Ok(())
    })
    .unwrap();
    runtime.start().unwrap();
    let schedule = make_schedule(json!([
        {"tick":0,"name":"game.increment","target":"player","payload":{"amount":3}},
        {"tick":2,"name":"game.increment","target":"player","payload":{"amount":7}}
    ]));
    runtime.request_scene("next", "", "").unwrap();
    runtime.enqueue_scheduled_events(&schedule, 0).unwrap();
    runtime.tick(1.0 / 60.0, Default::default()).unwrap();
    let mut trace = EventTrace::default();
    trace.capture(&runtime, 0);
    assert_eq!(runtime.active_scene().id, "next");
    assert!(runtime.try_snapshot().unwrap().events.is_empty());
    assert_eq!(trace.count, 1);
    assert_eq!(trace.events[0].payload["amount"], 3);
    runtime.tick(1.0 / 60.0, Default::default()).unwrap();
    trace.capture(&runtime, 1);
    assert_eq!(
        trace.count, 1,
        "empty dispatch must replace the previous record"
    );
    runtime.enqueue_scheduled_events(&schedule, 2).unwrap();
    runtime.tick(1.0 / 60.0, Default::default()).unwrap();
    trace.capture(&runtime, 2);
    assert_eq!(
        runtime.position("player").unwrap()[0],
        7.0,
        "stable IDs resolve to the replacement scene entity"
    );
    assert_eq!(trace.count, 2);
}
