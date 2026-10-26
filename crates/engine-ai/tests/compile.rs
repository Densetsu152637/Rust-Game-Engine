mod common;

use common::B;
use engine_ai::{
    behavior_tree::{bt_codes, compile, kind, shared_kind},
    blackboard::{BbValue, Blackboard, BlackboardError},
};
use engine_graph::{
    GraphDocument, GraphEdit, GraphVariable, PortType, Strictness, apply_edits, codes, validate,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn codes_of(doc: &GraphDocument) -> Vec<String> {
    match compile(doc) {
        Ok(_) => Vec::new(),
        Err(d) => d.into_iter().map(|d| d.code).collect(),
    }
}

fn seq_with(children: &[(&str, &str, Value)]) -> B {
    let mut b = B::new().node("s", "sequence", json!({})).link("root", "s");
    for (id, ty, params) in children {
        b = b.node(id, ty, params.clone()).link("s", id);
    }
    b
}

#[test]
fn every_behavior_tree_code_has_a_focused_negative_case() {
    let mut hit = BTreeSet::new();
    let mut expect = |code: &'static str, doc: GraphDocument| {
        let found = codes_of(&doc);
        assert!(
            found.iter().any(|c| c == code),
            "expected {code}, got {found:?}"
        );
        hit.insert(code);
    };
    expect(
        bt_codes::COMPOSITE_EMPTY,
        B::new()
            .node("s", "sequence", json!({}))
            .link("root", "s")
            .doc(),
    );
    expect(
        bt_codes::CONDITION_VALUE,
        B::new()
            .var("k", "int", Value::Null)
            .node(
                "c",
                "blackboard_condition",
                json!({"key":"k","operator":"eq"}),
            )
            .leaf("n", "ok")
            .link("root", "c")
            .link("c", "n")
            .doc(),
    );
    expect(
        bt_codes::CONDITION_VALUE,
        B::new()
            .var("k", "int", Value::Null)
            .node(
                "c",
                "blackboard_condition",
                json!({"key":"k","operator":"eq","value":"x"}),
            )
            .leaf("n", "ok")
            .link("root", "c")
            .link("c", "n")
            .doc(),
    );
    expect(
        bt_codes::CONDITION_OPERATOR,
        B::new()
            .var("k", "bool", Value::Null)
            .node(
                "c",
                "blackboard_condition",
                json!({"key":"k","operator":"lt","value":true}),
            )
            .leaf("n", "ok")
            .link("root", "c")
            .link("c", "n")
            .doc(),
    );
    // lower_priority needs a sequence/selector parent.
    expect(
        bt_codes::OBSERVER_PRIORITY,
        B::new()
            .var("k", "bool", Value::Null)
            .node("p", "parallel", json!({}))
            .node(
                "c",
                "blackboard_condition",
                json!({"key":"k","operator":"is_set","abort":"lower_priority"}),
            )
            .leaf("n", "ok")
            .link("root", "p")
            .link("p", "c")
            .link("c", "n")
            .doc(),
    );
    expect(
        bt_codes::OBSERVER_PRIORITY,
        B::new()
            .var("k", "bool", Value::Null)
            .node(
                "c",
                "blackboard_condition",
                json!({"key":"k","operator":"is_set","abort":"both"}),
            )
            .leaf("n", "ok")
            .link("root", "c")
            .link("c", "n")
            .doc(),
    );
    expect(
        bt_codes::VALUE_TYPE,
        seq_with(&[("a", "set_blackboard", json!({"key":"k","value":"text"}))])
            .var("k", "int", Value::Null)
            .doc(),
    );
    expect(
        bt_codes::MOVE_TARGET,
        seq_with(&[("m", "move_to", json!({}))]).doc(),
    );
    expect(
        bt_codes::MOVE_TARGET,
        seq_with(&[("m", "move_to", json!({"target":[0,0,0],"key":"v"}))])
            .var("v", "vec3", Value::Null)
            .doc(),
    );
    expect(
        bt_codes::EVENT_NAME,
        seq_with(&[("e", "emit_event", json!({"event":"bad\nname"}))]).doc(),
    );
    expect(
        bt_codes::EVENT_PAYLOAD,
        seq_with(&[("e", "emit_event", json!({"event":"ok.event","payload":5}))]).doc(),
    );
    expect(
        bt_codes::NATIVE_NAME,
        seq_with(&[("n", "native_task", json!({"name":"has space"}))]).doc(),
    );
    // 70 nested inverters exceed the depth bound.
    let mut deep = B::new();
    let mut parent = "root".to_owned();
    for i in 0..70 {
        let id = format!("i{i}");
        deep = deep.node(&id, "inverter", json!({})).link(&parent, &id);
        parent = id;
    }
    deep = deep.leaf("leaf", "ok").link(&parent, "leaf");
    expect(bt_codes::DEPTH, deep.doc());
    let mut wide = B::new().node("s", "sequence", json!({})).link("root", "s");
    for i in 0..65 {
        let id = format!("n{i}");
        wide = wide.leaf(&id, "ok").link("s", &id);
    }
    expect(bt_codes::CHILDREN_LIMIT, wide.doc());
    for code in bt_codes::ALL {
        assert!(hit.contains(code), "no negative case for {code}");
    }
}

