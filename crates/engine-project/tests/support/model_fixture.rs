use serde_json::json;
use std::{fs, path::Path};

pub fn write_project(root: &Path) {
    fs::write(
        root.join("project.json"),
        json!({"schema":1,"name":"Model import fixture","startup_scene":"main.json","assets":[]})
            .to_string(),
    )
    .unwrap();
    fs::write(
        root.join("main.json"),
        json!({"schema":1,"scene_id":"main","entities":[]}).to_string(),
    )
    .unwrap();
}
pub fn write_source(root: &Path, animated: bool) {
    let directory = root.join("source");
    fs::create_dir(&directory).unwrap();
    let mut bytes = Vec::new();
    let mut append = |values: &[f32]| {
        let offset = bytes.len();
        for v in values {
            bytes.extend(v.to_le_bytes());
        }
        (offset, values.len() * 4)
    };
    let positions = append(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
    let uv = append(&[0., 0., 1., 0., 0., 1.]);
    let color = append(&[1., 0., 0., 1., 0., 1., 0., 1., 0., 0., 1., 1.]);
    let times = append(&[0., 1.]);
    let translations = append(&[0., 0., 0., 0., 2., 0.]);
    let views: Vec<_> = [positions, uv, color, times, translations]
        .iter()
        .map(|(offset, len)| json!({"buffer":0,"byteOffset":offset,"byteLength":len}))
        .collect();
    let mut source = json!({"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],
        "nodes":[{"name":"Root","translation":[1,2,3],"children":[1]},{"name":"Triangle","mesh":0,"scale":[2,2,2]}],
        "buffers":[{"uri":"geometry.bin","byteLength":bytes.len()}],"bufferViews":views,
        "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
        {"bufferView":1,"componentType":5126,"count":3,"type":"VEC2"},{"bufferView":2,"componentType":5126,"count":3,"type":"VEC4"},
        {"bufferView":3,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[1]},
        {"bufferView":4,"componentType":5126,"count":2,"type":"VEC3"}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0,"TEXCOORD_0":1,"COLOR_0":2},"material":0}]}],
        "images":[{"uri":"color.png"}],"textures":[{"source":0}],
        "materials":[{"alphaMode":"BLEND","pbrMetallicRoughness":{"baseColorFactor":[0.5,0.6,0.7,0.8],"baseColorTexture":{"index":0},"metallicFactor":0.9,"roughnessFactor":0.2}}]});
    if animated {
        source["animations"] = json!([{"name":"move","samplers":[{"input":3,"output":4,"interpolation":"LINEAR"}],"channels":[{"sampler":0,"target":{"node":1,"path":"translation"}}]}]);
    }
    fs::write(directory.join("model.gltf"), source.to_string()).unwrap();
    fs::write(directory.join("geometry.bin"), bytes).unwrap();
    fs::write(
        directory.join("color.png"),
        [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 175,
            32, 240, 31, 0, 5, 128, 2, 47, 26, 225, 3, 79, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
            130,
        ],
    )
    .unwrap();
}
pub fn write_skinned_source(root: &Path) {
    write_source(root, true);
    let directory = root.join("source");
    let path = directory.join("model.gltf");
    let mut source: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut bytes = fs::read(directory.join("geometry.bin")).unwrap();
    let joints_offset = bytes.len();
    bytes.extend([0_u8; 12]);
    let weights_offset = bytes.len();
    for _ in 0..3 {
        for value in [1_f32, 0., 0., 0.] {
            bytes.extend(value.to_le_bytes());
        }
    }
    source["buffers"][0]["byteLength"] = json!(bytes.len());
    let views = source["bufferViews"].as_array_mut().unwrap();
    views.push(json!({"buffer":0,"byteOffset":joints_offset,"byteLength":12}));
    views.push(json!({"buffer":0,"byteOffset":weights_offset,"byteLength":48}));
    let accessors = source["accessors"].as_array_mut().unwrap();
    accessors.push(json!({"bufferView":5,"componentType":5121,"count":3,"type":"VEC4"}));
    accessors.push(json!({"bufferView":6,"componentType":5126,"count":3,"type":"VEC4"}));
    source["meshes"][0]["primitives"][0]["attributes"]["JOINTS_0"] = json!(5);
    source["meshes"][0]["primitives"][0]["attributes"]["WEIGHTS_0"] = json!(6);
    source["nodes"][0]["children"] = json!([1, 2]);
    source["nodes"][1]["scale"] = json!([1, 1, 1]);
    source["nodes"][1]["skin"] = json!(0);
    source["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"Joint"}));
    source["skins"] = json!([{"joints":[2],"skeleton":2}]);
    source["animations"][0]["channels"][0]["target"]["node"] = json!(2);
    fs::write(path, source.to_string()).unwrap();
    fs::write(directory.join("geometry.bin"), bytes).unwrap();
}
