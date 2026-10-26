//! Actual native entry-point acceptance for the editor's explicit reload action.
use engine_editor::{PlayHost, PlayStatus};
use engine_project::{EditOperation, Project, SceneDocument};
use engine_tooling::play_host::{NativePlayHost, PlayMode};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn engine_path(name: &str) -> String {
    root()
        .join("crates")
        .join(name)
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}
fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(600);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "native editor hot reload timed out"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
fn settled(host: &mut NativePlayHost) -> PlayStatus {
    let mut status = PlayStatus::Compiling;
    wait_until(|| {
        status = host.poll().unwrap();
        !matches!(status, PlayStatus::Compiling | PlayStatus::Reloading)
    });
    status
}
fn project(manifest: &Path, x: f32) -> Project {
    let mut document = SceneDocument::new(Project::load(manifest).unwrap());
    let revision = document.revision().to_owned();
    document
        .apply(
            EditOperation::SetPosition {
                entity_id: "entity:player".into(),
                position: [x, 0.0, 0.0],
            },
            &revision,
        )
        .unwrap();
    let revision = document.revision().to_owned();
    document.begin_play(&revision).unwrap()
}
fn source(events: &Path, version: u32) -> String {
    r#"
use engine_core::bevy_ecs::prelude::*;
use engine_project::{NativeScript,Transform};
fn event(message: &str) {
    use std::io::Write;
    let mut file=std::fs::OpenOptions::new().create(true).append(true).open(EVENTS).unwrap();
    writeln!(file,"{message}").unwrap(); file.flush().unwrap();
}
struct Player;
impl NativeScript for Player {
    fn on_create(&mut self,world:&mut World,owner:Entity)->engine_project::Result<()> {
        let x=world.get::<Transform>(owner).unwrap().position[0];
        let store=&world.resource::<engine_project::runtime::Persistence>().0;
        // Saving uses the actual editor-provided private directory.
        store.save("reload", Default::default(), None)?;
        event(&format!("VERSION:start:{x}:{}",store.options().root.display()));
        Ok(())
    }
    fn on_update(&mut self,_:&mut World,_:Entity,_:f32)->engine_project::Result<()> {
        std::thread::sleep(std::time::Duration::from_millis(1)); Ok(())
    }
    fn on_destroy(&mut self,_:&mut World,_:Entity)->engine_project::Result<()> {event("VERSION:stop");Ok(())}
}
pub fn configure(app:&mut engine_core::EngineApp)->engine_core::Result<()> {
    engine_project::register_script(app,"asset:player",|_,_|Ok(Box::new(Player)))?; Ok(())
}
"#.replace("EVENTS", &format!("{:?}", events.to_string_lossy())).replace("VERSION", &version.to_string())
}

#[test]
fn actual_editor_reload_uses_latest_snapshot_and_retains_game_on_compile_failure() {
    let scratch = root().join("target/editor-hotreload");
    fs::create_dir_all(&scratch).unwrap();
    let temp = tempfile::tempdir_in(&scratch).unwrap();
    let game = temp.path().join("game");
    fs::create_dir_all(game.join("src")).unwrap();
    fs::write(game.join("Cargo.toml"), format!("[package]\nname='editor-reload-game'\nversion='0.1.0'\nedition='2024'\n[workspace]\n[dependencies]\nengine-core={{path={:?}}}\nengine-project={{path={:?}}}\n",engine_path("engine-core"),engine_path("engine-project"))).unwrap();
    let events = temp.path().join("events");
    fs::write(game.join("src/lib.rs"), source(&events, 1)).unwrap();
    let manifest = game.join("project.json");
    fs::write(&manifest,r#"{"schema":1,"name":"editor hot reload","startup_scene":"main.scene.json","rust":{"manifest":"Cargo.toml"},"assets":[{"id":"asset:player","kind":"script","path":"src/lib.rs"}]}"#).unwrap();
    let scene = game.join("main.scene.json");
    fs::write(&scene,r#"{"schema":1,"scene_id":"scene:main","entities":[{"id":"entity:player","name":"Player","components":{"Transform":{"position":[0,0,0]},"Script":{"asset":"asset:player"}}}]}"#).unwrap();
    let authored = fs::read(&scene).unwrap();
    let read_events = || fs::read_to_string(&events).unwrap_or_default();
    let mut host = NativePlayHost::with_mode(PlayMode::Headless)
        .with_target_dir(scratch.join("native-target"));
    host.start(project(&manifest, 3.0)).unwrap();
    assert_eq!(settled(&mut host), PlayStatus::Running);
    wait_until(|| read_events().contains("1:start:3:"));
    fs::write(game.join("src/lib.rs"), "invalid Rust").unwrap();
    host.reload(project(&manifest, 9.0)).unwrap();
    assert!(
        matches!(settled(&mut host), PlayStatus::ReloadFailed {message} if message.contains("compiler.build"))
    );
    assert!(!read_events().contains("stop"));
    assert_eq!(host.poll().unwrap(), PlayStatus::Running);
    fs::write(game.join("src/lib.rs"), source(&events, 2)).unwrap();
    host.reload(project(&manifest, 7.0)).unwrap();
    assert_eq!(settled(&mut host), PlayStatus::Running);
    wait_until(|| read_events().contains("2:start:7:"));
    host.stop().unwrap();
    let observed = read_events();
    let lines: Vec<_> = observed.lines().collect();
    assert_eq!(lines.len(), 4, "{observed}");
    assert_eq!(lines[1], "1:stop");
    assert_eq!(lines[3], "2:stop");
    let old_save = Path::new(lines[0].strip_prefix("1:start:3:").unwrap());
    let new_save = Path::new(lines[2].strip_prefix("2:start:7:").unwrap());
    assert_ne!(old_save, new_save);
    assert!(old_save.ends_with("editor-user-data") && new_save.ends_with("editor-user-data"));
    assert!(
        old_save.starts_with(game.join(".derived/native"))
            && new_save.starts_with(game.join(".derived/native"))
    );
    assert!(old_save.is_dir() && new_save.is_dir());
    assert_eq!(authored, fs::read(scene).unwrap());
}