#[test]
fn graph_level_rules_apply_to_trees() {
    let negative: [(&str, GraphDocument); 5] = [
        (
            codes::PARAM_RANGE,
            seq_with(&[("w", "wait", json!({"seconds": -1}))]).doc(),
        ),
        (
            codes::PARAM_VARIABLE,
            seq_with(&[("c", "clear_blackboard", json!({"key":"nope"}))]).doc(),
        ),
        (
            codes::VARIABLE_TYPE,
            seq_with(&[("w", "wait", json!({}))])
                .var("flow", "any", Value::Null)
                .doc(),
        ),
        (
            codes::EDGE_MULTIPLE,
            seq_with(&[("a", "wait", json!({}))])
                .node("s2", "sequence", json!({}))
                .link("root", "s2")
                .link("s2", "a")
                .doc(),
        ),
        (
            codes::PARAM_MISSING,
            seq_with(&[("e", "emit_event", json!({}))]).doc(),
        ),
    ];
    for (code, doc) in negative {
        let found = codes_of(&doc);
        assert!(
            found.iter().any(|c| c == code),
            "expected {code}, got {found:?}"
        );
    }
    // A leaf with a service-typed edge to the wrong place is a type mismatch.
    let mut bad = seq_with(&[("w", "wait", json!({}))]).doc();
    bad.edges.push(
        serde_json::from_value(
            json!({"from":{"node":"s","port":"services"},"to":{"node":"w","port":"parent"}}),
        )
        .unwrap(),
    );
    assert!(codes_of(&bad).contains(&codes::EDGE_TYPE_MISMATCH.to_owned()));
}

#[test]
fn a_compiled_tree_reports_its_native_names_and_navigation_use() {
    let tree = B::new()
        .node("sel", "selector", json!({}))
        .node("s", "sequence", json!({}))
        .node("a", "native_task", json!({"name":"patrol.pick"}))
        .node("m", "move_to", json!({"target":[1,2,3]}))
        .leaf("b", "alert")
        .service(
            "sel",
            "sv",
            "native_service",
            json!({"name":"sense","interval":0.2}),
        )
        .link("root", "sel")
        .link("sel", "s")
        .link("s", "a")
        .link("s", "m")
        .link("sel", "b")
        .compile();
    assert_eq!(tree.node_count(), 6);
    assert_eq!(tree.node_id(0), Some("root"));
    assert!(tree.uses_move_to());
    assert_eq!(tree.native_sites(), 3);
    let names: Vec<_> = tree.native_names().iter().cloned().collect();
    assert_eq!(names, ["alert", "patrol.pick", "sense"]);
}

#[test]
fn the_kind_is_a_single_root_acyclic_registry() {
    let kind = kind();
    assert_eq!(kind.name, "behavior_tree");
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
        "sequence",
        "selector",
        "parallel",
        "random_selector",
        "inverter",
        "repeat",
        "retry",
        "cooldown",
        "time_limit",
        "force_success",
        "force_failure",
        "blackboard_condition",
        "wait",
        "set_blackboard",
        "clear_blackboard",
        "emit_event",
        "move_to",
        "native_task",
        "native_service",
        "set_blackboard_service",
        "emit_event_service",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing node type {expected}"
        );
    }
    assert_eq!(info["variable_types"].as_array().unwrap().len(), 6);
    // Services link through a dedicated port type, so they cannot be wired as children.
    let services = shared_kind()
        .registry
        .get("sequence")
        .unwrap()
        .port_def("services")
        .unwrap();
    assert_eq!(services.port.ty, PortType::Custom("bt.service".into()));
}

