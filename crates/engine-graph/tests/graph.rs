use engine_graph::{
    GraphDiagnostic, GraphDocument, GraphEdit, GraphKind, GraphLimits, GraphVariable, NodeId,
    NodeTypeDef, NodeTypeRegistry, ParamDef, ParamType, PortDef, PortRef, PortType, Strictness,
    apply_edit, apply_edits, codes, validate,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn kind() -> GraphKind {
    let registry = NodeTypeRegistry::new()
        .with(
            NodeTypeDef::new("start", "control", "Entry")
                .root()
                .port(PortDef::output("next", PortType::Flow)),
        )
        .with(
            NodeTypeDef::new("step", "task", "A step")
                .port(PortDef::input("prev", PortType::Flow).required())
                .port(PortDef::output("next", PortType::Flow))
                .param(ParamDef::new("count", ParamType::Int).range(1.0, 10.0))
                .param(ParamDef::new("ratio", ParamType::Float).range(0.0, 1.0))
                .param(ParamDef::new("label", ParamType::String).max_len(8))
                .param(ParamDef::new("mode", ParamType::Enum).choices(&["a", "b"]))
                .param(ParamDef::new("flag", ParamType::Bool))
                .param(ParamDef::new("pos", ParamType::Vec3))
                .param(ParamDef::new("data", ParamType::Json))
                .param(ParamDef::new("var", ParamType::Variable).variable_types(&[PortType::Int])),
        )
        .with(
            NodeTypeDef::new("needs", "task", "Has a required param")
                .port(PortDef::input("prev", PortType::Flow))
                .param(ParamDef::new("must", ParamType::Int).required())
                .param(
                    ParamDef::new("with_default", ParamType::Int)
                        .required()
                        .default_value(json!(3)),
                ),
        )
        .with(
            NodeTypeDef::new("join", "control", "Many in")
                .port(PortDef::input("prev", PortType::Flow).multiple())
                .port(PortDef::output("next", PortType::Flow)),
        )
        .with(
            NodeTypeDef::new("source", "data", "Data out")
                .port(PortDef::input("prev", PortType::Flow))
                .port(PortDef::output("int", PortType::Int).multiple())
                .port(PortDef::output("text", PortType::String)),
        )
        .with(
            NodeTypeDef::new("sink", "data", "Data in")
                .port(PortDef::input("prev", PortType::Flow))
                .port(PortDef::input("float", PortType::Float))
                .port(PortDef::input("any", PortType::Any)),
        )
        .with(
            NodeTypeDef::new("open", "control", "Extensible")
                .extensible()
                .port(PortDef::input("prev", PortType::Flow)),
        );
    GraphKind::new("test_flow", "Test kind", registry)
        .single_root()
        .variable_types(&[PortType::Int, PortType::Float, PortType::Vec3])
        .check(|doc| {
            if doc.metadata.contains_key("kind_check_fails") {
                vec![GraphDiagnostic::new(
                    "test.kind_check",
                    "",
                    "kind check ran",
                )]
            } else {
                Vec::new()
            }
        })
}

fn node(id: &str, node_type: &str) -> Value {
    json!({"id": id, "node_type": node_type})
}
fn edge(a: &str, ap: &str, b: &str, bp: &str) -> Value {
    json!({"from":{"node":a,"port":ap},"to":{"node":b,"port":bp}})
}
fn doc(nodes: Vec<Value>, edges: Vec<Value>) -> GraphDocument {
    GraphDocument::from_value(json!({"version":1,"kind":"test_flow","nodes":nodes,"edges":edges}))
        .expect("test document parses")
}
/// start -> a -> b
fn chain() -> GraphDocument {
    doc(
        vec![node("s", "start"), node("a", "step"), node("b", "step")],
        vec![
            edge("s", "next", "a", "prev"),
            edge("a", "next", "b", "prev"),
        ],
    )
}
fn codes_of(doc: &GraphDocument, level: Strictness) -> Vec<String> {
    validate(doc, &kind(), level)
        .into_iter()
        .map(|d| d.code)
        .collect()
}
fn with_params(mut document: GraphDocument, node: &str, params: Value) -> GraphDocument {
    let n = document
        .nodes
        .iter_mut()
        .find(|n| n.id.as_str() == node)
        .unwrap();
    n.params = serde_json::from_value(params).unwrap();
    document
}

#[test]
fn a_valid_chain_passes_full_validation_and_round_trips() {
    let document = chain();
    assert_eq!(validate(&document, &kind(), Strictness::Complete), vec![]);
    let text = document.to_json_pretty();
    let again = GraphDocument::from_json(text.as_bytes()).unwrap();
    assert_eq!(again, document);
    assert_eq!(again.to_json_pretty(), text);
}

#[test]
fn round_trip_keeps_every_field_kind() {
    let mut document = chain();
    document
        .metadata
        .insert("author".into(), json!({"name": "x", "n": [1, 2.5, null]}));
    document.variables.push(GraphVariable {
        name: "hp".into(),
        ty: PortType::Int,
        default: Some(json!(7)),
        description: "health".into(),
    });
    document.variables.push(GraphVariable {
        name: "pos".into(),
        ty: PortType::Vec3,
        default: None,
        description: String::new(),
    });
    document.nodes[1].position = Some([1.5, -2.0]);
    document.nodes[1].params.insert("count".into(), json!(3));
    document.nodes[1].ports = chain_ports();
    let text = document.to_json_pretty();
    let again = GraphDocument::from_json(text.as_bytes()).unwrap();
    assert_eq!(again, document);
    assert!(validate(&again, &kind(), Strictness::Complete).is_empty());
    // Custom port types survive too.
    let custom = serde_json::to_value(PortType::Custom("x.y".into())).unwrap();
    assert_eq!(custom, json!({"custom": "x.y"}));
    assert_eq!(
        serde_json::from_value::<PortType>(custom).unwrap(),
        PortType::Custom("x.y".into())
    );
}

fn chain_ports() -> Vec<engine_graph::Port> {
    kind().registry.get("step").unwrap().declared_ports()
}

#[test]
fn unknown_fields_are_rejected_by_the_parser() {
    let text = json!({"version":1,"kind":"test_flow","surprise":true}).to_string();
    let error = GraphDocument::from_json(text.as_bytes()).unwrap_err();
    assert_eq!(error.code, codes::DOCUMENT_MALFORMED);
    let nested =
        json!({"version":1,"kind":"test_flow","nodes":[{"id":"a","node_type":"x","extra":1}]});
    assert!(GraphDocument::from_value(nested).is_err());
}

/// One focused negative test per diagnostic code.
fn negative_cases() -> Vec<(&'static str, GraphDocument)> {
    let mut cases: Vec<(&'static str, GraphDocument)> = Vec::new();
    let mut v = chain();
    v.version = 2;
    cases.push((codes::VERSION_UNSUPPORTED, v));
    let mut k = chain();
    k.kind = "other".into();
    cases.push((codes::KIND_MISMATCH, k));
    cases.push((
        codes::NODE_ID_INVALID,
        doc(vec![node("s", "start"), node("bad id!", "step")], vec![]),
    ));
    cases.push((
        codes::NODE_DUPLICATE,
        doc(vec![node("s", "start"), node("s", "step")], vec![]),
    ));
    cases.push((
        codes::NODE_UNKNOWN_TYPE,
        doc(vec![node("s", "start"), node("q", "missing_type")], vec![]),
    ));
    let mut mismatch = chain();
    mismatch.nodes[1].ports = vec![engine_graph::Port {
        name: "prev".into(),
        direction: engine_graph::PortDirection::In,
        ty: PortType::Bool,
        multiple: false,
    }];
    cases.push((codes::NODE_PORTS_MISMATCH, mismatch));
    let mut extra_port = chain();
    let mut ports = chain_ports();
    ports.push(engine_graph::Port {
        name: "ghost".into(),
        direction: engine_graph::PortDirection::Out,
        ty: PortType::Flow,
        multiple: false,
    });
    extra_port.nodes[1].ports = ports;
    cases.push((codes::NODE_PORTS_MISMATCH, extra_port));
    let mut position = chain();
    position.nodes[1].position = Some([f32::NAN, 0.0]);
    cases.push((codes::NODE_POSITION_INVALID, position));
    let mut duplicate_port = chain();
    let mut ports = chain_ports();
    ports.push(ports[0].clone());
    duplicate_port.nodes[1].ports = ports;
    cases.push((codes::PORT_DUPLICATE, duplicate_port));
    let mut many_ports = chain();
    many_ports.nodes[1].ports = (0..65)
        .map(|i| engine_graph::Port {
            name: format!("p{i}"),
            direction: engine_graph::PortDirection::In,
            ty: PortType::Flow,
            multiple: false,
        })
        .collect();
    cases.push((codes::LIMIT_PORTS, many_ports));
    let many: serde_json::Map<String, Value> =
        (0..65).map(|i| (format!("p{i}"), json!(1))).collect();
    cases.push((
        codes::LIMIT_PARAMS,
        with_params(chain(), "a", Value::Object(many)),
    ));
    cases.push((
        codes::PARAM_UNKNOWN,
        with_params(chain(), "a", json!({"nope": 1})),
    ));
    cases.push((
        codes::PARAM_TYPE,
        with_params(chain(), "a", json!({"count": "three"})),
    ));
    cases.push((
        codes::PARAM_RANGE,
        with_params(chain(), "a", json!({"count": 99})),
    ));
    cases.push((
        codes::PARAM_RANGE,
        with_params(chain(), "a", json!({"mode": "c"})),
    ));
    cases.push((
        codes::PARAM_RANGE,
        with_params(chain(), "a", json!({"label": "way too long"})),
    ));
    cases.push((
        codes::PARAM_VARIABLE,
        with_params(chain(), "a", json!({"var": "undeclared"})),
    ));
    let mut wrong_var = with_params(chain(), "a", json!({"var": "f"}));
    wrong_var.variables.push(GraphVariable {
        name: "f".into(),
        ty: PortType::Float,
        default: None,
        description: String::new(),
    });
    cases.push((codes::PARAM_VARIABLE, wrong_var));
    cases.push((
        codes::LIMIT_VALUE,
        with_params(
            chain(),
            "a",
            json!({"data": {"a":{"b":{"c":{"d":{"e":{"f":{"g":{"h":1}}}}}}}}}),
        ),
    ));
    cases.push((
        codes::PARAM_MISSING,
        doc(
            vec![node("s", "start"), node("n", "needs")],
            vec![edge("s", "next", "n", "prev")],
        ),
    ));
    cases.push((
        codes::EDGE_UNKNOWN_NODE,
        doc(
            vec![node("s", "start")],
            vec![edge("s", "next", "ghost", "prev")],
        ),
    ));
    cases.push((
        codes::EDGE_UNKNOWN_PORT,
        doc(
            vec![node("s", "start"), node("a", "step")],
            vec![edge("s", "nope", "a", "prev")],
        ),
    ));
    cases.push((
        codes::EDGE_DIRECTION,
        doc(
            vec![node("s", "start"), node("a", "step")],
            vec![edge("a", "prev", "s", "next")],
        ),
    ));
    cases.push((
        codes::EDGE_TYPE_MISMATCH,
        doc(
            vec![node("s", "start"), node("src", "source"), node("k", "sink")],
            vec![
                edge("s", "next", "src", "prev"),
                edge("src", "text", "k", "float"),
            ],
        ),
    ));
    cases.push((
        codes::EDGE_DUPLICATE,
        doc(
            vec![node("s", "start"), node("a", "step")],
            vec![
                edge("s", "next", "a", "prev"),
                edge("s", "next", "a", "prev"),
            ],
        ),
    ));
    cases.push((
        codes::EDGE_MULTIPLE,
        doc(
            vec![node("s", "start"), node("a", "step"), node("b", "step")],
            vec![
                edge("s", "next", "a", "prev"),
                edge("s", "next", "b", "prev"),
            ],
        ),
    ));
    cases.push((
        codes::CYCLE,
        doc(
            vec![
                node("s", "start"),
                node("a", "step"),
                node("b", "step"),
                node("c", "step"),
            ],
            vec![
                edge("s", "next", "a", "prev"),
                edge("a", "next", "b", "prev"),
                edge("b", "next", "c", "prev"),
                edge("c", "next", "a", "prev"),
            ],
        ),
    ));
    cases.push((
        codes::PORT_UNCONNECTED,
        doc(vec![node("s", "start"), node("a", "step")], vec![]),
    ));
    cases.push((codes::ROOT_MISSING, doc(vec![node("a", "join")], vec![])));
    cases.push((
        codes::ROOT_MULTIPLE,
        doc(vec![node("s", "start"), node("t", "start")], vec![]),
    ));
    cases.push((
        codes::NODE_UNREACHABLE,
        doc(vec![node("s", "start"), node("j", "join")], vec![]),
    ));
    let mut bad_variable = chain();
    bad_variable.variables.push(GraphVariable {
        name: "no spaces".into(),
        ty: PortType::Int,
        default: None,
        description: String::new(),
    });
    cases.push((codes::VARIABLE_NAME_INVALID, bad_variable));
    let mut dup_variable = chain();
    for _ in 0..2 {
        dup_variable.variables.push(GraphVariable {
            name: "hp".into(),
            ty: PortType::Int,
            default: None,
            description: String::new(),
        });
    }
    cases.push((codes::VARIABLE_DUPLICATE, dup_variable));
    let mut wrong_type = chain();
    wrong_type.variables.push(GraphVariable {
        name: "flag".into(),
        ty: PortType::Bool,
        default: None,
        description: String::new(),
    });
    cases.push((codes::VARIABLE_TYPE, wrong_type));
    let mut flow_variable = chain();
    flow_variable.variables.push(GraphVariable {
        name: "f".into(),
        ty: PortType::Flow,
        default: None,
        description: String::new(),
    });
    cases.push((codes::VARIABLE_TYPE, flow_variable));
    let mut bad_default = chain();
    bad_default.variables.push(GraphVariable {
        name: "hp".into(),
        ty: PortType::Int,
        default: Some(json!("full")),
        description: String::new(),
    });
    cases.push((codes::VARIABLE_DEFAULT, bad_default));
    let mut key = chain();
    key.metadata.insert("bad key".into(), json!(1));
    cases.push((codes::METADATA_KEY_INVALID, key));
    let mut metadata = chain();
    for i in 0..65 {
        metadata.metadata.insert(format!("k{i}"), json!(i));
    }
    cases.push((codes::LIMIT_METADATA, metadata));
    let mut hooked = chain();
    hooked
        .metadata
        .insert("kind_check_fails".into(), json!(true));
    cases.push(("test.kind_check", hooked));
    cases
}

#[test]
fn every_code_is_hit_by_a_focused_negative_case() {
    let mut hit = BTreeSet::new();
    for (code, document) in negative_cases() {
        let found = codes_of(&document, Strictness::Complete);
        assert!(
            found.iter().any(|c| c == code),
            "expected {code}, validation returned {found:?}"
        );
        hit.insert(code);
    }
    // Codes whose trigger is not a single small document are covered below.
    let elsewhere: BTreeSet<&str> = [
        codes::DOCUMENT_MALFORMED,
        codes::DOCUMENT_TOO_LARGE,
        codes::DIAGNOSTICS_TRUNCATED,
        codes::LIMIT_NODES,
        codes::LIMIT_EDGES,
        codes::LIMIT_VARIABLES,
        codes::LIMIT_EDITS,
        codes::EDIT_NODE_MISSING,
        codes::EDIT_EDGE_MISSING,
        codes::EDIT_VARIABLE_MISSING,
        codes::EDIT_INDEX,
    ]
    .into_iter()
    .collect();
    for code in codes::ALL {
        assert!(
            hit.contains(code) || elsewhere.contains(code),
            "no negative test for {code}"
        );
    }
}

#[test]
fn structure_level_ignores_completeness_rules() {
    let incomplete = doc(vec![node("s", "start"), node("a", "step")], vec![]);
    assert!(validate(&incomplete, &kind(), Strictness::Structure).is_empty());
    assert!(
        codes_of(&incomplete, Strictness::Complete).contains(&codes::PORT_UNCONNECTED.to_owned())
    );
    let needs = doc(
        vec![node("s", "start"), node("n", "needs")],
        vec![edge("s", "next", "n", "prev")],
    );
    assert!(validate(&needs, &kind(), Strictness::Structure).is_empty());
}

#[test]
fn a_param_default_satisfies_a_required_param() {
    let ok = with_params(
        doc(
            vec![node("s", "start"), node("n", "needs")],
            vec![edge("s", "next", "n", "prev")],
        ),
        "n",
        json!({"must": 1}),
    );
    assert!(
        validate(&ok, &kind(), Strictness::Complete).is_empty(),
        "with_default has a default"
    );
}

#[test]
fn size_limits_are_enforced_before_deeper_work() {
    let small = kind().limits(GraphLimits {
        max_nodes: 2,
        max_edges: 1,
        max_variables: 0,
    });
    let mut document = chain();
    document.variables.push(GraphVariable {
        name: "hp".into(),
        ty: PortType::Int,
        default: None,
        description: String::new(),
    });
    let found: Vec<_> = validate(&document, &small, Strictness::Complete)
        .into_iter()
        .map(|d| d.code)
        .collect();
    for code in [
        codes::LIMIT_NODES,
        codes::LIMIT_EDGES,
        codes::LIMIT_VARIABLES,
    ] {
        assert!(found.iter().any(|c| c == code), "{code} in {found:?}");
    }
    // The hard ceiling applies even to a kind that asks for more.
    let greedy = kind().limits(GraphLimits {
        max_nodes: usize::MAX,
        max_edges: usize::MAX,
        max_variables: usize::MAX,
    });
    let nodes: Vec<Value> = (0..4097).map(|i| node(&format!("n{i}"), "join")).collect();
    let big = doc(nodes, vec![]);
    assert!(
        validate(&big, &greedy, Strictness::Structure)
            .iter()
            .any(|d| d.code == codes::LIMIT_NODES)
    );
}

#[test]
fn parser_limits_and_truncation() {
    assert_eq!(
        GraphDocument::from_json(b"{not json").unwrap_err().code,
        codes::DOCUMENT_MALFORMED
    );
    let huge = vec![b' '; engine_graph::MAX_DOCUMENT_BYTES + 1];
    assert_eq!(
        GraphDocument::from_json(&huge).unwrap_err().code,
        codes::DOCUMENT_TOO_LARGE
    );
    let nodes: Vec<Value> = (0..200).map(|i| node(&format!("n{i}"), "ghost")).collect();
    let document = doc(nodes, vec![]);
    let found = validate(&document, &kind(), Strictness::Structure);
    assert_eq!(found.len(), engine_graph::MAX_DIAGNOSTICS);
    assert_eq!(found.last().unwrap().code, codes::DIAGNOSTICS_TRUNCATED);
}

#[test]
fn validation_output_is_deterministic() {
    let mut messy = chain();
    messy.nodes[1].params =
        serde_json::from_value(json!({"count": 99, "ratio": 5, "label": 3})).unwrap();
    messy.variables.push(GraphVariable {
        name: "x y".into(),
        ty: PortType::Int,
        default: None,
        description: String::new(),
    });
    let first = validate(&messy, &kind(), Strictness::Complete);
    for _ in 0..5 {
        assert_eq!(validate(&messy, &kind(), Strictness::Complete), first);
    }
    assert!(first.len() >= 4);
}

#[test]
fn type_rules_allow_widening_any_and_reject_flow_mixing() {
    let ok = doc(
        vec![node("s", "start"), node("src", "source"), node("k", "sink")],
        vec![
            edge("s", "next", "src", "prev"),
            edge("s", "next", "k", "prev"),
            edge("src", "int", "k", "float"),
            edge("src", "text", "k", "any"),
        ],
    );
    let found = codes_of(&ok, Strictness::Structure);
    assert_eq!(
        found,
        vec![codes::EDGE_MULTIPLE.to_owned()],
        "int widens to float and text feeds any"
    );
    assert!(PortType::Any.accepts(&PortType::Vec3));
    assert!(!PortType::Any.accepts(&PortType::Flow));
    assert!(!PortType::Float.accepts(&PortType::String));
    assert!(PortType::Custom("a".into()).accepts(&PortType::Custom("a".into())));
    assert!(!PortType::Custom("a".into()).accepts(&PortType::Custom("b".into())));
}

#[test]
fn extensible_nodes_accept_extra_ports() {
    let mut document = doc(
        vec![node("s", "start"), node("o", "open")],
        vec![edge("s", "next", "o", "prev")],
    );
    let mut ports = kind().registry.get("open").unwrap().declared_ports();
    ports.push(engine_graph::Port {
        name: "extra".into(),
        direction: engine_graph::PortDirection::In,
        ty: PortType::Flow,
        multiple: false,
    });
    document.nodes[1].ports = ports;
    assert!(validate(&document, &kind(), Strictness::Complete).is_empty());
}

fn edit(value: Value) -> GraphEdit {
    serde_json::from_value(value).expect("edit parses")
}

#[test]
fn edits_build_a_valid_graph_with_ordered_connections() {
    let mut document = GraphDocument::new("test_flow");
    let kind = kind();
    let edits = vec![
        edit(json!({"op":"add_node","id":"s","node_type":"start"})),
        edit(json!({"op":"add_node","id":"j","node_type":"join","position":[10.0,20.0]})),
        edit(json!({"op":"add_node","id":"a","node_type":"step","params":{"count":2}})),
        edit(json!({"op":"add_node","id":"b","node_type":"step"})),
        edit(
            json!({"op":"connect","from":{"node":"s","port":"next"},"to":{"node":"j","port":"prev"}}),
        ),
        edit(
            json!({"op":"connect","from":{"node":"j","port":"next"},"to":{"node":"b","port":"prev"}}),
        ),
        edit(json!({"op":"set_param","node":"b","name":"label","value":"hi"})),
        edit(json!({"op":"set_position","node":"b","position":[1.0,2.0]})),
        edit(json!({"op":"add_variable","variable":{"name":"hp","type":"int","default":3}})),
        edit(json!({"op":"set_metadata","key":"author","value":"me"})),
    ];
    // "a" is unreachable and unconnected, so full validation rejects the batch...
    let failure = apply_edits(&mut document, &edits, &kind, Strictness::Complete).unwrap_err();
    assert_eq!(failure.op_index, None);
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.code == codes::NODE_UNREACHABLE)
    );
    assert_eq!(
        document,
        GraphDocument::new("test_flow"),
        "failed batch leaves the document untouched"
    );
    // ...while a structure-level save accepts work in progress.
    apply_edits(&mut document, &edits, &kind, Strictness::Structure).unwrap();
    assert_eq!(document.nodes.len(), 4);
    assert_eq!(document.nodes[1].position, Some([10.0, 20.0]));
    assert_eq!(
        document.nodes[2].ports.len(),
        2,
        "ports are copied from the registry"
    );
    let fixes = vec![
        edit(json!({"op":"remove_node","id":"a"})),
        edit(json!({"op":"set_param","node":"b","name":"label","value":null})),
        edit(json!({"op":"set_variable","variable":{"name":"hp","type":"int","default":5}})),
        edit(json!({"op":"remove_variable","name":"hp"})),
        edit(json!({"op":"set_metadata","key":"author","value":null})),
    ];
    apply_edits(&mut document, &fixes, &kind, Strictness::Complete).unwrap();
    assert!(document.node("a").is_none());
    assert!(document.node("b").unwrap().params.is_empty());
    assert!(document.variables.is_empty() && document.metadata.is_empty());
    assert!(validate(&document, &kind, Strictness::Complete).is_empty());
}

