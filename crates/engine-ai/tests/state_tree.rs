//! Executor behavior: selection, triggers, priority, delay, order, events,
//! evaluators and tree completion.
mod st_common;

use engine_ai::{
    BbValue,
    behavior_tree::{MoveStatus, Status},
    state_tree::{
        LogEvent, MAX_LOG, MAX_PENDING_EVENTS, MAX_TRANSITIONS_PER_TICK, Phase, StateTree,
    },
};
use serde_json::{Value, json};
use st_common::{Rig, S};
use std::sync::Arc;

// ---------------------------------------------------------------- selection

fn selection_tree() -> Arc<StateTree> {
    S::new()
        .var("flag", "bool", Value::Null)
        .var("mode", "int", Value::Null)
        .state("a", "root", json!({}))
        .cond("a", "a_c", "flag", "eq", json!(true))
        .state("b", "root", json!({}))
        .state("b1", "b", json!({}))
        .cond("b1", "b1_c", "mode", "eq", json!(1))
        .state("b2", "b", json!({}))
        .cond("b2", "b2_c", "mode", "eq", json!(2))
        .state("c", "root", json!({}))
        .compile()
}

#[test]
fn selection_tries_children_in_order_with_enter_conditions_and_falls_back() {
    // (flag, mode, expected active path)
    let cases: [(Option<bool>, Option<i64>, &str); 6] = [
        (Some(true), None, "a"),
        (Some(true), Some(1), "a"),
        (Some(false), Some(1), "b/b1"),
        (None, Some(2), "b/b2"),
        // b has children but none selectable: b itself is not selectable, c is.
        (Some(false), Some(3), "c"),
        (None, None, "c"),
    ];
    let tree = selection_tree();
    for (flag, mode, expected) in cases {
        let mut rig = Rig::new(&tree);
        if let Some(flag) = flag {
            rig.set("flag", BbValue::Bool(flag));
        }
        if let Some(mode) = mode {
            rig.set("mode", BbValue::Int(mode));
        }
        let report = rig.tick(0.1);
        assert_eq!(rig.path(), expected, "flag {flag:?} mode {mode:?}");
        let entered = expected.split('/').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(report.entered, entered, "enter order is parent first");
        assert!(report.exited.is_empty());
        assert_eq!(rig.instance.phase(), Phase::Running);
    }
}

#[test]
fn selection_modes_children_or_self_and_self() {
    let build = |selection: &str| {
        S::new()
            .var("flag", "bool", json!(false))
            .state("p", "root", json!({"selection": selection}))
            // With `self` selection the child is only reachable through a transition.
            .go(
                "p",
                "t",
                json!({"trigger":"on_event","event":"x","target":"state","state":"x"}),
            )
            .state("x", "p", json!({}))
            .cond("x", "x_c", "flag", "eq", json!(true))
            .compile()
    };
    // (selection, flag, expected path)
    let cases = [
        ("children_or_self", false, "p"),
        ("children_or_self", true, "p/x"),
        ("self", false, "p"),
        ("self", true, "p"),
    ];
    for (selection, flag, expected) in cases {
        let mut rig = Rig::new(&build(selection));
        rig.set("flag", BbValue::Bool(flag));
        rig.tick(0.1);
        assert_eq!(rig.path(), expected, "{selection} flag {flag}");
    }
}

#[test]
fn a_tree_with_no_selectable_top_level_state_fails_at_start() {
    let tree = S::new()
        .var("flag", "bool", json!(false))
        .state("a", "root", json!({}))
        .cond("a", "a_c", "flag", "eq", json!(true))
        .compile();
    let mut rig = Rig::new(&tree);
    let report = rig.tick(0.1);
    assert_eq!(report.finished, Some(Status::Failure));
    assert_eq!(rig.instance.phase(), Phase::Finished(Status::Failure));
    assert!(rig.instance.active_path().is_empty());
    // A finished tree ignores further ticks.
    let again = rig.tick(0.1);
    assert!(again.finished.is_none() && again.entered.is_empty());
    assert_eq!(rig.instance.ticks(), 1);
}

#[test]
fn a_transition_into_a_state_whose_enter_conditions_fail_does_not_fire() {
    // Going to a state whose enter condition fails does not fire.
    let tree = S::new()
        .var("open", "bool", json!(false))
        .state("a", "root", json!({}))
        .go(
            "a",
            "t",
            json!({"trigger":"on_tick","target":"state","state":"b"}),
        )
        .state("b", "root", json!({}))
        .cond("b", "b_c", "open", "eq", json!(true))
        .compile();
    let mut rig = Rig::new(&tree);
    rig.ticks(3, 0.1);
    assert_eq!(rig.path(), "a", "blocked while b cannot be entered");
    rig.set("open", BbValue::Bool(true));
    let report = rig.tick(0.1);
    assert_eq!(rig.path(), "b");
    assert_eq!(report.exited, ["a"]);
}

