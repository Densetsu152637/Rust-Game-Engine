//! `script_graph`: semantics, traps, determinism and one negative document per code.
mod common;

use common::{Doc, Rec, sigs};
use engine_graph::{Strictness, validate};
use engine_script::{
    Budget, ScriptInstance, Trap, Trigger, Value,
    codes::{self, graph as gc},
    graph::{self, Program, shared_kind},
};
use serde_json::{Value as Json, json};
use std::{collections::BTreeMap, sync::Arc};

fn program(doc: &Doc) -> Arc<Program> {
    let document = doc.build();
    let found = validate(&document, shared_kind(), Strictness::Complete);
    assert!(found.is_empty(), "document is valid: {found:#?}");
    Arc::new(graph::compile(&document, &sigs()).unwrap_or_else(|d| panic!("compiles: {d:#?}")))
}

fn codes_of(doc: &Doc) -> Vec<String> {
    let document = doc.build();
    let mut found: Vec<String> = validate(&document, shared_kind(), Strictness::Complete)
        .into_iter()
        .map(|d| d.code)
        .collect();
    if found.is_empty()
        && let Err(d) = graph::compile(&document, &sigs())
    {
        found = d.into_iter().map(|d| d.code).collect();
    }
    found
}

fn start(program: &Arc<Program>) -> (engine_script::graph::GraphInstance, Rec) {
    let mut instance = program.instantiate(&BTreeMap::new()).expect("instance");
    let mut host = Rec::default();
    instance
        .handle(&Trigger::Start, &mut host, &mut Budget::default())
        .unwrap_or_else(|f| panic!("start runs: {f}"));
    (instance, host)
}

fn int_var(instance: &engine_script::graph::GraphInstance, name: &str) -> i64 {
    match instance.variable(name) {
        Some(Value::Int(i)) => *i,
        other => panic!("{name} is {other:?}"),
    }
}

fn run_start_err(doc: &Doc, budget: Budget) -> engine_script::Fault {
    let program = program(doc);
    let mut instance = program.instantiate(&BTreeMap::new()).unwrap();
    let mut host = Rec::default();
    instance
        .handle(&Trigger::Start, &mut host, &mut { budget })
        .expect_err("the run traps")
}

/// on_start -> for_range(0..5): sum = sum + index
fn sum_loop() -> Doc {
    let mut d = Doc::new();
    d.var("sum", "int", json!(0))
        .node("s", "on_start", json!({}))
        .node("loop", "for_range", json!({"start": 0, "end": 5}))
        .node("get", "get_var", json!({"variable": "sum"}))
        .node("add", "add", json!({"type": "int"}))
        .node("set", "set_var", json!({"variable": "sum"}))
        .node("msg", "const", json!({"type": "string", "value": "done"}))
        .node("log", "log", json!({}))
        .flow("s", "then", "loop")
        .flow("loop", "body", "set")
        .flow("loop", "completed", "log")
        .data("get", "value", "add", "a")
        .data("loop", "index", "add", "b")
        .data("add", "result", "set", "value")
        .data("msg", "value", "log", "message");
    d
}

#[test]
fn loops_variables_and_logs_run() {
    let (instance, host) = start(&program(&sum_loop()));
    assert_eq!(int_var(&instance, "sum"), 10);
    assert_eq!(host.logs, ["done"], "logs reach the host, never stdout");
}

