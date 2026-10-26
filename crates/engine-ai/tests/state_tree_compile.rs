//! Compiler and graph-kind rules: one focused negative case per diagnostic code.
mod st_common;

use engine_ai::state_tree::{compile, kind, shared_kind, st_codes};
use engine_graph::{GraphDocument, GraphEdit, PortType, Strictness, apply_edits, codes, validate};
use serde_json::{Value, json};
use st_common::S;
use std::collections::BTreeSet;

fn codes_of(doc: &GraphDocument) -> Vec<String> {
    match compile(doc) {
        Ok(_) => Vec::new(),
        Err(d) => d.into_iter().map(|d| d.code).collect(),
    }
}

/// A tree with one state; negative cases add one defect to it.
fn base() -> S {
    S::new()
        .var("flag", "bool", json!(false))
        .var("count", "int", json!(0))
        .var("goal", "vec3", Value::Null)
        .state("s", "root", json!({}))
}

/// `root` with a chain of `n` nested states.
fn nested(n: usize) -> S {
    let mut b = S::new().state("n0", "root", json!({}));
    for i in 1..n {
        b = b.state(&format!("n{i}"), &format!("n{}", i - 1), json!({}));
    }
    b
}

#[test]
fn every_state_tree_code_has_a_focused_negative_case() {
    let mut hit = BTreeSet::new();
    let mut expect = |code: &'static str, doc: GraphDocument| {
        let found = codes_of(&doc);
        assert!(
            found.iter().any(|c| c == code),
            "expected {code}, got {found:?}"
        );
        hit.insert(code);
    };

    expect(st_codes::ROOT_EMPTY, S::new().doc());

    let mut evaluators = base();
    for i in 0..33 {
        evaluators = evaluators.eval(&format!("e{i}"), "native_evaluator", json!({"name":"x"}));
    }
    expect(st_codes::EVALUATORS_LIMIT, evaluators.doc());

    let mut states = S::new();
    for i in 0..65 {
        states = states.state(&format!("t{i}"), "root", json!({}));
    }
    expect(st_codes::CHILDREN_LIMIT, states.doc());

    let mut tasks = base();
    for i in 0..17 {
        tasks = tasks.wait("s", &format!("w{i}"), 1.0);
    }
    expect(st_codes::TASKS_LIMIT, tasks.doc());

    let mut transitions = base();
    for i in 0..33 {
        transitions = transitions.go(
            "s",
            &format!("t{i}"),
            json!({"trigger":"on_tick","target":"succeeded"}),
        );
    }
    expect(st_codes::TRANSITIONS_LIMIT, transitions.doc());

    let mut conditions = base();
    for i in 0..17 {
        conditions = conditions.cond("s", &format!("c{i}"), "flag", "is_set", Value::Null);
    }
    expect(st_codes::CONDITIONS_LIMIT, conditions.doc());

    expect(st_codes::DEPTH, nested(33).doc());
    assert!(
        codes_of(&nested(32).doc()).is_empty(),
        "32 levels still compile"
    );

    // A child of a `self` state is only reachable by transition; a transition
    // owned by an unreachable state does not make its target reachable.
    expect(
        st_codes::UNREACHABLE,
        S::new()
            .state("p", "root", json!({"selection":"self"}))
            .state("c", "p", json!({}))
            .go(
                "c",
                "t",
                json!({"trigger":"on_tick","target":"state","state":"d"}),
            )
            .state("q", "root", json!({"selection":"self"}))
            .state("d", "q", json!({}))
            .doc(),
    );
    let unreachable: Vec<String> = compile(
        &S::new()
            .state("p", "root", json!({"selection":"self"}))
            .state("c", "p", json!({}))
            .go(
                "c",
                "t",
                json!({"trigger":"on_tick","target":"state","state":"d"}),
            )
            .state("q", "root", json!({"selection":"self"}))
            .state("d", "q", json!({}))
            .doc(),
    )
    .unwrap_err()
    .into_iter()
    .map(|d| d.path)
    .collect();
    assert_eq!(unreachable, ["/nodes/c", "/nodes/d"]);

    expect(
        st_codes::TARGET_UNKNOWN,
        base()
            .go(
                "s",
                "t",
                json!({"trigger":"on_tick","target":"state","state":"ghost"}),
            )
            .doc(),
    );
    // A node that exists but is not a state is also unknown.
    expect(
        st_codes::TARGET_UNKNOWN,
        base()
            .wait("s", "w", 1.0)
            .go(
                "s",
                "t",
                json!({"trigger":"on_tick","target":"state","state":"w"}),
            )
            .doc(),
    );
    expect(
        st_codes::TARGET_INVALID,
        base().go("s", "t", json!({"target":"state"})).doc(),
    );
    expect(
        st_codes::TARGET_INVALID,
        base()
            .go("s", "t", json!({"target":"succeeded","state":"s"}))
            .doc(),
    );
    expect(
        st_codes::TARGET_INVALID,
        base().go("s", "t", json!({"target":"parent"})).doc(),
    );
    expect(
        st_codes::TARGET_INVALID,
        base().go("s", "t", json!({"target":"next_sibling"})).doc(),
    );

    expect(
        st_codes::TRIGGER_EVENT,
        base()
            .go("s", "t", json!({"trigger":"on_event","target":"succeeded"}))
            .doc(),
    );
    expect(
        st_codes::TRIGGER_EVENT,
        base()
            .go(
                "s",
                "t",
                json!({"trigger":"on_tick","event":"go","target":"succeeded"}),
            )
            .doc(),
    );
    expect(
        st_codes::CONDITION_VALUE,
        base().cond("s", "c", "count", "eq", Value::Null).doc(),
    );
    expect(
        st_codes::CONDITION_VALUE,
        base().cond("s", "c", "count", "eq", json!("many")).doc(),
    );
    expect(
        st_codes::CONDITION_OPERATOR,
        base().cond("s", "c", "flag", "lt", json!(true)).doc(),
    );
    expect(
        st_codes::VALUE_TYPE,
        base()
            .task(
                "s",
                "t",
                "set_blackboard",
                json!({"key":"count","value":"x"}),
            )
            .doc(),
    );
    expect(
        st_codes::MOVE_TARGET,
        base().task("s", "t", "move_to", json!({})).doc(),
    );
    expect(
        st_codes::MOVE_TARGET,
        base()
            .task("s", "t", "move_to", json!({"key":"goal","target":[0,0,0]}))
            .doc(),
    );
    expect(
        st_codes::EVENT_NAME,
        base()
            .task("s", "t", "emit_event", json!({"event":""}))
            .doc(),
    );
    expect(
        st_codes::EVENT_NAME,
        base()
            .go(
                "s",
                "t",
                json!({"trigger":"on_event","event":"bad\u{0}","target":"succeeded"}),
            )
            .doc(),
    );
    expect(
        st_codes::EVENT_PAYLOAD,
        base()
            .task("s", "t", "emit_event", json!({"event":"x","payload":[1]}))
            .doc(),
    );
    expect(
        st_codes::NATIVE_NAME,
        base().native("s", "t", "has space").doc(),
    );
    expect(
        st_codes::NATIVE_NAME,
        base()
            .eval("e", "native_evaluator", json!({"name":"no/slash"}))
            .doc(),
    );
    // Evaluators: exactly one source, matching types.
    expect(
        st_codes::EVALUATOR,
        base()
            .eval("e", "copy_evaluator", json!({"key":"count"}))
            .doc(),
    );
    expect(
        st_codes::EVALUATOR,
        base()
            .eval(
                "e",
                "copy_evaluator",
                json!({"input":"x","from":"count","key":"count"}),
            )
            .doc(),
    );
    expect(
        st_codes::EVALUATOR,
        base()
            .eval("e", "copy_evaluator", json!({"from":"flag","key":"count"}))
            .doc(),
    );
    expect(
        st_codes::EVALUATOR,
        base()
            .eval(
                "e",
                "compute_evaluator",
                json!({"op":"add","a":"count","key":"count"}),
            )
            .doc(),
    );
    expect(
        st_codes::EVALUATOR,
        base()
            .eval(
                "e",
                "compute_evaluator",
                json!({"op":"not","a":"count","key":"flag"}),
            )
            .doc(),
    );
    expect(
        st_codes::EVALUATOR,
        base()
            .eval(
                "e",
                "compute_evaluator",
                json!({"op":"distance","a":"goal","b":"count","key":"count"}),
            )
            .doc(),
    );

    for code in st_codes::ALL {
        assert!(hit.contains(code), "no negative case for {code}");
    }
}