// ----------------------------------------------------------------- triggers

#[test]
fn on_completed_fires_when_the_state_tasks_complete() {
    let tree = S::new()
        .state("s1", "root", json!({}))
        .wait("s1", "w", 0.25)
        .go(
            "s1",
            "t",
            json!({"trigger":"on_completed", "target":"state","state":"s2"}),
        )
        .state("s2", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&tree);
    let paths: Vec<String> = (0..5)
        .map(|_| {
            rig.tick(0.1);
            rig.path()
        })
        .collect();
    // elapsed 0 (start), 0.1, 0.2, 0.3 >= 0.25 on the fourth tick.
    assert_eq!(paths, ["s1", "s1", "s1", "s2", "s2"]);
}

#[test]
fn on_succeeded_and_on_failed_pick_by_the_task_outcome() {
    let tree = |native: &str| {
        S::new()
            .state("s1", "root", json!({}))
            .native("s1", "task", native)
            .go(
                "s1",
                "ts",
                json!({"trigger":"on_succeeded","target":"state","state":"good"}),
            )
            .go(
                "s1",
                "tf",
                json!({"trigger":"on_failed","target":"state","state":"bad"}),
            )
            .state("good", "root", json!({}))
            .state("bad", "root", json!({}))
            .compile()
    };
    // (native behavior, tick it resolves on, expected state, expected trigger)
    let cases = [
        ("run_ok", 2, "good", "on_succeeded"),
        ("run_fail", 2, "bad", "on_failed"),
        ("ok", 1, "good", "on_succeeded"),
        ("fail", 1, "bad", "on_failed"),
    ];
    for (native, at, expected, trigger) in cases {
        let mut rig = Rig::new(&tree(native));
        for tick in 1..=at {
            let report = rig.tick(0.1);
            if tick < at {
                assert_eq!(rig.path(), "s1", "{native} still running at tick {tick}");
            } else {
                assert_eq!(rig.path(), expected, "{native}");
                assert_eq!(report.transitions[0].trigger, trigger);
            }
        }
    }
}

#[test]
fn on_tick_fires_as_soon_as_its_conditions_hold() {
    let tree = S::new()
        .var("go", "bool", json!(false))
        .state("s1", "root", json!({}))
        .go(
            "s1",
            "t",
            json!({"trigger":"on_tick","target":"state","state":"s2"}),
        )
        .cond("t", "t_c", "go", "eq", json!(true))
        .state("s2", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&tree);
    rig.ticks(2, 0.1);
    assert_eq!(rig.path(), "s1");
    rig.set("go", BbValue::Bool(true));
    let report = rig.tick(0.1);
    assert_eq!(rig.path(), "s2");
    assert_eq!(report.transitions.len(), 1);
    assert_eq!(report.transitions[0].from, "s1");
    assert_eq!(report.transitions[0].to.as_deref(), Some("s2"));
}

#[test]
fn on_event_fires_for_its_event_only_and_events_are_consumed() {
    let tree = S::new()
        .state("s1", "root", json!({}))
        .go(
            "s1",
            "t",
            json!({"trigger":"on_event","event":"go","target":"state","state":"s2"}),
        )
        .state("s2", "root", json!({}))
        .go(
            "s2",
            "back",
            json!({"trigger":"on_event","event":"go","target":"state","state":"s1"}),
        )
        .compile();
    let mut rig = Rig::new(&tree);
    rig.tick(0.1);
    assert!(
        !rig.instance.send_event("unrelated"),
        "unlistened events are ignored"
    );
    assert!(rig.instance.send_event("go"));
    rig.tick(0.1);
    // One event drives one transition: s2's own listener does not see it.
    assert_eq!(rig.path(), "s2");
    rig.tick(0.1);
    assert_eq!(rig.path(), "s2", "the event was consumed");
    rig.instance.send_event("go");
    rig.tick(0.1);
    assert_eq!(rig.path(), "s1");
}

#[test]
fn pending_events_are_bounded() {
    let tree = S::new()
        .state("s1", "root", json!({}))
        .go(
            "s1",
            "t",
            json!({"trigger":"on_event","event":"go","target":"state","state":"s2"}),
        )
        .state("s2", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&tree);
    let accepted = (0..MAX_PENDING_EVENTS + 8)
        .filter(|_| rig.instance.send_event("go"))
        .count();
    assert_eq!(accepted, MAX_PENDING_EVENTS);
    assert_eq!(rig.instance.dropped_events(), 8);
    rig.tick(0.1);
    assert!(rig.instance.send_event("go"), "the queue drains every tick");
}

