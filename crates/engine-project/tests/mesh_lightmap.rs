//! Supplied lightmaps: indirect-asset loading, the authored `MeshLightmap`
//! component (schema, validation, snapshots) and its extraction into draws.
use engine_project::rendering3d::{LightmapMode, MeshLightmap};
use engine_project::{Project, Runtime, indirect::load_indirect_assets};
use serde_json::{Value, json};
use std::fs;

fn texels(values: &[[f32; 3]]) -> Vec<u8> {
    values
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

fn mesh_json(with_uv2: bool) -> Vec<u8> {
    let vertex = |x: f32, y: f32| json!({"position": [x, y, 0.0], "uv": [0.0, 0.0], "color": [1.0, 1.0, 1.0, 1.0]});
    let mut mesh = json!({
        "vertices": [vertex(-0.5, -0.5), vertex(0.5, -0.5), vertex(0.5, 0.5), vertex(-0.5, 0.5)],
        "normals": [[0, 0, 1], [0, 0, 1], [0, 0, 1], [0, 0, 1]],
        "indices": [0, 1, 2, 0, 2, 3]
    });
    if with_uv2 {
        mesh["uv2"] = json!([[0, 1], [1, 1], [1, 0], [0, 0]]);
    }
    serde_json::to_vec(&mesh).unwrap()
}

struct Fixture {
    /// Entities after the camera.
    entities: Vec<Value>,
    camera_indirect: Option<&'static str>,
    descriptor: Value,
}

impl Fixture {
    fn new() -> Self {
        Self {
            entities: vec![
                json!({"id":"sharing-a","name":"A","components":{
                    "Transform":{"position":[-1,0,0]},"MeshRenderer":{"mesh":"panel"},
                    "MeshLightmap":{"lightmap":0,"scale":[0.5,0.5],"offset":[0.5,0],"mode":"add"}}}),
                json!({"id":"sharing-b","name":"B","components":{
                    "Transform":{"position":[1,0,0]},"MeshRenderer":{"mesh":"panel"},
                    "MeshLightmap":{"lightmap":1}}}),
                json!({"id":"unbound","name":"U","components":{
                    "Transform":{"position":[0,1,0]},"MeshRenderer":{"mesh":"panel"}}}),
            ],
            camera_indirect: Some("baked"),
            descriptor: json!({"version":1,
                "lightmaps":[{"size":[2,2],"path":"atlas.rgb32f"},{"size":[1,1],"path":"flat.rgb32f"}]}),
        }
    }

    fn write(&self) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("atlas.rgb32f"),
            texels(&[
                [1.0, 0.0, 0.0],
                [0.0, 2.0, 0.0],
                [0.0, 0.0, 3.0],
                [4.0, 4.0, 0.0],
            ]),
        )
        .unwrap();
        fs::write(root.join("flat.rgb32f"), texels(&[[0.5, 0.25, 0.125]])).unwrap();
        fs::write(
            root.join("baked.json"),
            serde_json::to_vec(&self.descriptor).unwrap(),
        )
        .unwrap();
        fs::write(root.join("panel.json"), mesh_json(true)).unwrap();
        fs::write(root.join("bare.json"), mesh_json(false)).unwrap();
        let mut camera = json!({"id":"camera","name":"Camera","components":{
            "Transform":{"position":[0,0,4]},"Camera3D":{}}});
        if let Some(indirect) = self.camera_indirect {
            camera["components"]["Camera3D"] = json!({"lighting":{"indirect":indirect}});
        }
        let mut entities = vec![camera];
        entities.extend(self.entities.clone());
        fs::write(
            root.join("scene.json"),
            serde_json::to_vec(&json!({"schema":1,"scene_id":"s","entities":entities})).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("project.json"),
            serde_json::to_vec(
                &json!({"schema":1,"name":"lightmaps","startup_scene":"scene.json",
                "assets":[
                    {"id":"baked","kind":"indirect","path":"baked.json"},
                    {"id":"panel","kind":"mesh","path":"panel.json"},
                    {"id":"bare","kind":"mesh","path":"bare.json"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        dir
    }
}

fn load(dir: &tempfile::TempDir) -> engine_project::Result<Project> {
    Project::load(dir.path().join("project.json"))
}

fn extract_error(fixture: &Fixture) -> String {
    let dir = fixture.write();
    let runtime = Runtime::new(load(&dir).unwrap()).unwrap();
    runtime
        .extract_scene_frame()
        .expect_err("the draw must be rejected")
        .to_string()
}

#[test]
fn lightmaps_load_and_authored_bindings_reach_the_draws() {
    let dir = Fixture::new().write();
    let project = load(&dir).unwrap();
    let set = load_indirect_assets(&project)
        .unwrap()
        .remove("baked")
        .unwrap();
    assert!(set.environments.is_empty() && set.global.is_none() && set.probes.is_empty());
    assert_eq!(set.lightmaps.len(), 2);
    assert_eq!(set.lightmaps[0].size, [2, 2]);
    assert_eq!(set.lightmaps[0].pixels[3], [4.0, 4.0, 0.0]);
    assert_eq!(set.lightmaps[1].pixels, vec![[0.5, 0.25, 0.125]]);
    assert_eq!(set.decoded_bytes, 4 * 12 + 12);
    assert_eq!(set.texture_bytes, 4 * 8 + 8);

    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    let bound = |id: &str| {
        frame
            .meshes
            .iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("{id}"))
            .lightmap
            .clone()
    };
    assert_eq!(frame.indirect.as_ref().unwrap().lightmaps.len(), 2);
    assert_eq!(
        bound("sharing-a"),
        Some(MeshLightmap {
            version: 1,
            lightmap: 0,
            scale: [0.5, 0.5],
            offset: [0.5, 0.0],
            mode: LightmapMode::Add,
        })
    );
    assert_eq!(
        bound("sharing-b"),
        Some(MeshLightmap {
            lightmap: 1,
            ..Default::default()
        })
    );
    assert_eq!(bound("unbound"), None);
    // Entities sharing one mesh asset share its second UV set.
    let uv2 = |id: &str| {
        frame
            .meshes
            .iter()
            .find(|m| m.id == id)
            .unwrap()
            .mesh
            .clone()
    };
    assert!(std::sync::Arc::ptr_eq(&uv2("sharing-a"), &uv2("sharing-b")));
    assert_eq!(uv2("sharing-a").uv2[2], [1.0, 0.0]);

    // The component survives a snapshot (editor and save paths).
    let snapshot = runtime.try_snapshot().unwrap();
    let entity = snapshot
        .scene
        .entities
        .iter()
        .find(|e| e.id == "sharing-a")
        .unwrap();
    assert_eq!(
        entity.components.mesh_lightmap.as_ref().unwrap().mode,
        LightmapMode::Add
    );
    let value = serde_json::to_value(&entity.components).unwrap();
    assert_eq!(value["MeshLightmap"]["mode"], "add");
    assert!(
        snapshot
            .scene
            .entities
            .iter()
            .find(|e| e.id == "unbound")
            .unwrap()
            .components
            .mesh_lightmap
            .is_none()
    );
}

#[test]
fn bindings_without_uv2_indirect_or_in_range_lightmaps_fail_extraction_by_name() {
    let mut no_uv2 = Fixture::new();
    no_uv2.entities[0]["components"]["MeshRenderer"]["mesh"] = json!("bare");
    let message = extract_error(&no_uv2);
    assert!(
        message.contains("sharing-a") && message.contains("second UV set"),
        "{message}"
    );

    let mut no_indirect = Fixture::new();
    no_indirect.camera_indirect = None;
    let message = extract_error(&no_indirect);
    assert!(message.contains("lighting.indirect"), "{message}");

    let mut missing = Fixture::new();
    missing.entities[1]["components"]["MeshLightmap"]["lightmap"] = json!(2);
    let message = extract_error(&missing);
    assert!(
        message.contains("lightmap 2 is missing") && message.contains("holds 2"),
        "{message}"
    );

    let mut custom = Fixture::new();
    custom.entities[0]["components"]["MeshMaterial"] = json!({"shader":"custom"});
    let dir = custom.write();
    fs::write(dir.path().join("custom.wgsl"), "// custom material").unwrap();
    let manifest = dir.path().join("project.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"custom","kind":"shader","path":"custom.wgsl"}));
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    let runtime = Runtime::new(load(&dir).unwrap()).unwrap();
    let message = runtime.extract_scene_frame().unwrap_err().to_string();
    assert!(message.contains("built-in lit shader"), "{message}");
}

#[test]
fn authored_component_validation_rejects_bad_regions_and_missing_renderers() {
    let rejected = |components: Value| {
        let mut fixture = Fixture::new();
        fixture.entities[0]["components"] = components;
        let dir = fixture.write();
        load(&dir).err().map(|e| e.to_string())
    };
    let panel = |lightmap: Value| json!({"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"panel"},"MeshLightmap":lightmap});
    assert!(rejected(panel(json!({"scale":[0.5,0.5],"offset":[0.5,0.5]}))).is_none());
    for (bad, needle) in [
        (json!({"version":2}), "version 1"),
        (json!({"lightmap":8}), "below 8"),
        (json!({"scale":[0.0,1.0]}), "scale"),
        (json!({"scale":[1.5,1.0]}), "scale"),
        (json!({"scale":[-1.0,1.0]}), "scale"),
        (json!({"offset":[-0.1,0.0]}), "offset"),
        (json!({"offset":[0.0,1.5]}), "offset"),
        (
            json!({"scale":[0.5,0.5],"offset":[0.75,0.0]}),
            "inside the atlas",
        ),
    ] {
        let message = rejected(panel(bad.clone())).unwrap_or_else(|| panic!("{bad}"));
        assert!(message.contains(needle), "{bad}: {message}");
    }
    // Unknown fields and unknown modes are schema errors.
    assert!(rejected(panel(json!({"tile":2}))).is_some());
    assert!(rejected(panel(json!({"mode":"multiply"}))).is_some());
    let message = rejected(json!({
        "Transform":{"position":[0,0,0]},"MeshLightmap":{}
    }))
    .unwrap();
    assert!(
        message.contains("MeshLightmap needs a MeshRenderer"),
        "{message}"
    );
    // Non-finite values cannot be written as JSON, but native validation catches them.
    for bad in [
        MeshLightmap {
            scale: [f32::NAN, 1.0],
            ..Default::default()
        },
        MeshLightmap {
            offset: [f32::INFINITY, 0.0],
            ..Default::default()
        },
    ] {
        assert!(bad.validate().is_err());
    }
    assert!(MeshLightmap::default().validate().is_ok());
}

#[test]
fn lightmap_descriptors_are_bounded_and_confined() {
    let load_with = |descriptor: Value, mutate: &dyn Fn(&std::path::Path)| {
        let mut fixture = Fixture::new();
        fixture.descriptor = descriptor;
        let dir = fixture.write();
        mutate(dir.path());
        load(&dir).err().map(|e| e.to_string())
    };
    let none = |_: &std::path::Path| {};
    let one =
        |path: &str, size: [u32; 2]| json!({"version":1,"lightmaps":[{"size":size,"path":path}]});
    // Exact byte length, finite non-negative radiance, positive bounded size.
    assert!(load_with(one("flat.rgb32f", [1, 1]), &none).is_none());
    let message = load_with(one("flat.rgb32f", [2, 1]), &none).unwrap();
    assert!(message.contains("needs exactly 24 bytes"), "{message}");
    let message = load_with(one("flat.rgb32f", [0, 1]), &none).unwrap();
    assert!(message.contains("dimensions"), "{message}");
    let message = load_with(one("flat.rgb32f", [2049, 1]), &none).unwrap();
    assert!(message.contains("dimensions"), "{message}");
    let message = load_with(one("flat.rgb32f", [1, 1]), &|root| {
        fs::write(root.join("flat.rgb32f"), texels(&[[f32::NAN, 0.0, 0.0]])).unwrap();
    })
    .unwrap();
    assert!(message.contains("radiance"), "{message}");
    let message = load_with(one("flat.rgb32f", [1, 1]), &|root| {
        fs::write(root.join("flat.rgb32f"), texels(&[[-1.0, 0.0, 0.0]])).unwrap();
    })
    .unwrap();
    assert!(message.contains("radiance"), "{message}");
    let message = load_with(one("flat.rgb32f", [1, 1]), &|root| {
        fs::write(root.join("flat.rgb32f"), texels(&[[70000.0, 0.0, 0.0]])).unwrap();
    })
    .unwrap();
    assert!(message.contains("radiance"), "{message}");
    // Paths stay inside the project.
    assert!(load_with(one("../outside.rgb32f", [1, 1]), &none).is_some());
    assert!(load_with(one("missing.rgb32f", [1, 1]), &none).is_some());
    // At most eight lightmaps per set.
    let nine = json!({"version":1,"lightmaps":(0..9).map(|_| json!({"size":[1,1],"path":"flat.rgb32f"})).collect::<Vec<_>>()});
    let message = load_with(nine, &none).unwrap();
    assert!(message.contains("8 lightmaps"), "{message}");
    // A set must provide something to light with.
    let message = load_with(json!({"version":1}), &none).unwrap();
    assert!(message.contains("lightmap"), "{message}");
    // Unknown descriptor fields stay rejected.
    assert!(
        load_with(
            json!({"version":1,"lightmaps":[{"size":[1,1],"path":"flat.rgb32f","srgb":true}]}),
            &none
        )
        .is_some()
    );
}