#[test]
fn connect_index_controls_child_order() {
    let kind = kind();
    let mut multi = GraphDocument::new("test_flow");
    let make = vec![
        edit(json!({"op":"add_node","id":"j","node_type":"join"})),
        edit(json!({"op":"add_node","id":"p","node_type":"source"})),
        edit(json!({"op":"add_node","id":"q","node_type":"sink"})),
        edit(json!({"op":"add_node","id":"r","node_type":"sink"})),
    ];
    apply_edits(&mut multi, &make, &kind, Strictness::Structure).unwrap();
    for target in ["q", "r"] {
        let c = edit(
            json!({"op":"connect","from":{"node":"p","port":"int"},"to":{"node":target,"port":"any"}}),
        );
        apply_edit(&mut multi, &c, &kind).unwrap();
    }
    let first = edit(
        json!({"op":"connect","from":{"node":"p","port":"int"},"to":{"node":"q","port":"float"},"index":0}),
    );
    apply_edit(&mut multi, &first, &kind).unwrap();
    let order: Vec<_> = multi
        .edges
        .iter()
        .map(|e| format!("{}.{}", e.to.node, e.to.port))
        .collect();
    assert_eq!(order, vec!["q.float", "q.any", "r.any"]);
    let beyond = edit(
        json!({"op":"connect","from":{"node":"p","port":"int"},"to":{"node":"r","port":"float"},"index":9}),
    );
    let before = multi.clone();
    let error = apply_edit(&mut multi, &beyond, &kind).unwrap_err();
    assert_eq!(error[0].code, codes::EDIT_INDEX);
    assert_eq!(multi, before);
    apply_edit(
        &mut multi,
        &edit(json!({"op":"disconnect","from":{"node":"p","port":"int"},"to":{"node":"q","port":"any"}})),
        &kind,
    )
    .unwrap();
    assert_eq!(multi.edges.len(), 2);
}

