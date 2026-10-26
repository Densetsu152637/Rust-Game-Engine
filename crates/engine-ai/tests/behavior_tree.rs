mod common;

use common::{B, TestHost, finished, instance, run};
use engine_ai::{
    BbValue,
    behavior_tree::{AbortReason, AbortRecord, MoveStatus, Status, TreeInstance},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

const S: Option<Status> = Some(Status::Success);
const F: Option<Status> = Some(Status::Failure);
const R: Option<Status> = None;

/// root -> composite -> native leaves n0..; one tick; returns (finished, host log).
fn composite(kind: &str, params: Value, names: &[&str]) -> (Option<Status>, Vec<String>) {
    let mut b = B::new().node("c", kind, params).link("root", "c");
    for (i, name) in names.iter().enumerate() {
        let id = format!("n{i}");
        b = b.leaf(&id, name).link("c", &id);
    }
    let tree = b.compile();
    let (mut instance, mut host) = (instance(&tree), TestHost::new());
    let reports = run(&mut instance, &mut host, 1, 0.1);
    (reports[0].finished, host.log)
}

fn log(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn sequence_table() {
    let cases: [(&[&str], Option<Status>, &[&str]); 4] = [
        (&["ok", "ok"], S, &["start:n0", "start:n1"]),
        (&["ok", "fail", "ok"], F, &["start:n0", "start:n1"]),
        (&["ok", "run", "ok"], R, &["start:n0", "start:n1"]),
        (&["run", "ok"], R, &["start:n0"]),
    ];
    for (names, status, expected) in cases {
        assert_eq!(
            composite("sequence", json!({}), names),
            (status, log(expected)),
            "{names:?}"
        );
    }
}

#[test]
fn selector_table() {
    let cases: [(&[&str], Option<Status>, &[&str]); 4] = [
        (&["fail", "ok", "ok"], S, &["start:n0", "start:n1"]),
        (&["fail", "fail"], F, &["start:n0", "start:n1"]),
        (&["run", "ok"], R, &["start:n0"]),
        (&["ok", "fail"], S, &["start:n0"]),
    ];
    for (names, status, expected) in cases {
        assert_eq!(
            composite("selector", json!({}), names),
            (status, log(expected)),
            "{names:?}"
        );
    }
}

#[test]
fn parallel_policy_table() {
    // (success_policy, failure_policy, children, finished, log)
    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, &str, Vec<&str>, Option<Status>, Vec<&str>)> = vec![
        (
            "require_all",
            "require_one",
            vec!["ok", "ok"],
            S,
            vec!["start:n0", "start:n1"],
        ),
        (
            "require_all",
            "require_one",
            vec!["ok", "fail"],
            F,
            vec!["start:n0", "start:n1"],
        ),
        (
            "require_all",
            "require_one",
            vec!["ok", "run"],
            R,
            vec!["start:n0", "start:n1"],
        ),
        (
            "require_one",
            "require_one",
            vec!["run", "ok"],
            S,
            vec!["start:n0", "start:n1", "abort:n0"],
        ),
        (
            "require_all",
            "require_one",
            vec!["run", "fail"],
            F,
            vec!["start:n0", "start:n1", "abort:n0"],
        ),
        (
            "require_all",
            "require_all",
            vec!["fail", "run"],
            R,
            vec!["start:n0", "start:n1"],
        ),
        (
            "require_all",
            "require_all",
            vec!["fail", "fail"],
            F,
            vec!["start:n0", "start:n1"],
        ),
        (
            "require_all",
            "require_all",
            vec!["ok", "fail"],
            F,
            vec!["start:n0", "start:n1"],
        ),
        (
            "require_one",
            "require_all",
            vec!["fail", "ok"],
            S,
            vec!["start:n0", "start:n1"],
        ),
    ];
    for (success, failure, names, status, expected) in cases {
        let params = json!({"success_policy": success, "failure_policy": failure});
        assert_eq!(
            composite("parallel", params, &names),
            (status, log(&expected)),
            "{success}/{failure} {names:?}"
        );
    }
}

#[test]
fn parallel_does_not_restart_finished_children() {
    let tree = B::new()
        .node("p", "parallel", json!({}))
        .leaf("a", "ok")
        .leaf("b", "run_ok")
        .link("root", "p")
        .link("p", "a")
        .link("p", "b")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    let reports = run(&mut i, &mut host, 2, 0.1);
    assert_eq!(finished(&reports), vec![R, S]);
    assert_eq!(
        host.log,
        log(&["start:a", "start:b", "tick:b"]),
        "a ran once"
    );
}

fn decorated(
    kind: &str,
    params: Value,
    leaf: &str,
) -> std::sync::Arc<engine_ai::behavior_tree::BehaviorTree> {
    B::new()
        .node("d", kind, params)
        .leaf("n", leaf)
        .link("root", "d")
        .link("d", "n")
        .compile()
}

fn statuses(
    kind: &str,
    params: Value,
    leaf: &str,
    ticks: usize,
    dt: f32,
) -> (Vec<Option<Status>>, TestHost) {
    let tree = decorated(kind, params, leaf);
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    let reports = run(&mut i, &mut host, ticks, dt);
    (finished(&reports), host)
}

#[test]
fn inverter_and_force_decorators() {
    for (kind, leaf, expected) in [
        ("inverter", "ok", F),
        ("inverter", "fail", S),
        ("inverter", "run", R),
        ("force_success", "fail", S),
        ("force_success", "ok", S),
        ("force_success", "run", R),
        ("force_failure", "ok", F),
        ("force_failure", "fail", F),
        ("force_failure", "run", R),
    ] {
        assert_eq!(
            statuses(kind, json!({}), leaf, 1, 0.1).0,
            vec![expected],
            "{kind}({leaf})"
        );
    }
}

#[test]
fn repeat_runs_its_child_count_times_one_per_tick() {
    let (done, host) = statuses("repeat", json!({"count": 3}), "ok", 5, 0.1);
    assert_eq!(
        done,
        vec![R, R, S, R, R],
        "completes on the third tick, then the root restarts"
    );
    assert_eq!(host.filter("start:").len(), 5);
    let (done, _) = statuses("repeat", json!({"count": 3}), "fail", 1, 0.1);
    assert_eq!(done, vec![F], "stop_on_failure defaults to true");
    let (done, _) = statuses(
        "repeat",
        json!({"count": 2, "stop_on_failure": false}),
        "fail",
        2,
        0.1,
    );
    assert_eq!(done, vec![R, S]);
}

#[test]
fn retry_reruns_until_success_or_attempts_run_out() {
    let (done, _) = statuses("retry", json!({"attempts": 3}), "fail", 3, 0.1);
    assert_eq!(done, vec![R, R, F]);
    let (done, host) = statuses("retry", json!({"attempts": 5}), "flaky", 3, 0.1);
    assert_eq!(
        done,
        vec![R, R, S],
        "fails twice, succeeds on the third run"
    );
    assert_eq!(host.filter("start:").len(), 3);
}

#[test]
fn cooldown_blocks_restarts_for_its_duration() {
    let (done, host) = statuses("cooldown", json!({"duration": 1.0}), "ok", 6, 0.25);
    assert_eq!(done, vec![S, F, F, F, S, F]);
    assert_eq!(host.filter("start:").len(), 2);
    let (done, _) = statuses(
        "cooldown",
        json!({"duration": 1.0, "lock_on": "failure"}),
        "ok",
        4,
        0.25,
    );
    assert_eq!(
        done,
        vec![S, S, S, S],
        "a success does not lock when lock_on is failure"
    );
    let (done, _) = statuses(
        "cooldown",
        json!({"duration": 1.0, "lock_on": "failure"}),
        "fail",
        3,
        0.25,
    );
    assert_eq!(done, vec![F, F, F]);
}

#[test]
fn time_limit_aborts_a_running_child() {
    let (done, host) = statuses("time_limit", json!({"seconds": 0.5}), "run", 7, 0.1);
    assert_eq!(done, vec![R, R, R, R, R, F, R]);
    assert_eq!(
        host.log,
        log(&[
            "start:n", "tick:n", "tick:n", "tick:n", "tick:n", "abort:n", "start:n"
        ])
    );
}

#[test]
fn wait_succeeds_after_its_duration() {
    let tree = B::new()
        .node("w", "wait", json!({"seconds": 0.5}))
        .link("root", "w")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    assert_eq!(
        finished(&run(&mut i, &mut host, 6, 0.1)),
        vec![R, R, R, R, S, R]
    );
    let tree = B::new()
        .node("w", "wait", json!({"seconds": 0.0}))
        .link("root", "w")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    assert_eq!(finished(&run(&mut i, &mut host, 1, 0.1)), vec![S]);
}

#[test]
fn set_and_clear_blackboard_tasks_write_typed_values() {
    let tree = B::new()
        .var("hp", "int", json!(10))
        .var("speed", "float", Value::Null)
        .node("s", "sequence", json!({}))
        .node("a", "set_blackboard", json!({"key": "hp", "value": 3}))
        .node("b", "set_blackboard", json!({"key": "speed", "value": 2}))
        .node("c", "clear_blackboard", json!({"key": "hp"}))
        .link("root", "s")
        .link("s", "a")
        .link("s", "b")
        .link("s", "c")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    assert_eq!(i.blackboard().get("hp"), Some(&BbValue::Int(10)));
    assert_eq!(finished(&run(&mut i, &mut host, 1, 0.1)), vec![S]);
    assert_eq!(i.blackboard().get("hp"), None);
    assert_eq!(
        i.blackboard().get("speed"),
        Some(&BbValue::Float(2.0)),
        "int literal converts to float"
    );
}

/// root -> selector [ cond(key, op, value, abort) -> chase, patrol ]
fn guarded_selector(
    var: (&str, &str),
    condition: Value,
) -> Arc<engine_ai::behavior_tree::BehaviorTree> {
    B::new()
        .var(var.0, var.1, Value::Null)
        .node("sel", "selector", json!({}))
        .node("c1", "blackboard_condition", condition)
        .leaf("chase", "run")
        .leaf("patrol", "run")
        .link("root", "sel")
        .link("sel", "c1")
        .link("c1", "chase")
        .link("sel", "patrol")
        .compile()
}

#[test]
fn lower_priority_abort_preempts_in_the_tick_the_key_changes() {
    let tree = guarded_selector(
        ("alarm", "bool"),
        json!({"key":"alarm","operator":"eq","value":true,"abort":"lower_priority"}),
    );
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 2, 0.1);
    assert_eq!(host.log, log(&["start:patrol", "tick:patrol"]));
    i.blackboard_mut()
        .set("alarm", BbValue::Bool(true))
        .unwrap();
    let report = run(&mut i, &mut host, 1, 0.1).remove(0);
    assert_eq!(
        host.log,
        log(&["start:patrol", "tick:patrol", "abort:patrol", "start:chase"]),
        "the lower branch is aborted and the higher one starts in the same tick"
    );
    assert_eq!(
        report.aborts,
        vec![AbortRecord {
            node: "patrol".into(),
            reason: AbortReason::ObserverLowerPriority,
            key: Some("alarm".into())
        }]
    );
    // Turning the condition off again does not abort a lower_priority-only observer.
    i.blackboard_mut()
        .set("alarm", BbValue::Bool(false))
        .unwrap();
    let report = run(&mut i, &mut host, 1, 0.1).remove(0);
    assert!(report.aborts.is_empty());
    assert_eq!(host.log.last().unwrap(), "tick:chase");
}