// ----------------------------------------------------------------- priority

fn path_after_one_tick(tree: Arc<StateTree>) -> String {
    let mut rig = Rig::new(&tree);
    rig.tick(0.1);
    // The first tick selects and then runs transitions; follow the chain once.
    rig.path()
}

#[test]
fn transition_priority_orders_candidates_then_depth_then_authoring_order() {
    // Same state, different priorities: the highest wins whatever the order.
    let by_priority = S::new()
        .state("s", "root", json!({}))
        .go(
            "s",
            "low",
            json!({"trigger":"on_tick","target":"state","state":"a","priority":1}),
        )
        .go(
            "s",
            "high",
            json!({"trigger":"on_tick","target":"state","state":"b","priority":5}),
        )
        .go(
            "s",
            "mid",
            json!({"trigger":"on_tick","target":"state","state":"c","priority":3}),
        )
        .state("a", "root", json!({}))
        .state("b", "root", json!({}))
        .state("c", "root", json!({}))
        .compile();
    assert_eq!(path_after_one_tick(by_priority), "b");

    // Equal priority: authoring order.
    let by_order = S::new()
        .state("s", "root", json!({}))
        .go(
            "s",
            "t1",
            json!({"trigger":"on_tick","target":"state","state":"a"}),
        )
        .go(
            "s",
            "t2",
            json!({"trigger":"on_tick","target":"state","state":"b"}),
        )
        .state("a", "root", json!({}))
        .state("b", "root", json!({}))
        .compile();
    assert_eq!(path_after_one_tick(by_order), "a");

    // (parent priority, child priority) -> where the tree goes.
    let nested = |parent_priority: u32, child_priority: u32| {
        S::new()
            .state("p", "root", json!({}))
            .go(
                "p",
                "tp",
                json!({"trigger":"on_tick","target":"state","state":"x","priority":parent_priority}),
            )
            .state("c", "p", json!({}))
            .go(
                "c",
                "tc",
                json!({"trigger":"on_tick","target":"state","state":"y","priority":child_priority}),
            )
            .state("x", "root", json!({}))
            .state("y", "root", json!({}))
            .compile()
    };
    assert_eq!(
        path_after_one_tick(nested(0, 0)),
        "y",
        "deeper state wins ties"
    );
    assert_eq!(
        path_after_one_tick(nested(1, 0)),
        "x",
        "priority beats depth"
    );
    assert_eq!(path_after_one_tick(nested(0, 1)), "y");
}

// -------------------------------------------------------------------- delay

/// Tick on which `s1 -> s2` fires when the trigger first holds at tick 2.
fn delayed_fire_tick(dt: f32, delay: f64) -> u64 {
    let tree = S::new()
        .var("go", "bool", json!(false))
        .state("s1", "root", json!({}))
        .go(
            "s1",
            "t",
            json!({"trigger":"on_tick","target":"state","state":"s2","delay":delay}),
        )
        .cond("t", "t_c", "go", "eq", json!(true))
        .state("s2", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&tree);
    rig.tick(dt);
    rig.set("go", BbValue::Bool(true));
    for _ in 0..400 {
        rig.tick(dt);
        if rig.path() == "s2" {
            return rig.instance.ticks();
        }
    }
    panic!("never fired for dt {dt} delay {delay}");
}

#[test]
fn a_delayed_transition_fires_exactly_when_the_delay_has_elapsed() {
    const ARMED_AT: u64 = 2;
    // (dt, delay, ticks after arming)
    let cases: [(f32, f64, u64); 8] = [
        (1.0 / 60.0, 0.0, 0),
        (1.0 / 60.0, 0.5, 30),
        (1.0 / 60.0, 1.0, 60),
        (1.0 / 60.0, 0.25, 15),
        (0.1, 0.0, 0),
        (0.1, 0.1, 1),
        (0.1, 0.25, 3),
        (0.1, 1.0, 10),
    ];
    for (dt, delay, after) in cases {
        assert_eq!(
            delayed_fire_tick(dt, delay),
            ARMED_AT + after,
            "dt {dt} delay {delay}"
        );
    }
}