#[test]
fn graph_level_rules_apply_to_state_trees() {
    let check = |doc: GraphDocument| -> Vec<String> {
        validate(&doc, shared_kind(), Strictness::Complete)
            .into_iter()
            .map(|d| d.code)
            .collect()
    };
    // A hierarchy cycle is a graph cycle (states hold their parent edge).
    let cycle = S::new()
        .state("a", "root", json!({}))
        .state("b", "a", json!({}))
        .edge("b", "children", "a", "parent");
    let found = check(cycle.doc());
    assert!(found.iter().any(|c| c == codes::CYCLE), "{found:?}");
    // A state with two parents violates the single-connection parent port.
    let two_parents = S::new()
        .state("a", "root", json!({}))
        .state("b", "root", json!({}))
        .edge("a", "children", "b", "parent");
    let found = check(two_parents.doc());
    assert!(found.iter().any(|c| c == codes::EDGE_MULTIPLE), "{found:?}");
    // A state nobody links to is both unreachable and missing its parent.
    let orphan = S::new().state("a", "root", json!({})).doc();
    let mut orphan = orphan;
    orphan
        .nodes
        .push(serde_json::from_value(json!({"id":"loose","node_type":"state"})).unwrap());
    let found = check(orphan);
    assert!(
        found.iter().any(|c| c == codes::NODE_UNREACHABLE),
        "{found:?}"
    );
    assert!(
        found.iter().any(|c| c == codes::PORT_UNCONNECTED),
        "{found:?}"
    );
    // A task cannot be wired as a child state: custom port types only match themselves.
    let miswired = S::new()
        .state("a", "root", json!({}))
        .task("a", "w", "wait", json!({}))
        .edge("a", "children", "w", "owner");
    let found = check(miswired.doc());
    assert!(
        found.iter().any(|c| c == codes::EDGE_TYPE_MISMATCH),
        "{found:?}"
    );
    // Unknown parameters, wrong enum choices and missing required ones are graph codes.
    let unknown = base().go("s", "t", json!({"target":"succeeded","nope":1}));
    assert!(
        check(unknown.doc())
            .iter()
            .any(|c| c == codes::PARAM_UNKNOWN)
    );
    let bad_enum = base().go("s", "t", json!({"target":"sideways"}));
    assert!(
        check(bad_enum.doc())
            .iter()
            .any(|c| c == codes::PARAM_RANGE)
    );
    let missing = base().go("s", "t", json!({}));
    assert!(
        check(missing.doc())
            .iter()
            .any(|c| c == codes::PARAM_MISSING)
    );
    let bad_priority = base().go("s", "t", json!({"target":"succeeded","priority":5000}));
    assert!(
        check(bad_priority.doc())
            .iter()
            .any(|c| c == codes::PARAM_RANGE)
    );
    let bad_delay = base().go("s", "t", json!({"target":"succeeded","delay":-1.0}));
    assert!(
        check(bad_delay.doc())
            .iter()
            .any(|c| c == codes::PARAM_RANGE)
    );
    let bad_variable = base().cond("s", "c", "ghost", "is_set", Value::Null);
    assert!(
        check(bad_variable.doc())
            .iter()
            .any(|c| c == codes::PARAM_VARIABLE)
    );
    // Variable types are limited to the blackboard types.
    let doc = base().var("odd", "any", Value::Null);
    assert!(check(doc.doc()).iter().any(|c| c == codes::VARIABLE_TYPE));
}

