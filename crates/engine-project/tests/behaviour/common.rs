#![allow(dead_code)]
//! Shared fixtures. Every fixture lives in its own unique temporary directory that
//! is removed when the owning value drops.
use engine_project::{Project, Result, Runtime};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const PLAYER_COMPONENTS: &str = r#"{"Transform":{"position":[0,0,0]},"SpriteRenderer":{"texture":"texture","size":[1,1]},"Collider2D":{"size":[1,1]}}"#;

pub fn font_json() -> Value {
    json!({
        "schema": 1, "texture": "fonttex", "lineHeight": 10, "fallback": 63,
        "glyphs": [
            {"codepoint": 63, "source": [0, 0, 4, 4], "advance": 4, "bearing": [0, 0]},
            {"codepoint": 97, "source": [4, 0, 4, 4], "advance": 5, "bearing": [0, 0]},
            {"codepoint": 98, "source": [0, 4, 4, 4], "advance": 6, "bearing": [0, 0]},
            {"codepoint": 32, "source": [0, 0, 0, 0], "advance": 3, "bearing": [0, 0]}
        ]
    })
}

pub fn ppm(width: u32, height: u32, fill: impl Fn(u32, u32) -> [u8; 3]) -> String {
    let mut s = format!("P3\n{width} {height}\n255\n");
    for y in 0..height {
        for x in 0..width {
            let c = fill(x, y);
            s.push_str(&format!("{} {} {}\n", c[0], c[1], c[2]));
        }
    }
    s
}

pub fn wav(rate: u32, channels: u16, frames: usize) -> Vec<u8> {
    let data_len = frames * channels as usize * 2;
    let mut out = Vec::new();
    out.extend(b"RIFF");
    out.extend((36 + data_len as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(channels.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend((rate * channels as u32 * 2).to_le_bytes());
    out.extend((channels * 2).to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((data_len as u32).to_le_bytes());
    for i in 0..frames * channels as usize {
        out.extend((((i % 100) as i16 - 50) * 100).to_le_bytes());
    }
    out
}

pub fn ent(id: &str, components: Value) -> Value {
    json!({"id": id, "name": id, "components": components})
}

pub fn scene_json(id: &str, entities: Vec<Value>) -> Value {
    json!({"schema": 1, "scene_id": id, "entities": entities})
}

/// A solid box entity centred at `(x, y)`.
pub fn wall(id: &str, x: f32, y: f32, w: f32, h: f32) -> Value {
    ent(
        id,
        json!({"Transform":{"position":[x,y,0]},"Collider2D":{"size":[w,h]}}),
    )
}

pub fn trigger(id: &str, x: f32, y: f32) -> Value {
    ent(
        id,
        json!({"Transform":{"position":[x,y,0]},"Collider2D":{"size":[1,1],"trigger":true}}),
    )
}

pub fn marker(id: &str, x: f32, y: f32) -> Value {
    ent(id, json!({"Transform":{"position":[x,y,0]}}))
}

pub struct Proj {
    pub dir: tempfile::TempDir,
    pub manifest: Value,
    pub scene: Value,
}

impl Proj {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("player.rs"), "pub fn configure() {}\n").unwrap();
        fs::write(root.join("other.rs"), "pub fn configure() {}\n").unwrap();
        fs::write(
            root.join("quad.mesh"),
            "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
        )
        .unwrap();
        fs::write(root.join("white.ppm"), "P3\n1 1\n255\n255 255 255\n").unwrap();
        fs::write(
            root.join("sheet.ppm"),
            ppm(4, 2, |x, y| [(x * 40) as u8, (y * 100) as u8, 7]),
        )
        .unwrap();
        fs::write(root.join("font.ppm"), ppm(8, 8, |_, _| [255, 255, 255])).unwrap();
        fs::write(
            root.join("font.json"),
            serde_json::to_vec(&font_json()).unwrap(),
        )
        .unwrap();
        fs::write(root.join("sfx.wav"), wav(8000, 1, 800)).unwrap();
        let manifest = json!({
            "schema": 1, "name": "Behaviour Project", "startup_scene": "scene.json",
            "inputActions": {"right": "Right"},
            "assets": [
                {"id":"player","kind":"script","path":"player.rs"},
                {"id":"other","kind":"script","path":"other.rs"},
                {"id":"mesh","kind":"mesh","path":"quad.mesh"},
                {"id":"texture","kind":"texture","path":"white.ppm"},
                {"id":"sheet","kind":"texture","path":"sheet.ppm"},
                {"id":"fonttex","kind":"texture","path":"font.ppm"},
                {"id":"font","kind":"font","path":"font.json"},
                {"id":"sfx","kind":"audio","path":"sfx.wav"},
                {"id":"scene:a","kind":"scene","path":"scene.json"}
            ]
        });
        let components: Value = serde_json::from_str(PLAYER_COMPONENTS).unwrap();
        let scene = scene_json(
            "room:a",
            vec![
                ent("player", components),
                ent("camera", json!({"Camera2D":{"follow":"player"}})),
            ],
        );
        let p = Self {
            dir,
            manifest,
            scene,
        };
        p.flush();
        p
    }
    pub fn root(&self) -> &Path {
        self.dir.path()
    }
    pub fn manifest_path(&self) -> PathBuf {
        self.root().join("project.json")
    }
    pub fn write(&self, rel: &str, bytes: impl AsRef<[u8]>) {
        let path = self.root().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }
    pub fn read(&self, rel: &str) -> Vec<u8> {
        fs::read(self.root().join(rel)).unwrap()
    }
    pub fn flush(&self) {
        fs::write(
            self.manifest_path(),
            serde_json::to_vec(&self.manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            self.root().join("scene.json"),
            serde_json::to_vec(&self.scene).unwrap(),
        )
        .unwrap();
    }
    pub fn load(&self) -> Result<Project> {
        self.flush();
        Project::load(self.manifest_path())
    }
    pub fn project(&self) -> Project {
        self.load().unwrap()
    }
    pub fn runtime(&self) -> Runtime {
        Runtime::new(self.project()).unwrap()
    }
    pub fn add_asset(&mut self, id: &str, kind: &str, path: &str) {
        self.manifest["assets"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": id, "kind": kind, "path": path}));
    }
    pub fn add_scene(&mut self, id: &str, file: &str, scene: Value) {
        self.write(file, serde_json::to_vec(&scene).unwrap());
        self.add_asset(id, "scene", file);
    }
    pub fn entity(&mut self, id: &str) -> &mut Value {
        self.scene["entities"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|e| e["id"] == id)
            .unwrap()
    }
    pub fn set_player(&mut self, components: Value) {
        self.entity("player")["components"] = components;
    }
    pub fn add_entity(&mut self, entity: Value) {
        self.scene["entities"].as_array_mut().unwrap().push(entity);
    }
    pub fn with_script(&mut self, id: &str, asset: &str) {
        self.entity(id)["components"]["Script"] = json!({"asset": asset});
    }
}

pub fn code<T>(result: Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error.diagnostics[0].code.clone(),
    }
}

pub fn expect_code<T>(result: Result<T>, expected: &str) {
    assert_eq!(code(result), expected);
}

pub fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

pub fn map(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().expect("object literal").clone()
}

/// Try to create a directory symbolic link; `false` when the OS refuses (for
/// example an unprivileged Windows account).
pub fn try_symlink_dir(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_dir(target, link);
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    result.is_ok()
}

pub fn try_symlink_file(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_file(target, link);
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    result.is_ok()
}