#[test]
fn invalid_edits_are_rejected_without_mutation() {
    let kind = kind();
    let bad = [
        (
            json!({"op":"add_node","id":"a","node_type":"step"}),
            codes::NODE_DUPLICATE,
        ),
        (
            json!({"op":"add_node","id":"z","node_type":"nonsense"}),
            codes::NODE_UNKNOWN_TYPE,
        ),
        (
            json!({"op":"add_node","id":"bad id","node_type":"step"}),
            codes::NODE_ID_INVALID,
        ),
        (
            json!({"op":"add_node","id":"z","node_type":"step","params":{"count":99}}),
            codes::PARAM_RANGE,
        ),
        (
            json!({"op":"remove_node","id":"ghost"}),
            codes::EDIT_NODE_MISSING,
        ),
        (
            json!({"op":"connect","from":{"node":"a","port":"next"},"to":{"node":"ghost","port":"prev"}}),
            codes::EDGE_UNKNOWN_NODE,
        ),
        (
            json!({"op":"connect","from":{"node":"a","port":"prev"},"to":{"node":"b","port":"prev"}}),
            codes::EDGE_DIRECTION,
        ),
        (
            json!({"op":"connect","from":{"node":"a","port":"next"},"to":{"node":"b","port":"prev"}}),
            codes::EDGE_DUPLICATE,
        ),
        (
            json!({"op":"connect","from":{"node":"s","port":"next"},"to":{"node":"b","port":"prev"}}),
            codes::EDGE_MULTIPLE,
        ),
        (
            json!({"op":"connect","from":{"node":"b","port":"next"},"to":{"node":"a","port":"prev"}}),
            codes::CYCLE,
        ),
        (
            json!({"op":"disconnect","from":{"node":"s","port":"next"},"to":{"node":"b","port":"prev"}}),
            codes::EDIT_EDGE_MISSING,
        ),
        (
            json!({"op":"set_param","node":"ghost","name":"count","value":1}),
            codes::EDIT_NODE_MISSING,
        ),
        (
            json!({"op":"set_param","node":"a","name":"count","value":"x"}),
            codes::PARAM_TYPE,
        ),
        (
            json!({"op":"set_param","node":"a","name":"unknown","value":1}),
            codes::PARAM_UNKNOWN,
        ),
        (
            json!({"op":"set_position","node":"a","position":[1e30,0.0]}),
            codes::NODE_POSITION_INVALID,
        ),
        (
            json!({"op":"set_position","node":"ghost","position":null}),
            codes::EDIT_NODE_MISSING,
        ),
        (
            json!({"op":"add_variable","variable":{"name":"x y","type":"int"}}),
            codes::VARIABLE_NAME_INVALID,
        ),
        (
            json!({"op":"add_variable","variable":{"name":"hp","type":"int"}}),
            codes::VARIABLE_DUPLICATE,
        ),
        (
            json!({"op":"add_variable","variable":{"name":"b","type":"bool"}}),
            codes::VARIABLE_TYPE,
        ),
        (
            json!({"op":"add_variable","variable":{"name":"c","type":"int","default":"x"}}),
            codes::VARIABLE_DEFAULT,
        ),
        (
            json!({"op":"remove_variable","name":"ghost"}),
            codes::EDIT_VARIABLE_MISSING,
        ),
        (
            json!({"op":"remove_variable","name":"hp"}),
            codes::PARAM_VARIABLE,
        ),
        (
            json!({"op":"set_variable","variable":{"name":"hp","type":"float"}}),
            codes::PARAM_VARIABLE,
        ),
        (
            json!({"op":"set_variable","variable":{"name":"ghost","type":"int"}}),
            codes::EDIT_VARIABLE_MISSING,
        ),
        (
            json!({"op":"set_metadata","key":"bad key","value":1}),
            codes::METADATA_KEY_INVALID,
        ),
    ];
    for (op, expected) in bad {
        let mut document = chain();
        document.variables.push(GraphVariable {
            name: "hp".into(),
            ty: PortType::Int,
            default: None,
            description: String::new(),
        });
        document.nodes[1].params.insert("var".into(), json!("hp"));
        let before = document.clone();
        let error = apply_edit(&mut document, &edit(op.clone()), &kind)
            .expect_err(&format!("{op} should be rejected"));
        assert!(
            error.iter().any(|d| d.code == expected),
            "{op}: expected {expected}, got {error:?}"
        );
        assert_eq!(document, before, "{op} mutated the document");
    }
}