#[test]
fn a_delay_restarts_when_the_trigger_stops_holding() {
    let tree = S::new()
        .var("go", "bool", json!(false))
        .state("s1", "root", json!({}))
        .go(
            "s1",
            "t",
            json!({"trigger":"on_tick","target":"state","state":"s2","delay":0.3}),
        )
        .cond("t", "t_c", "go", "eq", json!(true))
        .state("s2", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&tree);
    rig.tick(0.1); // tick 1
    rig.set("go", BbValue::Bool(true));
    rig.ticks(2, 0.1); // ticks 2, 3: armed, 0.1 elapsed
    rig.set("go", BbValue::Bool(false));
    rig.tick(0.1); // tick 4: disarmed
    rig.set("go", BbValue::Bool(true));
    for expected in [5, 6] {
        rig.tick(0.1);
        assert_eq!(rig.path(), "s1", "still waiting at tick {expected}");
    }
    rig.tick(0.1); // tick 7: re-armed at 5, 0.1 at 6, 0.2 at 7 -> not yet
    assert_eq!(rig.path(), "s1");
    rig.tick(0.1); // tick 8: 0.3
    assert_eq!(rig.path(), "s2");
    assert_eq!(rig.instance.ticks(), 8);
}

#[test]
fn a_delayed_event_transition_latches_the_event() {
    let tree = S::new()
        .state("s1", "root", json!({}))
        .go(
            "s1",
            "t",
            json!({"trigger":"on_event","event":"go","target":"state","state":"s2","delay":0.2}),
        )
        .state("s2", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&tree);
    rig.tick(0.1);
    rig.instance.send_event("go");
    rig.tick(0.1); // armed
    rig.tick(0.1); // 0.1
    assert_eq!(rig.path(), "s1");
    rig.tick(0.1); // 0.2, no event needed any more
    assert_eq!(rig.path(), "s2");
}

#[test]
fn delay_variance_is_seeded_and_deterministic() {
    let tree = S::new()
        .state("s1", "root", json!({}))
        .go(
            "s1",
            "t",
            json!({"trigger":"on_tick","target":"state","state":"s2","delay":0.5,"delay_variance":0.4}),
        )
        .state("s2", "root", json!({}))
        .compile();
    let fire_ticks = |seed: u64| -> Vec<u64> {
        (0..4)
            .map(|offset| {
                let mut rig = Rig::seeded(&tree, seed + offset);
                for _ in 0..100 {
                    rig.tick(0.05);
                    if rig.path() == "s2" {
                        return rig.instance.ticks();
                    }
                }
                panic!("never fired");
            })
            .collect()
    };
    let first = fire_ticks(7);
    assert_eq!(first, fire_ticks(7), "same seeds, same timings");
    // Armed on tick 1 with 0.1..0.9 s at 0.05 s ticks.
    assert!(first.iter().all(|t| (3..=19).contains(t)), "{first:?}");
    assert!(
        first.iter().any(|t| *t != first[0]),
        "seeds differ: {first:?}"
    );
}

// -------------------------------------------------------------- enter / exit

fn order_tree() -> Arc<StateTree> {
    S::new()
        .var("flag", "bool", json!(false))
        .state("a", "root", json!({}))
        .native("a", "ta", "run")
        .state("a1", "a", json!({}))
        .native("a1", "ta1", "run")
        .go(
            "a1",
            "to_b1",
            json!({"trigger":"on_event","event":"to_b1","target":"state","state":"b1"}),
        )
        .go(
            "a1",
            "to_a2",
            json!({"trigger":"on_event","event":"to_a2","target":"state","state":"a2"}),
        )
        .go(
            "a1",
            "again",
            json!({"trigger":"on_event","event":"again","target":"state","state":"a1"}),
        )
        .go(
            "a1",
            "up",
            json!({"trigger":"on_event","event":"up","target":"parent"}),
        )
        .go(
            "a1",
            "next",
            json!({"trigger":"on_event","event":"next","target":"next_sibling"}),
        )
        .state("a2", "a", json!({}))
        .cond("a2", "a2_c", "flag", "eq", json!(true))
        .native("a2", "ta2", "run")
        .state("a3", "a", json!({}))
        .native("a3", "ta3", "run")
        .state("b", "root", json!({}))
        .native("b", "tb", "run")
        .state("b1", "b", json!({}))
        .native("b1", "tb1", "run")
        .compile()
}

/// (event, exited, entered, path afterwards, host log of the transition tick)
type OrderCase = (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    &'static str,
    &'static [&'static str],
);