#[test]
fn lower_priority_only_fires_when_the_condition_flips() {
    let tree = guarded_selector(
        ("n", "int"),
        json!({"key":"n","operator":"gt","value":0,"abort":"lower_priority"}),
    );
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 1, 0.1);
    i.blackboard_mut().set("n", BbValue::Int(1)).unwrap();
    assert_eq!(run(&mut i, &mut host, 1, 0.1)[0].aborts.len(), 1);
    i.blackboard_mut().set("n", BbValue::Int(2)).unwrap();
    assert!(
        run(&mut i, &mut host, 1, 0.1)[0].aborts.is_empty(),
        "still true: no flip"
    );
}

#[test]
fn self_abort_stops_the_child_when_the_condition_turns_false() {
    let tree = B::new()
        .var("go", "bool", json!(true))
        .node(
            "c",
            "blackboard_condition",
            json!({"key":"go","operator":"eq","value":true,"abort":"self"}),
        )
        .leaf("n", "run")
        .link("root", "c")
        .link("c", "n")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    assert_eq!(finished(&run(&mut i, &mut host, 2, 0.1)), vec![R, R]);
    i.blackboard_mut().set("go", BbValue::Bool(false)).unwrap();
    let reports = run(&mut i, &mut host, 1, 0.1);
    assert_eq!(finished(&reports), vec![F]);
    assert_eq!(reports[0].aborts[0].reason, AbortReason::ObserverSelf);
    assert_eq!(host.log, log(&["start:n", "tick:n", "abort:n"]));
    assert_eq!(
        finished(&run(&mut i, &mut host, 1, 0.1)),
        vec![F],
        "the condition now fails at once"
    );
    assert_eq!(host.filter("start:").len(), 1, "the child never restarts");
}