#[test]
fn a_failing_batch_reports_the_operation_and_changes_nothing() {
    let kind = kind();
    let mut document = chain();
    let before = document.clone();
    let edits = vec![
        edit(json!({"op":"add_node","id":"c","node_type":"step"})),
        edit(json!({"op":"remove_node","id":"ghost"})),
    ];
    let failure = apply_edits(&mut document, &edits, &kind, Strictness::Structure).unwrap_err();
    assert_eq!(failure.op_index, Some(1));
    assert_eq!(failure.diagnostics[0].code, codes::EDIT_NODE_MISSING);
    assert_eq!(document, before);
    let too_many: Vec<_> = (0..engine_graph::MAX_EDITS + 1)
        .map(|_| edit(json!({"op":"set_metadata","key":"k","value":1})))
        .collect();
    let failure = apply_edits(&mut document, &too_many, &kind, Strictness::Structure).unwrap_err();
    assert_eq!(failure.diagnostics[0].code, codes::LIMIT_EDITS);
    assert_eq!(document, before);
}

#[test]
fn edits_may_repair_a_document_that_was_already_invalid() {
    let kind = kind();
    let mut document = doc(
        vec![
            node("s", "start"),
            node("a", "step"),
            node("junk", "missing_type"),
        ],
        vec![edge("s", "next", "a", "prev")],
    );
    // An unrelated edit is fine; only newly introduced problems are rejected.
    apply_edit(
        &mut document,
        &edit(json!({"op":"set_metadata","key":"k","value":1})),
        &kind,
    )
    .unwrap();
    apply_edit(
        &mut document,
        &edit(json!({"op":"remove_node","id":"junk"})),
        &kind,
    )
    .unwrap();
    assert!(validate(&document, &kind, Strictness::Complete).is_empty());
}

