//! The versioned game template under `templates/game` stays consistent with this crate.
use engine::prelude::*;
use std::path::{Path, PathBuf};

#[path = "../../../templates/game/src/lib.rs"]
#[allow(dead_code)]
mod template;

fn template_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/game")
}

#[test]
fn the_template_pins_this_engine_version_by_git_tag() {
    let manifest = std::fs::read_to_string(template_dir().join("Cargo.toml")).unwrap();
    let expected = format!(
        "engine = {{ git = \"https://github.com/Densetsu152637/Rust-Game-Engine.git\", tag = \"v{}\" }}",
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        manifest.lines().any(|line| line == expected),
        "templates/game/Cargo.toml must contain `{expected}`; update the tag when the workspace version changes"
    );
}

#[test]
fn the_template_project_loads_and_its_script_runs_through_the_facade() {
    let project = Project::load(template_dir().join("project.json")).unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    template::configure(runtime.app_mut()).unwrap();
    let player = runtime.entity("entity:player").expect("authored player");
    let frame = ActionFrame {
        pressed: ["move_right".into()].into(),
        ..Default::default()
    };
    runtime.tick(1.0, frame).unwrap();
    let x = runtime
        .app()
        .world()
        .get::<Transform>(player)
        .unwrap()
        .position[0];
    assert!(x > 0.0, "player moved along +X, got {x}");
    runtime.stop().unwrap();
}