#[test]
fn branch_sequence_and_do_once_follow_edge_order() {
    let mut d = Doc::new();
    d.var("trace", "string", json!(""))
        .node("u", "on_update", json!({}))
        .node("seq", "sequence", json!({}))
        .node("once", "do_once", json!({}))
        .node("br", "branch", json!({}))
        .node("yes", "const", json!({"type": "bool", "value": true}))
        .node("get1", "get_var", json!({"variable": "trace"}))
        .node("a", "const", json!({"type": "string", "value": "a"}))
        .node("cat1", "concat", json!({}))
        .node("set1", "set_var", json!({"variable": "trace"}))
        .node("get2", "get_var", json!({"variable": "trace"}))
        .node("b", "const", json!({"type": "string", "value": "b"}))
        .node("cat2", "concat", json!({}))
        .node("set2", "set_var", json!({"variable": "trace"}))
        .node("get3", "get_var", json!({"variable": "trace"}))
        .node("c", "const", json!({"type": "string", "value": "c"}))
        .node("cat3", "concat", json!({}))
        .node("set3", "set_var", json!({"variable": "trace"}))
        .flow("u", "then", "seq")
        .flow("seq", "then", "once")
        .flow("once", "then", "set1")
        .flow("seq", "then", "br")
        .flow("br", "true", "set2")
        .flow("br", "false", "set3")
        .data("yes", "value", "br", "condition")
        .data("get1", "value", "cat1", "a")
        .data("a", "value", "cat1", "b")
        .data("cat1", "result", "set1", "value")
        .data("get2", "value", "cat2", "a")
        .data("b", "value", "cat2", "b")
        .data("cat2", "result", "set2", "value")
        .data("get3", "value", "cat3", "a")
        .data("c", "value", "cat3", "b")
        .data("cat3", "result", "set3", "value");
    let program = program(&d);
    let mut instance = program.instantiate(&BTreeMap::new()).unwrap();
    let mut host = Rec::default();
    for _ in 0..2 {
        instance
            .handle(
                &Trigger::Update { dt: 0.1 },
                &mut host,
                &mut Budget::default(),
            )
            .unwrap();
    }
    // First tick: do_once runs "a", then the branch runs "b". Second: only "b".
    assert_eq!(
        instance.variable("trace"),
        Some(&Value::str("abb")),
        "sequence runs in edge order; do_once fires once"
    );
}

#[test]
fn wait_resumes_after_accumulated_time() {
    let mut d = Doc::new();
    d.var("flag", "bool", json!(false))
        .node("s", "on_start", json!({}))
        .node("w", "wait", json!({"seconds": 1.0}))
        .node("t", "const", json!({"type": "bool", "value": true}))
        .node("set", "set_var", json!({"variable": "flag"}))
        .flow("s", "then", "w")
        .flow("w", "then", "set")
        .data("t", "value", "set", "value");
    let (mut instance, mut host) = start(&program(&d));
    assert_eq!(instance.debug().pending, 1);
    for dt in [0.5f32, 0.4] {
        instance
            .handle(&Trigger::Update { dt }, &mut host, &mut Budget::default())
            .unwrap();
        assert_eq!(instance.variable("flag"), Some(&Value::Bool(false)));
    }
    instance
        .handle(
            &Trigger::Update { dt: 0.2 },
            &mut host,
            &mut Budget::default(),
        )
        .unwrap();
    assert_eq!(instance.variable("flag"), Some(&Value::Bool(true)));
    assert_eq!(instance.debug().pending, 0);
}

fn function_doc(depth: usize) -> Doc {
    // on_start -> f0 -> f1 -> ... -> f{depth-1}; the last one sets `hit`.
    let mut d = Doc::new();
    d.var("hit", "int", json!(0))
        .node("s", "on_start", json!({}));
    d.node_with(
        "c_start",
        "call_function",
        json!({"function": "f0"}),
        json!([]),
    );
    d.flow("s", "then", "c_start");
    for i in 0..depth {
        let (entry, name) = (format!("e{i}"), format!("f{i}"));
        d.node(&entry, "function_entry", json!({"name": name}));
        if i + 1 < depth {
            let call = format!("c{i}");
            d.node_with(
                &call,
                "call_function",
                json!({"function": format!("f{}", i + 1)}),
                json!([]),
            );
            d.flow(&entry, "then", &call);
        } else {
            d.node("one", "const", json!({"type": "int", "value": 1}))
                .node("set", "set_var", json!({"variable": "hit"}))
                .data("one", "value", "set", "value")
                .flow(&entry, "then", "set");
        }
    }
    d
}