#[test]
fn removing_a_node_removes_its_edges() {
    let kind = kind();
    let mut document = chain();
    apply_edit(
        &mut document,
        &edit(json!({"op":"remove_node","id":"a"})),
        &kind,
    )
    .unwrap();
    assert!(document.edges.is_empty());
    assert_eq!(
        document
            .nodes
            .iter()
            .map(|n| n.id.clone())
            .collect::<Vec<NodeId>>(),
        vec![NodeId::from("s"), NodeId::from("b")]
    );
}

#[test]
fn registry_rejects_duplicate_declarations_and_describes_itself() {
    let mut registry = NodeTypeRegistry::new();
    registry.register(NodeTypeDef::new("a", "c", "d")).unwrap();
    assert!(registry.register(NodeTypeDef::new("a", "c", "d")).is_err());
    let repeated = NodeTypeDef::new("b", "c", "d")
        .port(PortDef::input("p", PortType::Flow))
        .port(PortDef::input("p", PortType::Flow));
    assert!(registry.register(repeated).is_err());
    let info = serde_json::to_value(kind().info()).unwrap();
    assert_eq!(info["name"], "test_flow");
    let types = info["node_types"].as_array().unwrap();
    let names: Vec<_> = types.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "node types are listed by name");
    let step = types.iter().find(|t| t["name"] == "step").unwrap();
    assert_eq!(step["ports"][0]["name"], "prev");
    assert_eq!(step["ports"][0]["required"], true);
    assert_eq!(step["params"][0]["type"], "int");
    assert_eq!(step["params"][0]["min"], 1.0);
}

