use engine_graph::{GraphDocument, GraphEdge, GraphNode, PortRef};
use engine_material_graph::{compile_material, material_kind};
use engine_render_pipeline::{MaterialTarget, compose_material};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn node(id: &str, kind: &str, params: &[(&str, Value)]) -> GraphNode {
    GraphNode {
        id: id.into(),
        node_type: kind.to_owned(),
        ports: vec![],
        params: params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
        position: None,
    }
}
fn edge(from: &str, from_port: &str, to: &str, to_port: &str) -> GraphEdge {
    GraphEdge {
        from: PortRef::new(from, from_port),
        to: PortRef::new(to, to_port),
    }
}
fn graph() -> GraphDocument {
    let mut doc = GraphDocument::new("material");
    doc.nodes = vec![
        node("uv", "uv", &[]),
        node("texture", "texture_color", &[]),
        node("tint", "tint", &[]),
        node("mul", "multiply_vec3", &[]),
        node("alpha_mul", "multiply_float", &[]),
        node("output", "material_output", &[]),
    ];
    doc.edges = vec![
        edge("uv", "value", "texture", "uv"),
        edge("texture", "color", "mul", "a"),
        edge("tint", "color", "mul", "b"),
        edge("texture", "alpha", "alpha_mul", "a"),
        edge("tint", "alpha", "alpha_mul", "b"),
        edge("mul", "value", "output", "base_color"),
        edge("alpha_mul", "value", "output", "alpha"),
    ];
    doc
}

#[test]
fn compiled_graph_satisfies_mesh_and_sprite_material_contracts() {
    let compiled = compile_material(&graph()).unwrap();
    for target in [MaterialTarget::Mesh, MaterialTarget::Sprite] {
        let composed = compose_material(&compiled.source, target).unwrap();
        assert!(composed.contains("fn graph_safe_float"));
        assert!(composed.contains("surface.albedo = graph_base_color"));
    }
    assert!(!compiled.source.contains("@group("));
    assert!(!compiled.source.contains("#import game::"));
}

#[test]
fn node_and_edge_array_order_do_not_change_compiled_source() {
    let doc = graph();
    let expected = compile_material(&doc).unwrap().source;
    let mut permuted = doc.clone();
    permuted.nodes.reverse();
    permuted.edges.reverse();
    assert_eq!(compile_material(&permuted).unwrap().source, expected);
}

#[test]
fn malformed_types_cycles_disconnected_and_nonfinite_constants_fail() {
    let mut doc = graph();
    doc.nodes
        .push(node("unused", "scalar", &[("value", json!(0.5))]));
    assert!(
        compile_material(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.code == "material.node.disconnected")
    );

    let mut doc = graph();
    doc.edges.push(edge("uv", "value", "output", "alpha"));
    assert!(
        compile_material(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.code == "graph.edge.type_mismatch")
    );

    let mut doc = graph();
    doc.edges.push(edge("mul", "value", "mul", "a"));
    assert!(
        compile_material(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.code == "graph.cycle")
    );

    let mut doc = GraphDocument::new("material");
    doc.nodes = vec![
        node("high", "scalar", &[("value", json!(1e300))]),
        node("output", "material_output", &[]),
    ];
    doc.edges.push(edge("high", "value", "output", "alpha"));
    assert!(
        compile_material(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.code == "material.param.nonfinite")
    );
}

#[test]
fn one_output_and_size_limits_are_enforced() {
    assert!(material_kind().registry.get("material_output").is_some());
    let mut doc = GraphDocument::new("material");
    assert!(
        compile_material(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.code == "material.output.missing")
    );
    doc.nodes.push(node("a", "material_output", &[]));
    doc.nodes.push(node("b", "material_output", &[]));
    assert!(
        compile_material(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.code == "material.output.multiple")
    );
    doc.nodes
        .extend((0..64).map(|i| node(&format!("n{i}"), "tint", &[])));
    assert!(
        compile_material(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.code == "graph.limit.nodes")
    );
}

#[test]
fn no_raw_shader_text_or_user_bindings_are_accepted() {
    let mut doc = graph();
    doc.nodes[0].params = BTreeMap::from([("wgsl".to_owned(), json!("@group(7)"))]);
    assert!(
        compile_material(&doc)
            .unwrap_err()
            .iter()
            .any(|e| e.code == "graph.param.unknown")
    );
}

#[test]
fn unlit_path_uses_graph_base_color_instead_of_builtin_sample() {
    let compiled = compile_material(&graph()).unwrap();
    assert!(
        compiled
            .source
            .contains("select(shaded, graph_base_color, material.options.x > 0.5)")
    );
    assert!(
        !compiled
            .source
            .contains("select(shaded, color.rgb, material.options.x > 0.5)")
    );
}