#[test]
fn abort_none_ignores_key_changes_while_running() {
    let tree = B::new()
        .var("go", "bool", json!(true))
        .node(
            "c",
            "blackboard_condition",
            json!({"key":"go","operator":"eq","value":true}),
        )
        .leaf("n", "run")
        .link("root", "c")
        .link("c", "n")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 1, 0.1);
    i.blackboard_mut().set("go", BbValue::Bool(false)).unwrap();
    let reports = run(&mut i, &mut host, 2, 0.1);
    assert_eq!(finished(&reports), vec![R, R]);
    assert!(host.filter("abort:").is_empty());
}

#[test]
fn both_mode_preempts_down_and_aborts_itself() {
    let tree = guarded_selector(
        ("alarm", "bool"),
        json!({"key":"alarm","operator":"eq","value":true,"abort":"both"}),
    );
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 1, 0.1);
    i.blackboard_mut()
        .set("alarm", BbValue::Bool(true))
        .unwrap();
    run(&mut i, &mut host, 1, 0.1);
    i.blackboard_mut()
        .set("alarm", BbValue::Bool(false))
        .unwrap();
    let report = run(&mut i, &mut host, 1, 0.1).remove(0);
    assert_eq!(report.aborts[0].reason, AbortReason::ObserverSelf);
    assert_eq!(
        host.log,
        log(&[
            "start:patrol",
            "abort:patrol",
            "start:chase",
            "abort:chase",
            "start:patrol"
        ]),
        "falls back to the next branch in the same tick"
    );
}

