use engine_project::{Project, Runtime};
use serde_json::{Value, json};
use std::fs;

fn material_graph() -> Value {
    json!({
        "version": 1,
        "kind": "material",
        "nodes": [
            {"id": "red", "node_type": "vec3", "params": {"value": [0.8, 0.1, 0.2]}},
            {"id": "out", "node_type": "material_output"}
        ],
        "edges": [{
            "from": {"node": "red", "port": "value"},
            "to": {"node": "out", "port": "base_color"}
        }]
    })
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("mesh.cgmesh"),
        "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 128, 64, 255]))
        .save(dir.path().join("texture.png"))
        .unwrap();
    fs::write(
        dir.path().join("material.graph.json"),
        material_graph().to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":[
            {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,4]},"Camera3D":{}}},
            {"id":"mesh","name":"Mesh","components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"mesh"},"MeshMaterial":{"shader":"material","texture":"texture","tint":[0.5,0.75,1.0,0.8],"metallic":0.2,"roughness":0.6}}},
            {"id":"sprite","name":"Sprite","components":{"Transform":{"position":[0,0,0]},"SpriteRenderer":{"texture":"texture"},"MeshMaterial":{"shader":"material","texture":"texture","tint":[0.5,0.75,1.0,0.8],"unlit":true}}}
        ]}).to_string(),
    ).unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"material-graph-test","startup_scene":"scene.json","assets":[
            {"id":"mesh","kind":"mesh","path":"mesh.cgmesh"},
            {"id":"texture","kind":"texture","path":"texture.png"},
            {"id":"material","kind":"graph","path":"material.graph.json"}
        ]})
        .to_string(),
    )
    .unwrap();
    dir
}

#[test]
fn authored_graph_compiles_into_mesh_and_sprite_material_snapshots() {
    let dir = fixture();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert_eq!(frame.meshes.len(), 1);
    let mesh = &frame.meshes[0];
    let mesh_source = mesh.shader.as_deref().unwrap();
    assert!(mesh_source.contains("surface.albedo = graph_base_color;"));
    assert!(mesh_source.contains("select(shaded, graph_base_color, material.options.x > 0.5)"));
    assert!(mesh_source.contains("vec3<f32>(0.8, 0.1, 0.2)"));
    assert!(mesh.texture.is_some());
    assert_eq!(mesh.material.tint, [0.5, 0.75, 1.0, 0.8]);
    assert_eq!(mesh.material.metallic, 0.2);
    assert_eq!(mesh.material.roughness, 0.6);

    let sprite = frame
        .sprites
        .draws
        .iter()
        .find(|draw| draw.id == "sprite:sprite")
        .unwrap();
    assert!(sprite.texture.is_some());
    let material = &frame.materials2d["sprite:sprite"];
    assert_eq!(material.shader.as_deref(), Some(mesh_source));
    assert!(material.material.unlit);
    assert_eq!(material.material.texture.as_deref(), Some("texture"));
    assert_eq!(material.material.tint, [0.5, 0.75, 1.0, 0.8]);
}

#[test]
fn graph_of_another_kind_cannot_be_a_material_shader() {
    let dir = fixture();
    let tree = json!({
        "version":1,"kind":"behavior_tree",
        "nodes":[
            {"id":"root","node_type":"root"},
            {"id":"wait","node_type":"wait","params":{"seconds":1.0}}
        ],
        "edges":[{"from":{"node":"root","port":"child"},"to":{"node":"wait","port":"parent"}}]
    });
    fs::write(dir.path().join("material.graph.json"), tree.to_string()).unwrap();
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "project.asset.kind", "{error}");
    assert!(error.to_string().contains("material graph"), "{error}");
}

#[test]
fn invalid_material_graph_reports_its_asset_path() {
    let dir = fixture();
    fs::write(
        dir.path().join("material.graph.json"),
        json!({"version":1,"kind":"material","nodes":[]}).to_string(),
    )
    .unwrap();
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert_eq!(
        error.diagnostics[0].code, "material.output.missing",
        "{error}"
    );
    assert_eq!(
        error.diagnostics[0].file,
        std::path::PathBuf::from("material.graph.json")
    );
}