#[test]
fn states_exit_child_to_parent_and_enter_parent_to_child_exactly_once() {
    let cases: [OrderCase; 5] = [
        (
            "to_b1",
            &["a1", "a"],
            &["b", "b1"],
            "b/b1",
            &[
                "tick:ta",
                "tick:ta1",
                "abort:ta1",
                "abort:ta",
                "start:tb",
                "start:tb1",
            ],
        ),
        // Siblings under one parent keep the parent running.
        (
            "next",
            &["a1"],
            &["a3"],
            "a/a3",
            &["tick:ta", "tick:ta1", "abort:ta1", "start:ta3"],
        ),
        // A self transition re-enters the state, not its ancestors.
        (
            "again",
            &["a1"],
            &["a1"],
            "a/a1",
            &["tick:ta", "tick:ta1", "abort:ta1", "start:ta1"],
        ),
        // parent re-enters the parent, which selects its first child again.
        (
            "up",
            &["a1", "a"],
            &["a", "a1"],
            "a/a1",
            &[
                "tick:ta",
                "tick:ta1",
                "abort:ta1",
                "abort:ta",
                "start:ta",
                "start:ta1",
            ],
        ),
        // a2's enter condition fails, so the transition does not fire.
        ("to_a2", &[], &[], "a/a1", &["tick:ta", "tick:ta1"]),
    ];
    for (event, exited, entered, path, log) in cases {
        let mut rig = Rig::new(&order_tree());
        rig.tick(0.1);
        rig.host.log.clear();
        assert!(rig.instance.send_event(event));
        let report = rig.tick(0.1);
        assert_eq!(report.exited, exited, "{event} exits child before parent");
        assert_eq!(
            report.entered, entered,
            "{event} enters parent before child"
        );
        assert_eq!(rig.path(), path, "{event}");
        assert_eq!(rig.host.log, log, "{event}");
        // The debug log records the same events, each once.
        let debug = rig.instance.debug();
        let logged_exits: Vec<_> = debug
            .log
            .iter()
            .filter(|r| r.tick == 2 && r.event == LogEvent::Exited)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(logged_exits, exited, "{event}");
    }
}

#[test]
fn next_sibling_skips_siblings_that_cannot_be_entered() {
    let mut rig = Rig::new(&order_tree());
    rig.tick(0.1);
    rig.instance.send_event("next");
    let report = rig.tick(0.1);
    // a2 needs flag; a3 is the next selectable sibling.
    assert_eq!(rig.path(), "a/a3");
    assert_eq!(report.transitions[0].to.as_deref(), Some("a3"));
    let mut rig = Rig::new(&order_tree());
    rig.set("flag", BbValue::Bool(true));
    rig.tick(0.1);
    rig.instance.send_event("next");
    rig.tick(0.1);
    assert_eq!(rig.path(), "a/a2");
}

#[test]
fn stopping_exits_every_active_state_child_first_and_aborts_tasks() {
    let mut rig = Rig::new(&order_tree());
    rig.tick(0.1);
    rig.host.log.clear();
    let exited = rig.instance.stop(&mut rig.host);
    assert_eq!(exited, ["a1", "a"]);
    assert_eq!(rig.host.log, ["abort:ta1", "abort:ta"]);
    assert_eq!(rig.instance.phase(), Phase::Stopped);
    assert!(
        rig.instance.stop(&mut rig.host).is_empty(),
        "stop is idempotent"
    );
    assert!(
        rig.tick(0.1).entered.is_empty(),
        "a stopped tree stays stopped"
    );
}

#[test]
fn finished_tasks_are_not_aborted_on_exit() {
    let tree = S::new()
        .state("s1", "root", json!({}))
        .native("s1", "t", "run_ok")
        .native("s1", "never", "run")
        .go(
            "s1",
            "go",
            json!({"trigger":"on_succeeded","target":"state","state":"s2"}),
        )
        .state("s2", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&tree);
    rig.ticks(2, 0.1);
    assert_eq!(rig.path(), "s2");
    assert_eq!(
        rig.host.filter("abort:"),
        ["never"],
        "only the running task aborts"
    );
}

#[test]
fn transitions_chain_within_a_tick_but_stay_bounded() {
    let tree = S::new()
        .state("a", "root", json!({}))
        .go(
            "a",
            "ab",
            json!({"trigger":"on_tick","target":"state","state":"b"}),
        )
        .state("b", "root", json!({}))
        .go(
            "b",
            "ba",
            json!({"trigger":"on_tick","target":"state","state":"a"}),
        )
        .compile();
    let mut rig = Rig::new(&tree);
    let report = rig.tick(0.1);
    assert_eq!(report.transitions.len(), MAX_TRANSITIONS_PER_TICK as usize);
    assert_eq!(report.passes, MAX_TRANSITIONS_PER_TICK);
    for _ in 0..40 {
        rig.tick(0.1);
    }
    assert!(
        rig.instance.debug().log.len() <= MAX_LOG,
        "the log is bounded"
    );
    assert_eq!(rig.instance.debug().log.len(), MAX_LOG);
}

// ----------------------------------------------------------------- evaluators