#[test]
fn a_change_made_during_the_walk_preempts_in_that_same_tick() {
    let tree = B::new()
        .var("alarm", "bool", Value::Null)
        .node("p", "parallel", json!({}))
        .node("sel", "selector", json!({}))
        .node(
            "c1",
            "blackboard_condition",
            json!({"key":"alarm","operator":"eq","value":true,"abort":"lower_priority"}),
        )
        .leaf("chase", "run")
        .leaf("patrol", "run")
        .node("seq", "sequence", json!({}))
        .node("w", "wait", json!({"seconds": 0.15}))
        .leaf("raise", "flag_on")
        .link("root", "p")
        .link("p", "sel")
        .link("sel", "c1")
        .link("c1", "chase")
        .link("sel", "patrol")
        .link("p", "seq")
        .link("seq", "w")
        .link("seq", "raise")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    let reports = run(&mut i, &mut host, 2, 0.1);
    assert_eq!(reports[0].passes, 1);
    assert_eq!(
        reports[1].passes, 2,
        "the preemption triggers a second pass"
    );
    assert_eq!(
        host.log,
        log(&[
            "start:patrol",
            "tick:patrol",
            "start:raise",
            "abort:patrol",
            "start:chase"
        ])
    );
}

#[test]
fn services_run_at_their_interval() {
    let build = |interval: f64| {
        B::new()
            .node("seq", "sequence", json!({}))
            .node("w", "wait", json!({"seconds": 1.0}))
            .service(
                "seq",
                "svc",
                "emit_event_service",
                json!({"event":"svc.tick","interval":interval}),
            )
            .link("root", "seq")
            .link("seq", "w")
            .compile()
    };
    let fire_ticks = |interval: f64, ticks: usize, dt: f32| -> Vec<u64> {
        let tree = build(interval);
        let (mut i, mut host) = (instance(&tree), TestHost::new());
        run(&mut i, &mut host, ticks, dt);
        host.events.iter().map(|(tick, ..)| *tick).collect()
    };
    let sixty: Vec<u64> = (1..=55).step_by(6).collect();
    assert_eq!(
        fire_ticks(0.1, 60, 1.0 / 60.0),
        sixty,
        "dt 1/60, interval 0.1: every sixth tick"
    );
    assert_eq!(
        fire_ticks(0.1, 6, 0.1),
        vec![1, 2, 3, 4, 5, 6],
        "dt 0.1, interval 0.1: every tick"
    );
    assert_eq!(
        fire_ticks(0.25, 14, 0.1),
        vec![1, 4, 6, 9, 11, 14],
        "no catch-up beyond one run per tick"
    );
    assert_eq!(
        fire_ticks(0.0, 4, 0.1),
        vec![1, 2, 3, 4],
        "interval 0 runs every tick"
    );
}