#[test]
fn effective_params_merge_defaults_under_authored_values() {
    let kind = kind();
    let def = kind.registry.get("needs").unwrap();
    let mut document = chain();
    document.nodes[1].node_type = "needs".into();
    document.nodes[1].params.insert("must".into(), json!(9));
    let params = def.effective_params(&document.nodes[1]);
    assert_eq!(params["must"], 9);
    assert_eq!(params["with_default"], 3);
}

#[test]
fn port_refs_serialize_with_plain_node_ids() {
    let edge = serde_json::to_value(engine_graph::GraphEdge {
        from: PortRef::new("a", "out"),
        to: PortRef::new("b", "in"),
    })
    .unwrap();
    assert_eq!(
        edge,
        json!({"from":{"node":"a","port":"out"},"to":{"node":"b","port":"in"}})
    );
}

#[test]
fn extensible_nodes_take_extra_ports_through_edits() {
    let kind = kind();
    let mut document = GraphDocument::new("test_flow");
    let extra = json!({"name":"extra","direction":"in","type":"flow"});
    let edits = vec![
        edit(json!({"op":"add_node","id":"s","node_type":"start"})),
        edit(json!({"op":"add_node","id":"o","node_type":"open","ports":[extra.clone()]})),
        edit(
            json!({"op":"connect","from":{"node":"s","port":"next"},"to":{"node":"o","port":"extra"}}),
        ),
    ];
    apply_edits(&mut document, &edits, &kind, Strictness::Structure).expect("extra port edit");
    let names: Vec<_> = document.nodes[1]
        .ports
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["prev", "extra"], "declared ports come first");

    // Removing a port that still carries an edge is rejected and changes nothing.
    let before = document.clone();
    let failure = apply_edits(
        &mut document,
        &[edit(json!({"op":"set_ports","node":"o","ports":[]}))],
        &kind,
        Strictness::Structure,
    )
    .unwrap_err();
    assert_eq!(failure.diagnostics[0].code, codes::EDGE_UNKNOWN_PORT);
    assert_eq!(document, before);

    // Replacing the extra ports works once the edge is gone.
    let more = json!({"name":"more","direction":"out","type":"int"});
    apply_edits(
        &mut document,
        &[
            edit(json!({"op":"disconnect","from":{"node":"s","port":"next"},"to":{"node":"o","port":"extra"}})),
            edit(json!({"op":"set_ports","node":"o","ports":[more]})),
        ],
        &kind,
        Strictness::Structure,
    )
    .expect("set_ports");
    let names: Vec<_> = document.nodes[1]
        .ports
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["prev", "more"]);
}

#[test]
fn non_extensible_nodes_reject_extra_ports() {
    let kind = kind();
    let extra = json!({"name":"extra","direction":"in","type":"flow"});
    let mut document = GraphDocument::new("test_flow");
    let failure = apply_edits(
        &mut document,
        &[edit(
            json!({"op":"add_node","id":"s","node_type":"start","ports":[extra.clone()]}),
        )],
        &kind,
        Strictness::Structure,
    )
    .unwrap_err();
    assert_eq!(failure.diagnostics[0].code, codes::NODE_PORTS_MISMATCH);
    apply_edits(
        &mut document,
        &[edit(json!({"op":"add_node","id":"s","node_type":"start"}))],
        &kind,
        Strictness::Structure,
    )
    .unwrap();
    let before = document.clone();
    let failure = apply_edits(
        &mut document,
        &[edit(json!({"op":"set_ports","node":"s","ports":[extra]}))],
        &kind,
        Strictness::Structure,
    )
    .unwrap_err();
    assert_eq!(failure.diagnostics[0].code, codes::NODE_PORTS_MISMATCH);
    assert_eq!(document, before);
}