#[test]
fn evaluator_writes_are_visible_to_transitions_in_the_same_tick() {
    let tree = S::new()
        .var("a", "int", json!(0))
        .var("b", "int", json!(0))
        .var("sum", "int", Value::Null)
        .state("s1", "root", json!({}))
        .go(
            "s1",
            "t",
            json!({"trigger":"on_tick","target":"state","state":"s2"}),
        )
        .cond("t", "t_c", "sum", "ge", json!(5))
        .state("s2", "root", json!({}))
        .eval(
            "e",
            "compute_evaluator",
            json!({"op":"add","a":"a","b":"b","key":"sum"}),
        )
        .compile();
    let mut rig = Rig::new(&tree);
    rig.tick(0.1);
    assert_eq!(rig.path(), "s1");
    assert_eq!(rig.instance.blackboard().get("sum"), Some(&BbValue::Int(0)));
    rig.set("a", BbValue::Int(2));
    rig.set("b", BbValue::Int(3));
    // The evaluator recomputes sum before transitions run: same tick.
    let report = rig.tick(0.1);
    assert_eq!(rig.instance.blackboard().get("sum"), Some(&BbValue::Int(5)));
    assert_eq!(rig.path(), "s2");
    assert_eq!(report.transitions.len(), 1);
}

#[test]
fn built_in_evaluators_copy_inputs_and_compute() {
    let tree = S::new()
        .var("pos", "vec3", Value::Null)
        .var("home", "vec3", json!([0.0, 0.0, 0.0]))
        .var("dist", "float", Value::Null)
        .var("near", "bool", Value::Null)
        .var("far", "bool", Value::Null)
        .var("copy", "float", Value::Null)
        .var("lim", "float", json!(2.0))
        .var("hi", "float", Value::Null)
        .var("n", "int", json!(7))
        .var("m", "int", json!(3))
        .var("prod", "int", Value::Null)
        .state("s1", "root", json!({}))
        .eval(
            "e_pos",
            "copy_evaluator",
            json!({"input":"self.position","key":"pos"}),
        )
        .eval(
            "e_dist",
            "compute_evaluator",
            json!({"op":"distance","a":"pos","b":"home","key":"dist"}),
        )
        .eval(
            "e_near",
            "compute_evaluator",
            json!({"op":"not","a":"far","key":"near"}),
        )
        .eval(
            "e_copy",
            "copy_evaluator",
            json!({"from":"dist","key":"copy"}),
        )
        .eval(
            "e_hi",
            "compute_evaluator",
            json!({"op":"max","a":"dist","b":"lim","key":"hi"}),
        )
        .eval(
            "e_prod",
            "compute_evaluator",
            json!({"op":"mul","a":"n","b":"m","key":"prod"}),
        )
        .compile();
    let mut rig = Rig::new(&tree);
    rig.host
        .inputs
        .insert("self.position".into(), BbValue::Vec3([3.0, 0.0, 4.0]));
    rig.tick(0.1);
    let bb = rig.instance.blackboard();
    assert_eq!(bb.get("pos"), Some(&BbValue::Vec3([3.0, 0.0, 4.0])));
    assert_eq!(bb.get("dist"), Some(&BbValue::Float(5.0)));
    assert_eq!(bb.get("copy"), Some(&BbValue::Float(5.0)));
    assert_eq!(bb.get("hi"), Some(&BbValue::Float(5.0)));
    assert_eq!(bb.get("prod"), Some(&BbValue::Int(21)));
    assert_eq!(
        bb.get("near"),
        None,
        "an unset operand leaves the result unset"
    );
    // A vanished input clears everything derived from it.
    rig.host.inputs.clear();
    rig.tick(0.1);
    let bb = rig.instance.blackboard();
    assert_eq!(bb.get("pos"), None);
    assert_eq!(bb.get("dist"), None);
    assert_eq!(bb.get("copy"), None);
    assert_eq!(bb.get("prod"), Some(&BbValue::Int(21)));
}

#[test]
fn native_evaluators_start_once_tick_every_tick_and_stop_with_the_tree() {
    let tree = S::new()
        .state("s1", "root", json!({}))
        .go(
            "s1",
            "t",
            json!({"trigger":"on_event","event":"end","target":"succeeded"}),
        )
        .eval("ev", "native_evaluator", json!({"name":"eval"}))
        .compile();
    let mut rig = Rig::new(&tree);
    rig.ticks(3, 0.1);
    assert_eq!(
        rig.host.log,
        ["eval_start:ev", "eval_tick:ev", "eval_tick:ev"]
    );
    rig.instance.send_event("end");
    let report = rig.tick(0.1);
    assert_eq!(report.finished, Some(Status::Success));
    assert_eq!(
        rig.host.log.last().map(String::as_str),
        Some("eval_stop:ev")
    );
    assert_eq!(rig.host.filter("eval_stop:").len(), 1);
}