#[test]
fn functions_take_arguments_and_return_results() {
    let mut d = Doc::new();
    d.var("v", "int", json!(0))
        .node("s", "on_start", json!({}))
        .node_with(
            "fe",
            "function_entry",
            json!({"name": "double"}),
            json!([{"name": "x", "direction": "out", "type": "int", "multiple": true}]),
        )
        .node("twice", "add", json!({"type": "int"}))
        .node_with(
            "ret",
            "return",
            json!({}),
            json!([{"name": "r", "direction": "in", "type": "int"}]),
        )
        .node_with(
            "call",
            "call_function",
            json!({"function": "double"}),
            json!([
                {"name": "x", "direction": "in", "type": "int"},
                {"name": "r", "direction": "out", "type": "int"}
            ]),
        )
        .node("k", "const", json!({"type": "int", "value": 21}))
        .node("set", "set_var", json!({"variable": "v"}))
        .flow("fe", "then", "ret")
        .data("fe", "x", "twice", "a")
        .data("fe", "x", "twice", "b")
        .data("twice", "result", "ret", "r")
        .flow("s", "then", "call")
        .data("k", "value", "call", "x")
        .flow("call", "then", "set")
        .data("call", "r", "set", "value");
    let (instance, _) = start(&program(&d));
    assert_eq!(int_var(&instance, "v"), 42);
}

#[test]
fn guest_call_depth_is_sixteen_frames() {
    let (instance, _) = start(&program(&function_doc(15)));
    assert_eq!(
        int_var(&instance, "hit"),
        1,
        "15 nested calls fit in 16 frames"
    );
    let fault = run_start_err(&function_doc(16), Budget::default());
    assert_eq!(fault.code(), codes::CALL_DEPTH);
    assert!(fault.site.is_some(), "traps carry the failing node");
}

#[test]
fn host_calls_pure_and_effectful() {
    let mut d = Doc::new();
    d.var("sum", "float", json!(0.0))
        .node("s", "on_start", json!({}))
        .node_with(
            "add",
            "call",
            json!({"function": "game.add"}),
            json!([
                {"name": "a", "direction": "in", "type": "float"},
                {"name": "b", "direction": "in", "type": "float"},
                {"name": "result", "direction": "out", "type": "float"}
            ]),
        )
        .node("one", "const", json!({"type": "float", "value": 1.5}))
        .node("two", "const", json!({"type": "int", "value": 2}))
        .node("set", "set_var", json!({"variable": "sum"}))
        .node_with(
            "rec",
            "call",
            json!({"function": "game.record"}),
            json!([{"name": "n", "direction": "in", "type": "int"}]),
        )
        .data("one", "value", "add", "a")
        .data("two", "value", "add", "b") // int widens to float
        .data("add", "result", "set", "value")
        .flow("s", "then", "rec")
        .flow("rec", "then", "set")
        .data("two", "value", "rec", "n");
    let (instance, host) = start(&program(&d));
    assert_eq!(instance.variable("sum"), Some(&Value::Float(3.5)));
    assert_eq!(host.calls.len(), 2);
    assert_eq!(
        host.calls[0],
        (1, vec![Value::Int(2)]),
        "effectful call first"
    );
    assert_eq!(
        host.calls[1].0, 0,
        "the pure call runs when its value is read"
    );
}

#[test]
fn events_reach_on_event_and_emit_goes_to_the_host() {
    let mut d = Doc::new();
    d.node("ev", "on_event", json!({"event": "collision.enter"}))
        .node(
            "out",
            "emit_event",
            json!({"event": "game.hit", "payload": {"k": 1}}),
        )
        .flow("ev", "then", "out")
        .data("ev", "source", "out", "target");
    let program = program(&d);
    assert_eq!(program.subscriptions(), ["collision.enter"]);
    let mut instance = program.instantiate(&BTreeMap::new()).unwrap();
    let mut host = Rec::default();
    let payload = json!({});
    instance
        .handle(
            &Trigger::Event {
                name: "collision.enter",
                payload: &payload,
                source: Some(engine_script::EntityRef(9)),
            },
            &mut host,
            &mut Budget::default(),
        )
        .unwrap();
    assert_eq!(host.emitted.len(), 1);
    assert_eq!(host.emitted[0].0, "game.hit");
    assert_eq!(host.emitted[0].2, Some(engine_script::EntityRef(9)));
    // A different event name runs nothing.
    let mut host = Rec::default();
    instance
        .handle(
            &Trigger::Event {
                name: "other.event",
                payload: &payload,
                source: None,
            },
            &mut host,
            &mut Budget::default(),
        )
        .unwrap();
    assert!(host.emitted.is_empty());
}