#[test]
fn the_kind_is_a_single_root_acyclic_registry() {
    let kind = kind();
    assert_eq!(kind.name, "state_tree");
    assert!(kind.single_root && !kind.allow_cycles);
    let info = serde_json::to_value(shared_kind().info()).unwrap();
    let names: Vec<_> = info["node_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    for expected in [
        "root",
        "state",
        "transition",
        "blackboard_condition",
        "wait",
        "set_blackboard",
        "clear_blackboard",
        "emit_event",
        "move_to",
        "native_task",
        "copy_evaluator",
        "compute_evaluator",
        "native_evaluator",
    ] {
        assert!(names.iter().any(|n| n == expected), "missing {expected}");
    }
    assert_eq!(info["variable_types"].as_array().unwrap().len(), 6);
    // Attachments link through dedicated port types, so none can be wired as a child state.
    for (node, port, ty) in [
        ("state", "tasks", "st.task"),
        ("state", "conditions", "st.condition"),
        ("state", "transitions", "st.transition"),
        ("transition", "conditions", "st.condition"),
        ("root", "evaluators", "st.evaluator"),
    ] {
        let def = shared_kind()
            .registry
            .get(node)
            .unwrap()
            .port_def(port)
            .unwrap();
        assert_eq!(def.port.ty, PortType::Custom(ty.into()), "{node}.{port}");
    }
    // Instant tasks do not complete their state by default; waiting tasks do.
    for (node, considered) in [
        ("set_blackboard", false),
        ("clear_blackboard", false),
        ("emit_event", false),
        ("wait", true),
        ("move_to", true),
        ("native_task", true),
    ] {
        let def = shared_kind().registry.get(node).unwrap();
        assert_eq!(
            def.param_def("considered").unwrap().default,
            Some(json!(considered)),
            "{node}"
        );
    }
}