// -------------------------------------------------------------------- tasks

#[test]
fn instant_tasks_run_on_entry_without_completing_the_state() {
    let tree = S::new()
        .var("seen", "bool", json!(false))
        .var("gone", "int", json!(4))
        .state("s1", "root", json!({}))
        .task(
            "s1",
            "set",
            "set_blackboard",
            json!({"key":"seen","value":true}),
        )
        .task("s1", "clr", "clear_blackboard", json!({"key":"gone"}))
        .task("s1", "ev", "emit_event", json!({"event":"hello"}))
        .compile();
    let mut rig = Rig::new(&tree);
    let report = rig.ticks(3, 0.1).pop().unwrap();
    assert_eq!(
        rig.instance.blackboard().get("seen"),
        Some(&BbValue::Bool(true))
    );
    assert_eq!(rig.instance.blackboard().get("gone"), None);
    assert_eq!(rig.host.events, ["hello"], "emitted once on entry");
    assert!(report.finished.is_none() && rig.instance.phase() == Phase::Running);
}

#[test]
fn completion_policy_any_versus_all() {
    let tree = |completion: &str, second: &str| {
        S::new()
            .state("s", "root", json!({"completion": completion}))
            .wait("s", "w1", 0.1)
            .task(
                "s",
                "w2",
                if second == "wait" {
                    "wait"
                } else {
                    "native_task"
                },
                if second == "wait" {
                    json!({"seconds":0.3})
                } else {
                    json!({"name":second})
                },
            )
            .go(
                "s",
                "t",
                json!({"trigger":"on_completed","target":"state","state":"done"}),
            )
            .state("done", "root", json!({}))
            .compile()
    };
    let fire_tick = |completion: &str, second: &str| {
        let mut rig = Rig::new(&tree(completion, second));
        for _ in 0..20 {
            rig.tick(0.1);
            if rig.path() == "done" {
                return rig.instance.ticks();
            }
        }
        0
    };
    assert_eq!(fire_tick("any", "wait"), 2, "first task done");
    assert_eq!(fire_tick("all", "wait"), 4, "both tasks done");
    assert_eq!(
        fire_tick("all", "fail"),
        1,
        "an early failure completes all at once"
    );
}

#[test]
fn completion_results_are_decided_by_the_first_finisher_for_any() {
    let build = |native: &str| {
        S::new()
            .state("s", "root", json!({}))
            .wait("s", "w", 0.2)
            .native("s", "n", native)
            .go(
                "s",
                "ok",
                json!({"trigger":"on_succeeded","target":"state","state":"good"}),
            )
            .go(
                "s",
                "ko",
                json!({"trigger":"on_failed","target":"state","state":"bad"}),
            )
            .state("good", "root", json!({}))
            .state("bad", "root", json!({}))
            .compile()
    };
    // The failing native task finishes (tick 2) before the wait (tick 3).
    let mut rig = Rig::new(&build("run_fail"));
    rig.ticks(2, 0.1);
    assert_eq!(rig.path(), "bad");
    // The wait finishes first: success wins even though the native fails later.
    let mut rig = Rig::new(&build("run"));
    rig.ticks(3, 0.1);
    assert_eq!(rig.path(), "good");
}

#[test]
fn move_to_drives_the_host_and_is_cancelled_when_its_state_exits() {
    let tree = S::new()
        .state("walk", "root", json!({}))
        .task("walk", "mv", "move_to", json!({"target":[1.0,0.0,2.0]}))
        .go(
            "walk",
            "arrive",
            json!({"trigger":"on_succeeded","target":"state","state":"idle"}),
        )
        .go(
            "walk",
            "stop",
            json!({"trigger":"on_event","event":"stop","target":"state","state":"idle"}),
        )
        .state("idle", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&tree);
    rig.ticks(2, 0.1);
    assert_eq!(rig.host.moves, [[1.0, 0.0, 2.0]]);
    rig.host.status = MoveStatus::Arrived;
    rig.tick(0.1);
    assert_eq!(rig.path(), "idle");
    assert_eq!(rig.host.cancels, 0, "a finished move needs no cancel");

    let mut rig = Rig::new(&tree);
    rig.ticks(2, 0.1);
    rig.instance.send_event("stop");
    rig.tick(0.1);
    assert_eq!(rig.path(), "idle");
    assert_eq!(rig.host.cancels, 1, "leaving the state cancels the move");

    // A move from a key, and a timeout that fails the task.
    let keyed = S::new()
        .var("goal", "vec3", json!([4.0, 0.0, 4.0]))
        .state("walk", "root", json!({}))
        .task(
            "walk",
            "mv",
            "move_to",
            json!({"key":"goal","timeout":0.25}),
        )
        .go(
            "walk",
            "late",
            json!({"trigger":"on_failed","target":"state","state":"idle"}),
        )
        .state("idle", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&keyed);
    rig.ticks(3, 0.1);
    assert_eq!(rig.host.moves, [[4.0, 0.0, 4.0]]);
    assert_eq!(rig.path(), "walk");
    rig.tick(0.1);
    assert_eq!(rig.path(), "idle", "0.3 s >= timeout 0.25 s");
    assert_eq!(rig.host.cancels, 1);
}