#[test]
fn two_runs_are_identical() {
    let program = program(&sum_loop());
    let run = || {
        let mut instance = program.instantiate(&BTreeMap::new()).unwrap();
        let mut host = Rec::default();
        let mut budget = Budget::default();
        instance
            .handle(&Trigger::Start, &mut host, &mut budget)
            .unwrap();
        (instance.debug(), host.logs, budget.fuel)
    };
    assert_eq!(run(), run());
}

// --- traps ---------------------------------------------------------------------------

fn while_forever(max: i64) -> Doc {
    let mut d = Doc::new();
    d.node("s", "on_start", json!({}))
        .node("w", "while", json!({"max_iterations": max}))
        .node("t", "const", json!({"type": "bool", "value": true}))
        .flow("s", "then", "w")
        .data("t", "value", "w", "condition");
    d
}

#[test]
fn fuel_exhaustion_traps_with_the_loop_site() {
    let fault = run_start_err(&while_forever(10_000), Budget::default());
    assert_eq!(fault.code(), codes::FUEL_EXHAUSTED);
    assert!(fault.site.is_some());
    // The iteration bound traps the same way even with plenty of fuel.
    let fault = run_start_err(&while_forever(5), Budget::with_fuel(1_000_000));
    assert_eq!(fault.code(), codes::FUEL_EXHAUSTED);
}

#[test]
fn state_and_string_memory_are_capped() {
    // concat doubling hits the 4096-byte string limit.
    let mut d = Doc::new();
    d.var("s", "string", json!("x"))
        .node("on", "on_start", json!({}))
        .node("loop", "for_range", json!({"start": 0, "end": 20}))
        .node("get", "get_var", json!({"variable": "s"}))
        .node("cat", "concat", json!({}))
        .node("set", "set_var", json!({"variable": "s"}))
        .flow("on", "then", "loop")
        .flow("loop", "body", "set")
        .data("get", "value", "cat", "a")
        .data("get", "value", "cat", "b")
        .data("cat", "result", "set", "value");
    let fault = run_start_err(&d, Budget::with_fuel(100_000));
    assert_eq!(fault.code(), codes::MEMORY_LIMIT);
    // A small state budget traps on the variable write instead.
    let mut budget = Budget::with_fuel(100_000);
    budget.max_state_bytes = 200;
    let fault = run_start_err(&d, budget);
    assert_eq!(fault.code(), codes::MEMORY_LIMIT);
}

#[test]
fn numeric_boundaries_trap() {
    let divide = |ty: &str, a: Json, b: Json| {
        let mut d = Doc::new();
        d.var(
            "v",
            ty,
            json!(if ty == "int" { json!(0) } else { json!(0.0) }),
        )
        .node("s", "on_start", json!({}))
        .node("a", "const", json!({"type": ty, "value": a}))
        .node("b", "const", json!({"type": ty, "value": b}))
        .node("div", "div", json!({"type": ty}))
        .node("set", "set_var", json!({"variable": "v"}))
        .flow("s", "then", "set")
        .data("a", "value", "div", "a")
        .data("b", "value", "div", "b")
        .data("div", "result", "set", "value");
        run_start_err(&d, Budget::default())
    };
    assert_eq!(
        divide("float", json!(1.0), json!(0.0)).code(),
        codes::VALUE_NONFINITE
    );
    assert_eq!(divide("int", json!(1), json!(0)).code(), codes::HOST_FAILED);
    assert_eq!(
        divide("int", json!(i64::MIN), json!(-1)).code(),
        codes::HOST_FAILED,
        "overflow traps"
    );
}