#[test]
fn a_native_service_starts_ticks_and_aborts_with_its_branch() {
    let tree = B::new()
        .node("seq", "sequence", json!({}))
        .node("w", "wait", json!({"seconds": 0.25}))
        .service(
            "seq",
            "svc1",
            "native_service",
            json!({"name":"svc","interval":0.1}),
        )
        .link("root", "seq")
        .link("seq", "w")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    let reports = run(&mut i, &mut host, 3, 0.1);
    assert_eq!(finished(&reports), vec![R, R, S]);
    assert_eq!(
        host.log,
        log(&["start:svc1", "tick:svc1", "tick:svc1", "abort:svc1"])
    );
}

#[test]
fn a_service_can_feed_an_observer() {
    // The service raises the key on activation; the condition then holds at once.
    let tree = B::new()
        .var("alarm", "bool", Value::Null)
        .node("sel", "selector", json!({}))
        .node(
            "c1",
            "blackboard_condition",
            json!({"key":"alarm","operator":"is_set"}),
        )
        .leaf("chase", "run")
        .leaf("patrol", "run")
        .service(
            "sel",
            "raise",
            "set_blackboard_service",
            json!({"key":"alarm","value":true,"interval":5.0}),
        )
        .link("root", "sel")
        .link("sel", "c1")
        .link("c1", "chase")
        .link("sel", "patrol")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 1, 0.1);
    assert_eq!(host.log, log(&["start:chase"]));
}

fn random_orders(authored_seed: u64, agent_seed: u64, activations: usize) -> Vec<Vec<String>> {
    let mut b = B::new()
        .node("rs", "random_selector", json!({"seed": authored_seed}))
        .link("root", "rs");
    for i in 0..5 {
        let id = format!("n{i}");
        b = b.leaf(&id, "fail").link("rs", &id);
    }
    let tree = b.compile();
    let mut i = TreeInstance::new(tree, &BTreeMap::new(), agent_seed).unwrap();
    let mut host = TestHost::new();
    run(&mut i, &mut host, activations, 0.1);
    host.filter("start:")
        .chunks(5)
        .map(<[String]>::to_vec)
        .collect()
}

#[test]
fn random_selector_is_seeded_and_reproducible() {
    let a = random_orders(1, 0, 6);
    assert_eq!(a, random_orders(1, 0, 6), "same seed, same orders");
    assert_ne!(
        a,
        random_orders(2, 0, 6),
        "a different authored seed diverges"
    );
    assert_ne!(
        a,
        random_orders(1, 99, 6),
        "a different agent seed diverges"
    );
    for order in &a {
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            ["n0", "n1", "n2", "n3", "n4"],
            "every child is tried once: {order:?}"
        );
    }
    assert!(
        a.iter().any(|o| o != &a[0]),
        "successive activations draw new orders"
    );
}

#[test]
fn native_tasks_start_tick_and_complete_in_order() {
    let tree = B::new()
        .node("s", "sequence", json!({}))
        .leaf("n1", "run3_ok")
        .leaf("n2", "ok")
        .link("root", "s")
        .link("s", "n1")
        .link("s", "n2")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    let reports = run(&mut i, &mut host, 4, 0.1);
    assert_eq!(finished(&reports), vec![R, R, R, S]);
    assert_eq!(
        host.log,
        log(&["start:n1", "tick:n1", "tick:n1", "tick:n1", "start:n2"])
    );
}