#[test]
fn a_compiled_tree_reports_its_names_and_navigation_use() {
    let tree = S::new()
        .var("goal", "vec3", json!([1.0, 0.0, 1.0]))
        .var("pos", "vec3", Value::Null)
        .state("a", "root", json!({}))
        .native("a", "t1", "alpha")
        .native("a", "t2", "beta")
        .task("a", "mv", "move_to", json!({"key":"goal"}))
        .go(
            "a",
            "t",
            json!({"trigger":"on_event","event":"go","target":"state","state":"b"}),
        )
        .state("b", "a", json!({}))
        .native("b", "t3", "alpha")
        .eval("e1", "native_evaluator", json!({"name":"sense"}))
        .eval(
            "e2",
            "copy_evaluator",
            json!({"input":"self.position","key":"pos"}),
        )
        .compile();
    assert_eq!(tree.state_count(), 2);
    assert_eq!(tree.state_id(0), Some("a"));
    assert_eq!(tree.state_index("b"), Some(1));
    assert_eq!(tree.transition_count(), 1);
    assert!(tree.uses_move_to());
    assert_eq!(
        tree.native_task_names().iter().collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    assert_eq!(
        tree.native_evaluator_names().iter().collect::<Vec<_>>(),
        ["sense"]
    );
    assert_eq!(
        tree.input_names().iter().collect::<Vec<_>>(),
        ["self.position"]
    );
    assert_eq!(tree.trigger_events().iter().collect::<Vec<_>>(), ["go"]);
    assert_eq!(tree.native_sites(), 4);
    assert_eq!(tree.variables().len(), 2);
}

#[test]
fn trees_are_authored_and_repaired_with_graph_edits() {
    let mut doc = GraphDocument::new("state_tree");
    let edits: Vec<GraphEdit> = serde_json::from_value(json!([
        {"op":"add_node","id":"root","node_type":"root"},
        {"op":"add_node","id":"idle","node_type":"state"},
        {"op":"add_node","id":"w","node_type":"wait","params":{"seconds":2.0}},
        {"op":"add_node","id":"t","node_type":"transition","params":{"target":"state","state":"idle"}},
        {"op":"connect","from":{"node":"root","port":"children"},"to":{"node":"idle","port":"parent"}},
        {"op":"connect","from":{"node":"idle","port":"tasks"},"to":{"node":"w","port":"owner"}},
        {"op":"connect","from":{"node":"idle","port":"transitions"},"to":{"node":"t","port":"owner"}}
    ]))
    .unwrap();
    apply_edits(&mut doc, &edits, shared_kind(), Strictness::Complete)
        .unwrap_or_else(|f| panic!("{f:?}"));
    assert!(compile(&doc).is_ok(), "{:?}", codes_of(&doc));
    // Repointing the transition at a state that does not exist is caught by the
    // kind check, which full validation runs.
    let mut broken = doc.clone();
    broken
        .nodes
        .iter_mut()
        .find(|n| n.id.as_str() == "t")
        .unwrap()
        .params
        .insert("state".into(), json!("gone"));
    let found = validate(&broken, shared_kind(), Strictness::Complete);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, st_codes::TARGET_UNKNOWN);
    assert_eq!(found[0].path, "/nodes/t/params/state");
}