#[test]
fn the_emit_budget_traps_with_the_event_limit() {
    let mut d = Doc::new();
    d.node("s", "on_start", json!({}))
        .node("loop", "for_range", json!({"start": 0, "end": 17}))
        .node("e", "emit_event", json!({"event": "game.ping"}))
        .flow("s", "then", "loop")
        .flow("loop", "body", "e");
    let fault = run_start_err(&d, Budget::with_fuel(100_000));
    assert_eq!(fault.code(), codes::EVENT_LIMIT);
}

#[test]
fn json_to_mismatch_is_a_host_argument_trap_and_host_failure_propagates() {
    let mut d = Doc::new();
    d.var("v", "float", json!(0.0))
        .node("ev", "on_event", json!({"event": "game.x"}))
        .node("conv", "json_to", json!({"type": "float"}))
        .node("set", "set_var", json!({"variable": "v"}))
        .flow("ev", "then", "set")
        .data("ev", "payload", "conv", "value")
        .data("conv", "result", "set", "value");
    let program = program(&d);
    let mut instance = program.instantiate(&BTreeMap::new()).unwrap();
    let payload = json!("not a number");
    let fault = instance
        .handle(
            &Trigger::Event {
                name: "game.x",
                payload: &payload,
                source: None,
            },
            &mut Rec::default(),
            &mut Budget::default(),
        )
        .unwrap_err();
    assert_eq!(fault.code(), codes::HOST_ARGUMENT);

    let mut d = Doc::new();
    d.node("s", "on_start", json!({}))
        .node_with("f", "call", json!({"function": "game.fail"}), json!([]))
        .flow("s", "then", "f");
    let fault = run_start_err(&d, Budget::default());
    assert!(matches!(fault.trap, Trap::Host(_)));
    assert_eq!(fault.code(), codes::HOST_FAILED);
}

#[test]
fn compiling_never_runs_host_code_and_check_works_without_signatures() {
    // `check_document` has no registry at all; a program that was only checked
    // cannot be instantiated.
    let mut d = Doc::new();
    d.node("s", "on_start", json!({}))
        .node_with(
            "rec",
            "call",
            json!({"function": "anything.goes"}),
            json!([{"name": "n", "direction": "in", "type": "int"}]),
        )
        .flow("s", "then", "rec");
    let document = d.build();
    assert!(validate(&document, shared_kind(), Strictness::Complete).is_empty());
    assert_eq!(
        graph::compile(&document, &sigs()).unwrap_err()[0].code,
        codes::CALL_UNKNOWN
    );
}

// --- one negative document per code ----------------------------------------------------

fn entry_and(extra: impl FnOnce(&mut Doc)) -> Doc {
    let mut d = Doc::new();
    d.node("s", "on_start", json!({}));
    extra(&mut d);
    d
}