#[test]
fn move_to_runs_until_the_host_reports_arrival() {
    let tree = B::new()
        .node("s", "sequence", json!({}))
        .node("m", "move_to", json!({"target": [1.0, 0.0, 2.0]}))
        .node(
            "e",
            "emit_event",
            json!({"event": "arrived.event", "payload": {"at": "a"}}),
        )
        .link("root", "s")
        .link("s", "m")
        .link("s", "e")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    assert_eq!(finished(&run(&mut i, &mut host, 2, 0.1)), vec![R, R]);
    assert_eq!(
        host.moves,
        vec![[1.0, 0.0, 2.0]],
        "one request however long it takes"
    );
    host.status = MoveStatus::Arrived;
    assert_eq!(finished(&run(&mut i, &mut host, 1, 0.1)), vec![S]);
    assert_eq!(host.events[0].1, "arrived.event");
    assert_eq!(host.events[0].2, json!({"at": "a"}));
}

#[test]
fn move_to_failure_timeout_and_key_targets() {
    let tree = B::new()
        .node("m", "move_to", json!({"target": [0.0, 0.0, 1.0]}))
        .link("root", "m")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 1, 0.1);
    host.status = MoveStatus::Failed;
    assert_eq!(finished(&run(&mut i, &mut host, 1, 0.1)), vec![F]);
    assert_eq!(host.cancels, 0, "a failed move needs no cancel");

    let tree = B::new()
        .node(
            "m",
            "move_to",
            json!({"target": [0.0, 0.0, 1.0], "timeout": 0.25}),
        )
        .link("root", "m")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    assert_eq!(finished(&run(&mut i, &mut host, 3, 0.1)), vec![R, R, F]);
    assert_eq!(host.cancels, 1);

    let tree = B::new()
        .var("dest", "vec3", json!([4.0, 0.0, 5.0]))
        .var("unset", "vec3", Value::Null)
        .node("s", "selector", json!({}))
        .node("m", "move_to", json!({"key": "dest"}))
        .link("root", "s")
        .link("s", "m")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 1, 0.1);
    assert_eq!(host.moves, vec![[4.0, 0.0, 5.0]]);
    let tree = B::new()
        .var("dest", "vec3", Value::Null)
        .node("m", "move_to", json!({"key": "dest"}))
        .link("root", "m")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    assert_eq!(
        finished(&run(&mut i, &mut host, 1, 0.1)),
        vec![F],
        "an unset key cannot be a target"
    );
    assert!(host.moves.is_empty());
}

#[test]
fn preempting_a_move_cancels_it() {
    let tree = B::new()
        .var("alarm", "bool", Value::Null)
        .node("sel", "selector", json!({}))
        .node(
            "c1",
            "blackboard_condition",
            json!({"key":"alarm","operator":"eq","value":true,"abort":"lower_priority"}),
        )
        .leaf("chase", "run")
        .node("m", "move_to", json!({"target": [9.0, 0.0, 9.0]}))
        .link("root", "sel")
        .link("sel", "c1")
        .link("c1", "chase")
        .link("sel", "m")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 1, 0.1);
    i.blackboard_mut()
        .set("alarm", BbValue::Bool(true))
        .unwrap();
    run(&mut i, &mut host, 1, 0.1);
    assert_eq!(host.cancels, 1);
    assert_eq!(
        host.log,
        log(&["move:[9.0, 0.0, 9.0]", "cancel_move", "start:chase"])
    );
}

#[test]
fn stop_aborts_everything_running() {
    let tree = B::new()
        .node("s", "sequence", json!({}))
        .leaf("a", "run")
        .link("root", "s")
        .link("s", "a")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 1, 0.1);
    let aborts = i.stop(&mut host);
    assert_eq!(aborts.len(), 1);
    assert_eq!(aborts[0].reason, AbortReason::TreeStopped);
    assert_eq!(aborts[0].node, "root");
    assert_eq!(host.log, log(&["start:a", "abort:a"]));
    assert!(i.debug().active_path.is_empty());
}