// ----------------------------------------------------------------- completion

#[test]
fn transitions_can_complete_the_tree_either_way() {
    for (target, expected) in [("succeeded", Status::Success), ("failed", Status::Failure)] {
        let tree = S::new()
            .state("a", "root", json!({}))
            .go(
                "a",
                "end",
                json!({"trigger":"on_event","event":"end","target":target}),
            )
            .state("child", "a", json!({}))
            .compile();
        let mut rig = Rig::new(&tree);
        rig.tick(0.1);
        assert_eq!(rig.path(), "a/child");
        rig.instance.send_event("end");
        let report = rig.tick(0.1);
        assert_eq!(report.finished, Some(expected));
        assert_eq!(report.exited, ["child", "a"]);
        assert_eq!(rig.instance.last_finished(), Some(expected));
        assert_eq!(rig.instance.phase(), Phase::Finished(expected));
        assert!(rig.instance.active_path().is_empty());
        let debug = rig.instance.debug();
        assert_eq!(
            debug.log.last().map(|r| r.event),
            Some(LogEvent::Finished(expected))
        );
    }
}

#[test]
fn an_unhandled_completion_completes_the_tree_with_its_result() {
    for (native, expected) in [("run_ok", Status::Success), ("run_fail", Status::Failure)] {
        let tree = S::new()
            .state("a", "root", json!({}))
            .native("a", "t", native)
            .compile();
        let mut rig = Rig::new(&tree);
        assert!(rig.tick(0.1).finished.is_none());
        let report = rig.tick(0.1);
        assert_eq!(report.finished, Some(expected), "{native}");
    }
}

#[test]
fn a_completion_with_a_pending_handler_does_not_complete_the_tree() {
    // The on_completed transition is waiting out its delay.
    let tree = S::new()
        .state("a", "root", json!({}))
        .native("a", "t", "ok")
        .go(
            "a",
            "later",
            json!({"trigger":"on_completed","target":"state","state":"b","delay":0.2}),
        )
        .state("b", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&tree);
    let reports = rig.ticks(4, 0.1);
    assert!(reports.iter().all(|r| r.finished.is_none()));
    assert_eq!(rig.path(), "b");
}

#[test]
fn a_child_completion_triggers_the_parents_completion_transition() {
    let tree = S::new()
        .state("p", "root", json!({}))
        .go(
            "p",
            "t",
            json!({"trigger":"on_succeeded","target":"state","state":"q"}),
        )
        .state("c", "p", json!({}))
        .wait("c", "w", 0.1)
        .state("q", "root", json!({}))
        .compile();
    let mut rig = Rig::new(&tree);
    rig.tick(0.1);
    assert_eq!(rig.path(), "p/c");
    rig.tick(0.1);
    assert_eq!(rig.path(), "q");
}

// -------------------------------------------------------------- determinism

#[test]
fn equal_inputs_give_equal_reports_logs_and_blackboards() {
    let run = || {
        let tree = S::new()
            .var("n", "int", json!(0))
            .state("a", "root", json!({}))
            .wait("a", "w", 0.35)
            .go("a", "t", json!({"trigger":"on_completed","target":"state","state":"b","delay":0.1,"delay_variance":0.05}))
            .state("b", "root", json!({}))
            .wait("b", "w2", 0.2)
            .go("b", "t2", json!({"trigger":"on_completed","target":"state","state":"a"}))
            .compile();
        let mut rig = Rig::seeded(&tree, 42);
        let reports: Vec<_> = (0..60).map(|_| rig.tick(1.0 / 30.0)).collect();
        let debug = serde_json::to_value(rig.instance.debug()).unwrap();
        (reports, debug, rig.host.log.clone())
    };
    let first = run();
    let second = run();
    assert_eq!(first, second);
    assert!(first.0.iter().any(|r| !r.transitions.is_empty()));
}