#[test]
fn every_kind_code_has_a_negative_document() {
    // entry_missing
    let mut d = Doc::new();
    d.node("k", "const", json!({"type": "int", "value": 1}));
    assert!(codes_of(&d).contains(&gc::ENTRY_MISSING.to_owned()));

    // unreachable
    let d = entry_and(|d| {
        d.node("orphan", "do_once", json!({}));
    });
    assert_eq!(codes_of(&d), [gc::UNREACHABLE]);

    // flow_fanout: two edges from one flow out, and a merge into one exec.
    let d = entry_and(|d| {
        d.node("a", "do_once", json!({}))
            .node("b", "do_once", json!({}))
            .flow("s", "then", "a")
            .flow("s", "then", "b");
    });
    assert_eq!(codes_of(&d), [gc::FLOW_FANOUT]);
    let d = entry_and(|d| {
        d.node("u", "on_update", json!({}))
            .node("a", "do_once", json!({}))
            .flow("s", "then", "a")
            .flow("u", "then", "a");
    });
    assert!(codes_of(&d).contains(&gc::FLOW_FANOUT.to_owned()));

    // value_unavailable: a loop index read after the loop; an entry output of another entry.
    let d = entry_and(|d| {
        d.var("v", "int", json!(0))
            .node("loop", "for_range", json!({"end": 2}))
            .node("set", "set_var", json!({"variable": "v"}))
            .flow("s", "then", "loop")
            .flow("loop", "completed", "set")
            .data("loop", "index", "set", "value");
    });
    assert_eq!(codes_of(&d), [gc::VALUE_UNAVAILABLE]);
    let d = entry_and(|d| {
        d.var("v", "float", json!(0.0))
            .node("u", "on_update", json!({}))
            .node("set", "set_var", json!({"variable": "v"}))
            .flow("s", "then", "set")
            .data("u", "dt", "set", "value");
    });
    assert_eq!(codes_of(&d), [gc::VALUE_UNAVAILABLE]);

    // loop_bound
    let d = entry_and(|d| {
        d.node("loop", "for_range", json!({"end": 100_001}))
            .flow("s", "then", "loop");
    });
    assert_eq!(codes_of(&d), [gc::LOOP_BOUND]);

    // latent_in_function
    let mut d = Doc::new();
    d.node("s", "on_start", json!({}))
        .node("fe", "function_entry", json!({"name": "f"}))
        .node("w", "wait", json!({}))
        .flow("fe", "then", "w");
    assert!(codes_of(&d).contains(&gc::LATENT_IN_FUNCTION.to_owned()));

    // function_unknown / function_duplicate / function_recursive
    let d = entry_and(|d| {
        d.node_with("c", "call_function", json!({"function": "nope"}), json!([]))
            .flow("s", "then", "c");
    });
    assert_eq!(codes_of(&d), [gc::FUNCTION_UNKNOWN]);
    let d = entry_and(|d| {
        d.node("f1", "function_entry", json!({"name": "same"}))
            .node("f2", "function_entry", json!({"name": "same"}));
    });
    assert_eq!(codes_of(&d), [gc::FUNCTION_DUPLICATE]);
    let d = entry_and(|d| {
        d.node("f", "function_entry", json!({"name": "f"}))
            .node_with("c", "call_function", json!({"function": "f"}), json!([]))
            .flow("f", "then", "c");
    });
    assert_eq!(codes_of(&d), [gc::FUNCTION_RECURSIVE]);

    // function_ports: the call passes an argument the function does not declare.
    let d = entry_and(|d| {
        d.node("f", "function_entry", json!({"name": "f"}))
            .node_with(
                "c",
                "call_function",
                json!({"function": "f"}),
                json!([{"name": "x", "direction": "in", "type": "int"}]),
            )
            .flow("s", "then", "c");
    });
    assert_eq!(codes_of(&d), [gc::FUNCTION_PORTS]);

    // event_name
    let mut d = Doc::new();
    d.node("ev", "on_event", json!({"event": "Bad Name"}));
    assert_eq!(codes_of(&d), [gc::EVENT_NAME]);

    // const_type
    let d = entry_and(|d| {
        d.node("k", "const", json!({"type": "int", "value": "text"}));
    });
    assert_eq!(codes_of(&d), [gc::CONST_TYPE]);

    // type_mismatch: a float feeding an int-typed add.
    let d = entry_and(|d| {
        d.var("v", "int", json!(0))
            .node("f", "const", json!({"type": "float", "value": 1.0}))
            .node("add", "add", json!({"type": "int"}))
            .node("set", "set_var", json!({"variable": "v"}))
            .flow("s", "then", "set")
            .data("f", "value", "add", "a")
            .data("f", "value", "add", "b")
            .data("add", "result", "set", "value");
    });
    assert_eq!(codes_of(&d), [gc::TYPE_MISMATCH]);

    // code_size: a long pure chain re-evaluated by many readers.
    let mut d = Doc::new();
    d.var("v", "float", json!(0.0))
        .node("s", "on_start", json!({}))
        .node("seq", "sequence", json!({}))
        .node("k", "const", json!({"type": "float", "value": 1.0}))
        .flow("s", "then", "seq");
    d.node("add0", "add", json!({"type": "float"}))
        .data("k", "value", "add0", "a")
        .data("k", "value", "add0", "b");
    for i in 1..100 {
        let (id, before) = (format!("add{i}"), format!("add{}", i - 1));
        d.node(&id, "add", json!({"type": "float"}))
            .data(&before, "result", &id, "a")
            .data(&before, "result", &id, "b");
    }
    for i in 0..700 {
        let id = format!("set{i}");
        d.node(&id, "set_var", json!({"variable": "v"}))
            .flow("seq", "then", &id)
            .data("add99", "result", &id, "value");
    }
    assert_eq!(codes_of(&d), [gc::CODE_SIZE]);
}

