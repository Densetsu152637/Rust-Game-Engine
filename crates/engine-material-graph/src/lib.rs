//! Bounded, deterministic material graph compilation to the engine shader contract.
use engine_graph::{
    GraphDiagnostic, GraphDocument, GraphIndex, GraphKind, GraphLimits, GraphNode, NodeTypeDef,
    NodeTypeRegistry, ParamDef, ParamType, PortDef, PortType, Strictness, validate,
};
use engine_render_pipeline::shader::LIT_WGSL;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const KIND: &str = "material";
pub const MAX_MATERIAL_NODES: usize = 64;
pub const MAX_MATERIAL_EDGES: usize = 128;
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;
const SAFE_FUNCTIONS: &str = "fn graph_safe_float(v: f32) -> f32 { let capped = clamp(v, -65504.0, 65504.0); return select(0.0, capped, capped == capped); }\nfn graph_safe_vec3(v: vec3<f32>) -> vec3<f32> { return vec3<f32>(graph_safe_float(v.x), graph_safe_float(v.y), graph_safe_float(v.z)); }\nfn graph_safe_color(v: vec3<f32>) -> vec3<f32> { return clamp(graph_safe_vec3(v), vec3<f32>(0.0), vec3<f32>(65504.0)); }\n";
const VEC2: &str = "material_vec2";

fn input(name: &str, ty: PortType) -> PortDef {
    PortDef::input(name, ty).required()
}
fn output(name: &str, ty: PortType) -> PortDef {
    PortDef::output(name, ty).multiple()
}
fn node(name: &str, category: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, category, "Material expression")
}

/// Editor schema and validation rules for authored material graphs.
/// `single_root` is deliberately false: values flow *toward* the output sink.
pub fn material_kind() -> GraphKind {
    let vec2 = PortType::Custom(VEC2.to_owned());
    let mut registry = NodeTypeRegistry::new()
        .with(
            node("scalar", "constant")
                .port(output("value", PortType::Float))
                .param(ParamDef::new("value", ParamType::Float).required()),
        )
        .with(
            node("vec3", "constant")
                .port(output("value", PortType::Vec3))
                .param(ParamDef::new("value", ParamType::Vec3).required()),
        )
        .with(node("uv", "input").port(output("value", vec2.clone())))
        .with(
            node("tint", "input")
                .port(output("color", PortType::Vec3))
                .port(output("alpha", PortType::Float)),
        )
        .with(node("metallic", "input").port(output("value", PortType::Float)))
        .with(node("roughness", "input").port(output("value", PortType::Float)))
        .with(
            node("texture_color", "input")
                .port(PortDef::input("uv", vec2))
                .port(output("color", PortType::Vec3))
                .port(output("alpha", PortType::Float)),
        );
    for (name, ty) in [("float", PortType::Float), ("vec3", PortType::Vec3)] {
        for operation in ["add", "multiply"] {
            registry = registry.with(
                node(&format!("{operation}_{name}"), "math")
                    .port(input("a", ty.clone()))
                    .port(input("b", ty.clone()))
                    .port(output("value", ty.clone())),
            );
        }
    }
    registry = registry
        .with(
            node("mix_vec3", "math")
                .port(input("a", PortType::Vec3))
                .port(input("b", PortType::Vec3))
                .port(input("factor", PortType::Float))
                .port(output("value", PortType::Vec3)),
        )
        .with(
            node("material_output", "output")
                .port(PortDef::input("base_color", PortType::Vec3))
                .port(PortDef::input("alpha", PortType::Float))
                .port(PortDef::input("metallic", PortType::Float))
                .port(PortDef::input("roughness", PortType::Float)),
        );
    GraphKind::new(KIND, "Mesh and sprite material shader", registry)
        .limits(GraphLimits {
            max_nodes: MAX_MATERIAL_NODES,
            max_edges: MAX_MATERIAL_EDGES,
            max_variables: 0,
        })
        .check(check_material)
}

fn error(code: &str, path: impl Into<String>, message: impl Into<String>) -> GraphDiagnostic {
    GraphDiagnostic::new(code, path, message)
}

/// Raw WGSL accepted by `compose_material` for both `Mesh` and `Sprite`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledMaterial {
    pub source: String,
}

/// Validate and compile a complete graph. Diagnostics identify stable graph paths.
pub fn compile_material(
    document: &GraphDocument,
) -> Result<CompiledMaterial, Vec<GraphDiagnostic>> {
    let diagnostics = validate(document, &material_kind(), Strictness::Complete);
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    compile_checked(document).map(|source| CompiledMaterial { source })
}

fn check_material(document: &GraphDocument) -> Vec<GraphDiagnostic> {
    compile_checked(document).err().unwrap_or_default()
}

