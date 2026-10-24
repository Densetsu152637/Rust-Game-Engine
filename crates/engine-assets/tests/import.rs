use engine_assets::*;
use serde_json::{Value, json};
use std::path::Path;

fn append_f32(bytes: &mut Vec<u8>, values: &[f32]) -> (usize, usize) {
    let offset = bytes.len();
    for value in values {
        bytes.extend(value.to_le_bytes());
    }
    (offset, bytes.len() - offset)
}
fn fixture() -> (Value, Vec<u8>) {
    let mut bytes = Vec::new();
    let positions = append_f32(&mut bytes, &[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
    let normals = append_f32(&mut bytes, &[0., 0., 1., 0., 0., 1., 0., 0., 1.]);
    let uv = append_f32(&mut bytes, &[0., 0., 1., 0., 0., 1.]);
    let tangents = append_f32(
        &mut bytes,
        &[1., 0., 0., 1., 1., 0., 0., 1., 1., 0., 0., 1.],
    );
    let colors = append_f32(
        &mut bytes,
        &[1., 0., 0., 1., 0., 1., 0., 1., 0., 0., 1., 1.],
    );
    let joints = (bytes.len(), 12);
    bytes.extend([0_u8, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0]);
    let weights = append_f32(
        &mut bytes,
        &[0.25, 0.75, 0., 0., 0.25, 0.75, 0., 0., 0.25, 0.75, 0., 0.],
    );
    let indices = (bytes.len(), 6);
    for value in [0_u16, 1, 2] {
        bytes.extend(value.to_le_bytes());
    }
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    let identity = [
        1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
    ];
    let binds = append_f32(&mut bytes, &[identity, identity].concat());
    let times = append_f32(&mut bytes, &[0., 1.]);
    let translation = append_f32(&mut bytes, &[0., 0., 0., 0., 2., 0.]);
    let rotation = append_f32(&mut bytes, &[0., 0., 0., 1., 0., 0., 1., 0.]);
    let views: Vec<_> = [
        positions,
        normals,
        uv,
        tangents,
        colors,
        joints,
        weights,
        indices,
        binds,
        times,
        translation,
        rotation,
    ]
    .into_iter()
    .map(|(offset, length)| json!({"buffer":0,"byteOffset":offset,"byteLength":length}))
    .collect();
    let document = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],
        "nodes":[{"name":"mesh-root","mesh":0,"skin":0,"children":[1]}, {"name":"joint-root","children":[2]}, {"name":"joint-child","translation":[0,1,0]}],
        "buffers":[{"byteLength":bytes.len()}],"bufferViews":views,
        "accessors":[
            {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
            {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
            {"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"},
            {"bufferView":3,"componentType":5126,"count":3,"type":"VEC4"},
            {"bufferView":4,"componentType":5126,"count":3,"type":"VEC4"},
            {"bufferView":5,"componentType":5121,"count":3,"type":"VEC4"},
            {"bufferView":6,"componentType":5126,"count":3,"type":"VEC4"},
            {"bufferView":7,"componentType":5123,"count":3,"type":"SCALAR"},
            {"bufferView":8,"componentType":5126,"count":2,"type":"MAT4"},
            {"bufferView":9,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[1]},
            {"bufferView":10,"componentType":5126,"count":2,"type":"VEC3"},
            {"bufferView":11,"componentType":5126,"count":2,"type":"VEC4"}],
        "meshes":[{"name":"triangle","primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2,"TANGENT":3,"COLOR_0":4,"JOINTS_0":5,"WEIGHTS_0":6},"indices":7,"material":0}]}],
        "materials":[{"name":"blue","doubleSided":true,"alphaMode":"BLEND","pbrMetallicRoughness":{"baseColorFactor":[0,0,1,0.5],"metallicFactor":0.2,"roughnessFactor":0.8}}],
        "skins":[{"joints":[1,2],"skeleton":1,"inverseBindMatrices":8}],
        "animations":[{"name":"walk","samplers":[{"input":9,"output":10,"interpolation":"LINEAR"},{"input":9,"output":11,"interpolation":"STEP"}],"channels":[{"sampler":0,"target":{"node":2,"path":"translation"}},{"sampler":1,"target":{"node":1,"path":"rotation"}}]}]
    });
    (document, bytes)
}
fn glb(document: &Value, binary: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(document).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bin = binary.to_vec();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let mut output = Vec::new();
    for value in [
        0x46546c67_u32,
        2,
        (12 + 8 + json.len() + 8 + bin.len()) as u32,
        json.len() as u32,
        0x4e4f534a,
    ] {
        output.extend(value.to_le_bytes());
    }
    output.extend(json);
    output.extend((bin.len() as u32).to_le_bytes());
    output.extend(0x004e4942_u32.to_le_bytes());
    output.extend(bin);
    output
}
fn write_glb(root: &Path, document: &Value, binary: &[u8]) {
    std::fs::write(root.join("model.glb"), glb(document, binary)).unwrap();
}
fn import(root: &Path) -> Result<ImportedScene> {
    import_gltf(root, Path::new("model.glb"), &ImportLimits::default())
}

#[test]
fn real_glb_preserves_geometry_material_hierarchy_skin_and_clip() {
    let temp = tempfile::tempdir().unwrap();
    let (document, bytes) = fixture();
    write_glb(temp.path(), &document, &bytes);
    let scene = import(temp.path()).unwrap();
    assert_eq!(scene.nodes[2].parent, Some(1));
    assert_eq!(scene.nodes[0].children, vec![1]);
    assert_eq!(scene.nodes[2].transform.translation, [0., 1., 0.]);
    assert_eq!(scene.scenes, vec![vec![0]]);
    assert_eq!(scene.default_scene, Some(0));
    let primitive = &scene.meshes[0].primitives[0];
    assert_eq!(primitive.indices, vec![0, 1, 2]);
    assert_eq!(primitive.material, Some(0));
    assert_eq!(primitive.vertices[1].position, [1., 0., 0.]);
    assert_eq!(primitive.vertices[1].uv0, Some([1., 0.]));
    assert_eq!(primitive.vertices[1].normal, Some([0., 0., 1.]));
    assert_eq!(primitive.vertices[1].color, Some([0., 1., 0., 1.]));
    assert_eq!(primitive.vertices[1].tangent, Some([1., 0., 0., 1.]));
    assert_eq!(primitive.vertices[0].joints, Some([0, 1, 0, 0]));
    assert_eq!(primitive.vertices[0].weights, Some([0.25, 0.75, 0., 0.]));
    assert_eq!(scene.materials[0].base_color, [0., 0., 1., 0.5]);
    assert_eq!(scene.materials[0].alpha_mode, AlphaMode::Blend);
    assert_eq!(scene.skins[0].joints, vec![1, 2]);
    assert_eq!(scene.skins[0].inverse_bind_matrices.len(), 2);
    assert_eq!(scene.clips[0].duration, 1.);
    assert_eq!(scene.clips[0].channels[0].times, vec![0., 1.]);
    assert_eq!(scene.clips[0].channels[0].target_node, 2);
    assert!(
        matches!(&scene.clips[0].channels[0].values,ChannelValues::Translation(values) if values[1]==[0.,2.,0.])
    );
    assert_eq!(
        scene.clips[0].channels[1].interpolation,
        Interpolation::Step
    );
}
#[test]
fn source_relative_dependencies_are_hashed_and_checkout_independent() {
    let temp = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let (mut doc, mut bin) = fixture();
    doc["buffers"][0]["uri"] = json!("mesh%20data.bin");
    for root in [temp.path(), other.path()] {
        std::fs::create_dir(root.join("assets")).unwrap();
        std::fs::write(
            root.join("assets/scene.gltf"),
            serde_json::to_vec(&doc).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("assets/mesh data.bin"), &bin).unwrap();
    }
    let read = |root: &Path| {
        import_gltf(
            root,
            Path::new("assets/scene.gltf"),
            &ImportLimits::default(),
        )
        .unwrap()
    };
    let first = read(temp.path());
    assert_eq!(first.fingerprint, read(other.path()).fingerprint);
    assert_eq!(first.dependencies.len(), 2);
    bin[0..4].copy_from_slice(&0.5_f32.to_le_bytes());
    std::fs::write(temp.path().join("assets/mesh data.bin"), bin).unwrap();
    assert_ne!(first.fingerprint, read(temp.path()).fingerprint);
}
#[test]
fn decodes_png_and_retains_texture_association() {
    let temp = tempfile::tempdir().unwrap();
    let (mut doc, bin) = fixture();
    image::RgbaImage::from_pixel(2, 1, image::Rgba([12, 34, 56, 255]))
        .save(temp.path().join("color.png"))
        .unwrap();
    doc["images"] = json!([{"uri":"color.png"}]);
    doc["textures"] = json!([{"source":0}]);
    doc["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"] =
        json!({"index":0,"texCoord":0});
    write_glb(temp.path(), &doc, &bin);
    let scene = import(temp.path()).unwrap();
    assert_eq!((scene.images[0].width, scene.images[0].height), (2, 1));
    assert_eq!(
        scene.images[0].rgba8,
        vec![12, 34, 56, 255, 12, 34, 56, 255]
    );
    assert_eq!(scene.textures[0].image, 0);
    assert_eq!(scene.materials[0].base_color_texture.unwrap().texture, 0);
    let limits = ImportLimits {
        max_decoded_image_bytes: 4,
        ..Default::default()
    };
    assert!(import_gltf(temp.path(), Path::new("model.glb"), &limits).is_err());
}
#[test]
fn rejects_out_of_root_source_buffer_and_image_and_remote_uri() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let (mut doc, bin) = fixture();
    std::fs::write(temp.path().join("outside.bin"), &bin).unwrap();
    for uri in [
        "../outside.bin",
        "%2e%2e/outside.bin",
        "https://example.com/mesh.bin",
        "file:///outside.bin",
        "..\\outside.bin",
    ] {
        doc["buffers"][0]["uri"] = json!(uri);
        write_glb(&root, &doc, &bin);
        assert!(import(&root).is_err(), "{uri}");
    }
    doc["buffers"][0].as_object_mut().unwrap().remove("uri");
    doc["images"] = json!([{"uri":"../outside.bin"}]);
    write_glb(&root, &doc, &bin);
    assert!(matches!(import(&root), Err(AssetError::OutsideProject)));
    write_glb(temp.path(), &doc, &bin);
    assert!(matches!(
        import_gltf(&root, Path::new("../model.glb"), &ImportLimits::default()),
        Err(AssetError::OutsideProject)
    ));
}
#[test]
fn rejects_malformed_ranges_counts_cycles_extensions_and_nonfinite_values() {
    let temp = tempfile::tempdir().unwrap();
    let (doc, bin) = fixture();
    let mut cases = Vec::new();
    let mut bad = doc.clone();
    bad["bufferViews"][0]["byteLength"] = json!(999999);
    cases.push(bad);
    let mut bad = doc.clone();
    bad["accessors"][0]["count"] = json!(1_000_000_000_u64);
    cases.push(bad);
    let mut bad = doc.clone();
    bad["accessors"][1]["type"] = json!("VEC2");
    cases.push(bad);
    let mut bad = doc.clone();
    bad["nodes"][2]["children"] = json!([0]);
    cases.push(bad);
    let mut bad = doc.clone();
    bad["extensionsRequired"] = json!(["KHR_draco_mesh_compression"]);
    cases.push(bad);
    let mut bad = doc.clone();
    bad["animations"][0]["samplers"][0]["output"] = json!(11);
    cases.push(bad);
    for case in cases {
        write_glb(temp.path(), &case, &bin);
        assert!(import(temp.path()).is_err());
    }
    let mut nan = bin.clone();
    nan[0..4].copy_from_slice(&f32::NAN.to_le_bytes());
    write_glb(temp.path(), &doc, &nan);
    assert!(import(temp.path()).is_err());
    std::fs::write(temp.path().join("model.glb"), b"not gltf").unwrap();
    assert!(import(temp.path()).is_err());
}
#[test]
fn limits_are_applied_to_aggregate_work() {
    let temp = tempfile::tempdir().unwrap();
    let (doc, bin) = fixture();
    write_glb(temp.path(), &doc, &bin);
    for limits in [
        ImportLimits {
            max_source_bytes: 10,
            ..Default::default()
        },
        ImportLimits {
            max_total_bytes: 10,
            ..Default::default()
        },
        ImportLimits {
            max_entities: 5,
            ..Default::default()
        },
        ImportLimits {
            max_vertices: 2,
            ..Default::default()
        },
        ImportLimits {
            max_indices: 2,
            ..Default::default()
        },
        ImportLimits {
            max_animation_keys: 3,
            ..Default::default()
        },
    ] {
        assert!(import_gltf(temp.path(), Path::new("model.glb"), &limits).is_err());
    }
}
#[test]
fn nonindexed_mesh_and_embedded_buffer_work() {
    use base64::Engine;
    let temp = tempfile::tempdir().unwrap();
    let (mut doc, bin) = fixture();
    doc["meshes"][0]["primitives"][0]
        .as_object_mut()
        .unwrap()
        .remove("indices");
    doc["buffers"][0]["uri"] = json!(format!(
        "data:application/octet-stream;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bin)
    ));
    std::fs::write(
        temp.path().join("model.glb"),
        serde_json::to_vec(&doc).unwrap(),
    )
    .unwrap();
    assert_eq!(
        import(temp.path()).unwrap().meshes[0].primitives[0].indices,
        vec![0, 1, 2]
    );
}
#[test]
fn failed_reload_retains_last_valid_asset_and_generation_invalidates_stale_handles() {
    let mut store = AssetStore::default();
    let first = CacheFingerprint([1; 32]);
    let handle = store.load(LoadedAsset {
        value: "old".to_string(),
        fingerprint: first,
    });
    assert!(
        store
            .reload(handle, || Err(AssetError::Invalid(
                "broken replacement".into()
            )))
            .is_err()
    );
    assert_eq!(store.get(handle).unwrap(), "old");
    assert_eq!(store.fingerprint(handle), Some(first));
    store
        .reload(handle, || {
            Ok(LoadedAsset {
                value: "new".to_string(),
                fingerprint: CacheFingerprint([2; 32]),
            })
        })
        .unwrap();
    assert_eq!(store.unload(handle).unwrap(), "new");
    let next = store.load(LoadedAsset {
        value: "other".to_string(),
        fingerprint: first,
    });
    assert!(store.get(handle).is_none());
    assert!(
        store
            .reload(handle, || panic!("stale loader must not run"))
            .is_err()
    );
    assert_eq!(store.get(next).unwrap(), "other");
}
#[test]
fn cubic_keys_retain_tangent_triplets_and_missing_bind_matrices_default_to_identity() {
    let temp = tempfile::tempdir().unwrap();
    let (mut doc, mut bin) = fixture();
    let values = append_f32(
        &mut bin,
        &[
            0., 0., 0., 0., 0., 0., 0., 1., 0., 0., 1., 0., 0., 2., 0., 0., 0., 0.,
        ],
    );
    let view = doc["bufferViews"].as_array().unwrap().len();
    doc["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer":0,"byteOffset":values.0,"byteLength":values.1}));
    doc["buffers"][0]["byteLength"] = json!(bin.len());
    doc["accessors"][10]["bufferView"] = json!(view);
    doc["accessors"][10]["count"] = json!(6);
    doc["animations"][0]["samplers"][0]["interpolation"] = json!("CUBICSPLINE");
    doc["skins"][0]
        .as_object_mut()
        .unwrap()
        .remove("inverseBindMatrices");
    write_glb(temp.path(), &doc, &bin);
    let scene = import(temp.path()).unwrap();
    let channel = &scene.clips[0].channels[0];
    assert_eq!(channel.interpolation, Interpolation::CubicSpline);
    assert!(
        matches!(&channel.values,ChannelValues::Translation(v) if v.len()==6 && v[4]==[0.,2.,0.])
    );
    assert_eq!(scene.skins[0].inverse_bind_matrices[0][3], [0., 0., 0., 1.]);
}
#[test]
fn rejects_invalid_indices_joint_references_and_key_order() {
    let temp = tempfile::tempdir().unwrap();
    let (doc, bin) = fixture();
    for (view, value) in [
        (7, 99_u16.to_le_bytes().to_vec()),
        (5, vec![99_u8]),
        (9, 1_f32.to_le_bytes().to_vec()),
    ] {
        let mut bad = bin.clone();
        let offset = doc["bufferViews"][view]["byteOffset"].as_u64().unwrap() as usize;
        bad[offset..offset + value.len()].copy_from_slice(&value);
        write_glb(temp.path(), &doc, &bad);
        assert!(import(temp.path()).is_err());
    }
}
#[test]
fn rejects_dependencies_inside_protected_metadata_or_build_directories() {
    let temp = tempfile::tempdir().unwrap();
    let (mut document, binary) = fixture();
    for directory in [".git", ".derived", "target"] {
        std::fs::create_dir(temp.path().join(directory)).unwrap();
        std::fs::write(temp.path().join(directory).join("mesh.bin"), &binary).unwrap();
        document["buffers"][0]["uri"] = json!(format!("{directory}/mesh.bin"));
        write_glb(temp.path(), &document, &binary);
        assert!(matches!(
            import(temp.path()),
            Err(AssetError::Unsupported(_))
        ));
    }
}