#[test]
fn trees_are_authored_and_repaired_with_graph_edits() {
    let kind = shared_kind();
    let mut doc = GraphDocument::new("behavior_tree");
    let edits: Vec<GraphEdit> = serde_json::from_value(json!([
        {"op":"add_node","id":"root","node_type":"root"},
        {"op":"add_node","id":"seq","node_type":"sequence"},
        {"op":"add_node","id":"w","node_type":"wait","params":{"seconds":2.5}},
        {"op":"add_node","id":"tick","node_type":"emit_event_service","params":{"event":"bt.tick","interval":1.0}},
        {"op":"connect","from":{"node":"root","port":"child"},"to":{"node":"seq","port":"parent"}},
        {"op":"connect","from":{"node":"seq","port":"children"},"to":{"node":"w","port":"parent"}},
        {"op":"connect","from":{"node":"seq","port":"services"},"to":{"node":"tick","port":"owner"}},
        {"op":"add_variable","variable":{"name":"mood","type":"string","default":"calm"}}
    ]))
    .unwrap();
    apply_edits(&mut doc, &edits, kind, Strictness::Complete).expect("a complete tree");
    assert!(compile(&doc).is_ok());
    let text = doc.to_json_pretty();
    assert_eq!(GraphDocument::from_json(text.as_bytes()).unwrap(), doc);
    // An incomplete edit batch is rejected whole and the document stays valid.
    let before = doc.clone();
    let bad: Vec<GraphEdit> = serde_json::from_value(json!([
        {"op":"add_node","id":"orphan","node_type":"wait"}
    ]))
    .unwrap();
    let failure = apply_edits(&mut doc, &bad, kind, Strictness::Complete).unwrap_err();
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.code == codes::NODE_UNREACHABLE)
    );
    assert_eq!(doc, before);
    assert!(validate(&doc, kind, Strictness::Complete).is_empty());
}

#[test]
fn blackboard_stores_typed_values_and_notifies_changes() {
    let variable = |name: &str, ty: PortType, default: Option<Value>| GraphVariable {
        name: name.into(),
        ty,
        default,
        description: String::new(),
    };
    let mut board = Blackboard::from_variables(&[
        variable("hp", PortType::Int, Some(json!(10))),
        variable("speed", PortType::Float, None),
        variable("name", PortType::String, None),
        variable("pos", PortType::Vec3, Some(json!([1.0, 2.0, 3.0]))),
        variable("target", PortType::Entity, None),
        variable("alert", PortType::Bool, None),
    ])
    .unwrap();
    assert_eq!(board.get("hp"), Some(&BbValue::Int(10)));
    assert_eq!(board.get("pos"), Some(&BbValue::Vec3([1.0, 2.0, 3.0])));
    assert!(!board.has_changes(), "defaults are not changes");
    assert_eq!(
        board.set("hp", BbValue::Int(10)),
        Ok(false),
        "equal write: no notification"
    );
    assert_eq!(board.set("hp", BbValue::Int(8)), Ok(true));
    assert_eq!(
        board.set("speed", BbValue::Int(3)),
        Ok(true),
        "int converts to a float key"
    );
    assert_eq!(board.get("speed"), Some(&BbValue::Float(3.0)));
    assert_eq!(board.set_json("name", &json!("guard")), Ok(true));
    assert_eq!(board.set_json("target", &json!("player")), Ok(true));
    assert_eq!(board.set_json("alert", &json!(true)), Ok(true));
    assert!(matches!(
        board.set("hp", BbValue::Bool(true)),
        Err(BlackboardError::TypeMismatch { .. })
    ));
    assert!(matches!(
        board.set("ghost", BbValue::Int(1)),
        Err(BlackboardError::UnknownKey(_))
    ));
    assert!(board.set_json("pos", &json!([1.0, 2.0])).is_err());
    assert!(board.set_json("hp", &json!(1.5)).is_err());
    assert_eq!(board.clear("alert"), Ok(true));
    assert_eq!(board.clear("alert"), Ok(false));
    let changed: Vec<_> = board.take_changes().into_iter().collect();
    assert_eq!(
        changed,
        ["alert", "hp", "name", "speed", "target"],
        "sorted, deduplicated"
    );
    assert!(!board.has_changes());
    let summary = board.summary(2, 3);
    assert_eq!(summary.len(), 2, "summary is capped");
    board.set_json("name", &json!("abcdefgh")).unwrap();
    assert_eq!(
        board.summary(10, 3)["name"],
        BbValue::String("abc".into()),
        "strings are cut"
    );
    assert_eq!(
        serde_json::to_value(board.summary(10, 8)["pos"].clone()).unwrap(),
        json!([1.0, 2.0, 3.0])
    );
}