fn compile_checked(document: &GraphDocument) -> Result<String, Vec<GraphDiagnostic>> {
    let outputs: Vec<_> = document
        .nodes
        .iter()
        .filter(|n| n.node_type == "material_output")
        .collect();
    let sink = match outputs.as_slice() {
        [] => {
            return Err(vec![error(
                "material.output.missing",
                "",
                "one material_output node is required",
            )]);
        }
        [sink] => *sink,
        _ => {
            return Err(vec![error(
                "material.output.multiple",
                "",
                "only one material_output node is allowed",
            )]);
        }
    };
    let index = GraphIndex::new(document);
    let mut reachable = BTreeSet::new();
    let mut pending = vec![sink.id.as_str()];
    while let Some(id) = pending.pop() {
        if !reachable.insert(id) {
            continue;
        }
        pending.extend(
            document
                .edges
                .iter()
                .filter(|e| e.to.node.as_str() == id)
                .map(|e| e.from.node.as_str()),
        );
    }
    if let Some(node) = document
        .nodes
        .iter()
        .find(|n| !reachable.contains(n.id.as_str()))
    {
        return Err(vec![error(
            "material.node.disconnected",
            format!("/nodes/{}", node.id),
            "node does not contribute to material_output",
        )]);
    }
    let mut indegree: BTreeMap<&str, usize> =
        document.nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    for edge in &document.edges {
        *indegree.entry(edge.to.node.as_str()).or_default() += 1;
    }
    let mut ready: BTreeSet<&str> = indegree
        .iter()
        .filter_map(|(&id, &degree)| (degree == 0).then_some(id))
        .collect();
    let mut order = Vec::with_capacity(document.nodes.len());
    while let Some(id) = ready.pop_first() {
        order.push(id);
        for edge in document
            .edges
            .iter()
            .filter(|edge| edge.from.node.as_str() == id)
        {
            let degree = indegree
                .get_mut(edge.to.node.as_str())
                .expect("validated node");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(edge.to.node.as_str());
            }
        }
    }
    if order.len() != document.nodes.len() {
        return Err(vec![error(
            "material.cycle",
            "",
            "material graph contains a cycle",
        )]);
    }
    let numbers: BTreeMap<&str, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut lines = String::new();
    for id in order {
        let node = index.node(id).expect("validated node");
        if node.node_type == "material_output" {
            continue;
        }
        emit_node(&mut lines, node, &index, &numbers)?;
    }
    let mut outputs = String::new();
    for (port, default) in [
        ("base_color", "color.rgb"),
        ("alpha", "color.a"),
        (
            "metallic",
            "material.options.y * select(1.0, mr.b, maps.z > 0.5)",
        ),
        (
            "roughness",
            "material.options.z * select(1.0, mr.g, maps.z > 0.5)",
        ),
    ] {
        let expression =
            incoming(&index, &numbers, sink, port).unwrap_or_else(|| default.to_owned());
        let expression = match port {
            "base_color" => format!("graph_safe_color({expression})"),
            _ => format!("clamp(graph_safe_float({expression}), 0.0, 1.0)"),
        };
        outputs.push_str(&format!("    let graph_{port} = {expression};\n"));
    }
    for marker in [
        "struct Output {",
        "    let derivative_normal = cross(dpdx(input.world), dpdy(input.world));",
        "    let mr = textureSample(metallic_roughness_map, image_sampler, input.uv);",
        "surface.albedo = color.rgb;",
        "surface.metallic = material.options.y * select(1.0, mr.b, maps.z > 0.5);",
        "surface.roughness = material.options.z * select(1.0, mr.g, maps.z > 0.5);",
        "return vec4<f32>(result, color.a);",
    ] {
        if LIT_WGSL.matches(marker).count() != 1 {
            return Err(vec![error(
                "material.template.changed",
                "",
                "built-in material shader template has changed",
            )]);
        }
    }
    let source = LIT_WGSL.replace(
        "    let derivative_normal = cross(dpdx(input.world), dpdy(input.world));",
        &format!("{lines}    let derivative_normal = cross(dpdx(input.world), dpdy(input.world));"),
    ).replace(
        "    let mr = textureSample(metallic_roughness_map, image_sampler, input.uv);",
        &format!("    let mr = textureSample(metallic_roughness_map, image_sampler, input.uv);\n{outputs}"),
    ).replace("struct Output {", &format!("{SAFE_FUNCTIONS}struct Output {{"))
     .replace("surface.albedo = color.rgb;", "surface.albedo = graph_base_color;")
     .replace("surface.metallic = material.options.y * select(1.0, mr.b, maps.z > 0.5);", "surface.metallic = graph_metallic;")
     .replace("surface.roughness = material.options.z * select(1.0, mr.g, maps.z > 0.5);", "surface.roughness = graph_roughness;")
     .replace("return vec4<f32>(result, color.a);", "return vec4<f32>(result, graph_alpha);");
    if source == LIT_WGSL || !source.contains("let graph_base_color") {
        return Err(vec![error(
            "material.template.changed",
            "",
            "built-in material shader template has changed",
        )]);
    }
    if source.len() > MAX_SOURCE_BYTES {
        return Err(vec![error(
            "material.source.too_large",
            "",
            format!("generated WGSL exceeds {MAX_SOURCE_BYTES} bytes"),
        )]);
    }
    Ok(source)
}