#[test]
fn project_aware_call_codes() {
    // call.unknown: no such function.
    let mut d = Doc::new();
    d.node("s", "on_start", json!({}))
        .node_with("c", "call", json!({"function": "game.missing"}), json!([]))
        .flow("s", "then", "c");
    assert_eq!(codes_of(&d), [codes::CALL_UNKNOWN]);
    // call.ports: wrong parameter name; an impure function without flow; a pure
    // function with flow.
    let wrong = |function: &str, extra: Json, wire_flow: bool| {
        let mut d = Doc::new();
        d.node("s", "on_start", json!({})).node_with(
            "c",
            "call",
            json!({"function": function}),
            extra,
        );
        if wire_flow {
            d.flow("s", "then", "c");
        }
        codes_of(&d)
    };
    assert_eq!(
        wrong(
            "game.record",
            json!([{"name": "wrong", "direction": "in", "type": "int"}]),
            true
        ),
        [codes::CALL_PORTS]
    );
    assert_eq!(
        wrong(
            "game.record",
            json!([{"name": "n", "direction": "in", "type": "int"}]),
            false
        ),
        [codes::CALL_PORTS]
    );
    assert_eq!(
        wrong(
            "game.add",
            json!([
                {"name": "a", "direction": "in", "type": "float"},
                {"name": "b", "direction": "in", "type": "float"},
                {"name": "result", "direction": "out", "type": "float"}
            ]),
            true
        ),
        [codes::CALL_PORTS]
    );
}

#[test]
fn the_kind_is_registered_and_variables_are_typed() {
    let kind = shared_kind();
    assert_eq!(kind.name, "script_graph");
    assert!(kind.registry.len() >= 40);
    let mut instance_overrides = BTreeMap::new();
    instance_overrides.insert("sum".to_owned(), json!(5));
    let program = program(&sum_loop());
    let instance = program.instantiate(&instance_overrides).unwrap();
    assert_eq!(instance.variable("sum"), Some(&Value::Int(5)));
    instance_overrides.insert("sum".to_owned(), json!("five"));
    assert_eq!(
        program.instantiate(&instance_overrides).err().unwrap().code,
        codes::VARIABLE_INVALID
    );
}

#[test]
fn deep_documents_compile_and_run_without_native_recursion() {
    // 1,000 chained flow nodes and a 900-deep value chain, on a small stack.
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut d = Doc::new();
            d.var("v", "int", json!(0))
                .node("s", "on_start", json!({}))
                .node("one", "const", json!({"type": "int", "value": 1}));
            let mut prev = "one".to_owned();
            let mut prev_port = "value";
            for i in 0..900 {
                let id = format!("add{i}");
                d.node(&id, "add", json!({"type": "int"}))
                    .data(&prev, prev_port, &id, "a")
                    .data("one", "value", &id, "b");
                prev = id;
                prev_port = "result";
            }
            let mut last = "s".to_owned();
            for i in 0..1000 {
                let id = format!("set{i}");
                d.node(&id, "set_var", json!({"variable": "v"}))
                    .flow(&last, "then", &id);
                // Only the first set reads the long chain; the rest reuse `one`.
                if i == 0 {
                    d.data(&prev, prev_port, &id, "value");
                } else {
                    d.data("one", "value", &id, "value");
                }
                last = id;
            }
            let program = program(&d);
            let mut instance = program.instantiate(&BTreeMap::new()).unwrap();
            let mut budget = Budget::with_fuel(1_000_000);
            instance
                .handle(&Trigger::Start, &mut Rec::default(), &mut budget)
                .unwrap();
            assert_eq!(instance.variable("v"), Some(&Value::Int(1)));
        })
        .unwrap();
    handle.join().expect("no stack overflow");
}
