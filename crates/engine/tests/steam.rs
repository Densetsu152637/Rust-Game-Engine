//! `engine::steam` is reachable through the facade, and the Steam host service works
//! from a facade-based game: this is the wiring a game's `main` writes.
#![cfg(feature = "desktop")]

use engine::desktop::{RunOptions, run_with_services};
use engine::prelude::*;
use engine::project::SteamManifest;
use engine::steam::{AppId, FakeBackend, SteamPlugin, SteamSession, status_json};
use std::collections::BTreeMap;
use std::path::Path;

#[path = "../../../templates/game/src/lib.rs"]
#[allow(dead_code)]
mod template;

fn template_runtime() -> Runtime {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/game/project.json");
    let mut runtime = Runtime::new(Project::load(path).unwrap()).unwrap();
    template::configure(runtime.app_mut()).unwrap();
    runtime
}

#[test]
fn a_facade_game_runs_the_steam_host_service_headless() {
    let session = SteamSession::new(Box::new(FakeBackend::ready(AppId(480), "tester")));
    let manifest = SteamManifest {
        app_id: 480,
        cloud: false,
        required: false,
        rich_presence: BTreeMap::new(),
    };
    let services: Vec<Box<dyn engine::desktop::HostService>> =
        vec![Box::new(SteamPlugin::new(session.clone(), manifest))];
    let runtime = template_runtime();
    let options = RunOptions {
        headless: true,
        ticks: 3,
        ..Default::default()
    };
    let report = run_with_services(runtime, options, None, services).unwrap();
    assert_eq!(report.ticks, 3);
    assert_eq!(status_json(&session.status())["state"], "ready");
}

#[test]
fn the_default_backend_matches_the_compiled_feature() {
    assert_eq!(engine::steam::STEAM_COMPILED, cfg!(feature = "steam"));
    let status = engine::steam::default_backend().status();
    if cfg!(feature = "steam") {
        assert_ne!(status, engine::steam::SteamStatus::NotCompiled);
    } else {
        assert_eq!(status, engine::steam::SteamStatus::NotCompiled);
    }
}