fn incoming(
    index: &GraphIndex<'_>,
    numbers: &BTreeMap<&str, usize>,
    node: &GraphNode,
    port: &str,
) -> Option<String> {
    let edge = index.incoming(node.id.as_str(), port).first()?;
    let number = numbers.get(edge.from.node.as_str())?;
    Some(format!("n{number}_{}", edge.from.port))
}

fn emit_node(
    lines: &mut String,
    node: &GraphNode,
    index: &GraphIndex<'_>,
    numbers: &BTreeMap<&str, usize>,
) -> Result<(), Vec<GraphDiagnostic>> {
    let n = numbers[node.id.as_str()];
    let source =
        |port: &str| incoming(index, numbers, node, port).expect("required port validated");
    let emit = |lines: &mut String, port: &str, expression: String| {
        lines.push_str(&format!("    let n{n}_{port} = {expression};\n"));
    };
    match node.node_type.as_str() {
        "scalar" => emit(lines, "value", numeric_param(node, "value")?),
        "vec3" => {
            let components = node
                .params
                .get("value")
                .and_then(Value::as_array)
                .expect("validated vec3");
            let values: Result<Vec<_>, _> = components
                .iter()
                .map(|v| format_number(v, node, "value"))
                .collect();
            emit(lines, "value", format!("vec3<f32>({})", values?.join(", ")));
        }
        "uv" => emit(lines, "value", "input.uv".to_owned()),
        "tint" => {
            emit(lines, "color", "material.tint.rgb".to_owned());
            emit(lines, "alpha", "material.tint.a".to_owned());
        }
        "metallic" => emit(lines, "value", "material.options.y".to_owned()),
        "roughness" => emit(lines, "value", "material.options.z".to_owned()),
        "texture_color" => {
            let uv = incoming(index, numbers, node, "uv").unwrap_or_else(|| "input.uv".to_owned());
            emit(
                lines,
                "sample",
                format!("textureSample(image, image_sampler, {uv})"),
            );
            emit(lines, "color", format!("n{n}_sample.rgb"));
            emit(lines, "alpha", format!("n{n}_sample.a"));
        }
        "add_float" | "multiply_float" => {
            let op = if node.node_type == "add_float" {
                "+"
            } else {
                "*"
            };
            emit(
                lines,
                "value",
                format!("graph_safe_float({} {op} {})", source("a"), source("b")),
            );
        }
        "add_vec3" | "multiply_vec3" => {
            let op = if node.node_type == "add_vec3" {
                "+"
            } else {
                "*"
            };
            emit(
                lines,
                "value",
                format!("graph_safe_vec3({} {op} {})", source("a"), source("b")),
            );
        }
        "mix_vec3" => emit(
            lines,
            "value",
            format!(
                "graph_safe_vec3(mix({}, {}, clamp(graph_safe_float({}), 0.0, 1.0)))",
                source("a"),
                source("b"),
                source("factor")
            ),
        ),
        _ => {
            return Err(vec![error(
                "material.node.unknown",
                format!("/nodes/{}", node.id),
                "unknown material node",
            )]);
        }
    }
    Ok(())
}

fn numeric_param(node: &GraphNode, name: &str) -> Result<String, Vec<GraphDiagnostic>> {
    let value = node.params.get(name).expect("required parameter validated");
    format_number(value, node, name)
}

fn format_number(
    value: &Value,
    node: &GraphNode,
    name: &str,
) -> Result<String, Vec<GraphDiagnostic>> {
    let number = value.as_f64().expect("validated numeric parameter");
    let float = number as f32;
    if !float.is_finite() {
        return Err(vec![error(
            "material.param.nonfinite",
            format!("/nodes/{}/params/{name}", node.id),
            "parameter cannot be represented as a finite f32",
        )]);
    }
    let mut text = format!("{float:?}");
    if !text.chars().any(|c| matches!(c, '.' | 'e' | 'E')) {
        text.push_str(".0");
    }
    Ok(text)
}