#[test]
fn debug_state_reports_path_status_counters_and_a_bounded_log() {
    let tree = B::new()
        .node("s", "sequence", json!({}))
        .leaf("a", "ok")
        .leaf("b", "run")
        .link("root", "s")
        .link("s", "a")
        .link("s", "b")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 2, 0.1);
    let debug = i.debug();
    assert_eq!(debug.ticks, 2);
    assert!((debug.time - 0.2).abs() < 1e-6);
    assert_eq!(debug.active_path, ["root", "s", "b"]);
    let by_id: BTreeMap<_, _> = debug.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    assert_eq!(by_id["a"].last, S);
    assert!(!by_id["a"].running && by_id["b"].running);
    assert_eq!(by_id["b"].last, None);
    assert!(debug.log.iter().any(|l| l.node == "a"));
    serde_json::to_value(&debug).expect("debug view serializes");

    let tree = B::new()
        .node("i", "inverter", json!({}))
        .leaf("a", "ok")
        .link("root", "i")
        .link("i", "a")
        .compile();
    let (mut i, mut host) = (instance(&tree), TestHost::new());
    run(&mut i, &mut host, 200, 0.1);
    let debug = i.debug();
    assert_eq!(
        debug.log.len(),
        engine_ai::behavior_tree::MAX_LOG,
        "the log is a bounded ring"
    );
    assert_eq!(debug.ticks, 200);
    assert_eq!(debug.last_finished, F);
}

#[test]
fn overrides_seed_the_blackboard_and_bad_ones_are_rejected() {
    let tree = B::new()
        .var("hp", "int", json!(5))
        .node("w", "wait", json!({"seconds": 1.0}))
        .link("root", "w")
        .compile();
    let overrides: BTreeMap<String, Value> = [("hp".to_owned(), json!(9))].into();
    let i = TreeInstance::new(Arc::clone(&tree), &overrides, 0).unwrap();
    assert_eq!(i.blackboard().get("hp"), Some(&BbValue::Int(9)));
    for bad in [("hp", json!("x")), ("nope", json!(1))] {
        let overrides: BTreeMap<String, Value> = [(bad.0.to_owned(), bad.1)].into();
        assert!(TreeInstance::new(Arc::clone(&tree), &overrides, 0).is_err());
    }
}

/// Two instances fed identical inputs produce identical transcripts and debug views.
#[test]
fn execution_is_deterministic() {
    let build = || {
        B::new()
            .var("alarm", "bool", Value::Null)
            .node("sel", "selector", json!({}))
            .node(
                "c1",
                "blackboard_condition",
                json!({"key":"alarm","operator":"eq","value":true,"abort":"both"}),
            )
            .node("rs", "random_selector", json!({"seed": 7}))
            .leaf("chase", "run_ok")
            .leaf("r0", "run_fail")
            .leaf("r1", "fail")
            .leaf("r2", "run_ok")
            .service(
                "sel",
                "ping",
                "emit_event_service",
                json!({"event":"ping","interval":0.3}),
            )
            .link("root", "sel")
            .link("sel", "c1")
            .link("c1", "chase")
            .link("sel", "rs")
            .link("rs", "r0")
            .link("rs", "r1")
            .link("rs", "r2")
            .compile()
    };
    let transcript = || {
        let tree = build();
        let (mut i, mut host) = (instance(&tree), TestHost::new());
        let mut reports = Vec::new();
        for tick in 0..60 {
            if tick % 9 == 4 {
                let on = (tick / 9) % 2 == 0;
                i.blackboard_mut().set("alarm", BbValue::Bool(on)).unwrap();
            }
            reports.extend(run(&mut i, &mut host, 1, 0.1));
        }
        (
            host.log,
            host.events,
            serde_json::to_string(&i.debug()).unwrap(),
            reports
                .iter()
                .map(|r| (r.finished, r.passes, r.aborts.len()))
                .collect::<Vec<_>>(),
        )
    };
    let first = transcript();
    assert!(first.0.len() > 20, "the scenario exercises the tree");
    assert_eq!(first, transcript());
}
