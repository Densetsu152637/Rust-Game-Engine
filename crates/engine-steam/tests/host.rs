//! Steam as a desktop host service, driven through the headless runner.

mod common;

use engine_desktop::{RunOptions, run_with_services};
use engine_project::{HostService, HostServiceExit, Project, Runtime, SteamManifest};
use engine_steam::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Shares a `FakeBackend` with the test and records when the backend is dropped.
struct Spy {
    fake: Arc<Mutex<FakeBackend>>,
    dropped: Arc<AtomicBool>,
}

impl Drop for Spy {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl SteamBackend for Spy {
    fn status(&self) -> SteamStatus {
        self.fake.lock().unwrap().status()
    }
    fn init(&mut self, app_id: Option<AppId>) -> SteamStatus {
        self.fake.lock().unwrap().init(app_id)
    }
    fn run_callbacks(&mut self) -> Vec<SteamEvent> {
        self.fake.lock().unwrap().run_callbacks()
    }
    fn restart_app_if_necessary(&mut self, app_id: AppId) -> SteamResult<bool> {
        self.fake.lock().unwrap().restart_app_if_necessary(app_id)
    }
    fn request_current_stats(&mut self) -> SteamResult<()> {
        self.fake.lock().unwrap().request_current_stats()
    }
    fn achievement(&self, name: &str) -> SteamResult<bool> {
        self.fake.lock().unwrap().achievement(name)
    }
    fn set_achievement(&mut self, name: &str, unlocked: bool) -> SteamResult<()> {
        self.fake.lock().unwrap().set_achievement(name, unlocked)
    }
    fn stat(&self, name: &str) -> SteamResult<StatValue> {
        self.fake.lock().unwrap().stat(name)
    }
    fn set_stat(&mut self, name: &str, value: StatValue) -> SteamResult<()> {
        self.fake.lock().unwrap().set_stat(name, value)
    }
    fn store_stats(&mut self) -> SteamResult<()> {
        self.fake.lock().unwrap().store_stats()
    }
    fn cloud_enabled_for_app(&self) -> SteamResult<bool> {
        self.fake.lock().unwrap().cloud_enabled_for_app()
    }
    fn cloud_enabled_for_account(&self) -> SteamResult<bool> {
        self.fake.lock().unwrap().cloud_enabled_for_account()
    }
    fn cloud_list(&self) -> SteamResult<Vec<String>> {
        self.fake.lock().unwrap().cloud_list()
    }
    fn cloud_timestamp(&self, name: &str) -> SteamResult<i64> {
        self.fake.lock().unwrap().cloud_timestamp(name)
    }
    fn cloud_read(&self, name: &str) -> SteamResult<Vec<u8>> {
        self.fake.lock().unwrap().cloud_read(name)
    }
    fn cloud_write(&mut self, name: &str, data: &[u8]) -> SteamResult<()> {
        self.fake.lock().unwrap().cloud_write(name, data)
    }
    fn set_rich_presence(&mut self, key: &str, value: Option<&str>) -> SteamResult<()> {
        self.fake.lock().unwrap().set_rich_presence(key, value)
    }
    common::forward_net!();
}

struct Harness {
    fake: Arc<Mutex<FakeBackend>>,
    dropped: Arc<AtomicBool>,
    session: SteamSession,
}

fn harness(fake: FakeBackend) -> Harness {
    let fake = Arc::new(Mutex::new(fake));
    let dropped = Arc::new(AtomicBool::new(false));
    let spy = Spy {
        fake: fake.clone(),
        dropped: dropped.clone(),
    };
    Harness {
        fake,
        dropped,
        session: SteamSession::new(Box::new(spy)),
    }
}

impl Harness {
    fn calls(&self) -> Vec<String> {
        self.fake.lock().unwrap().calls().to_vec()
    }
}

fn runtime() -> Runtime {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine-desktop/examples/assets/project.json");
    Runtime::new(Project::load(path).unwrap()).unwrap()
}

fn manifest(required: bool) -> SteamManifest {
    SteamManifest {
        app_id: 480,
        cloud: false,
        required,
        rich_presence: BTreeMap::from([("status".to_owned(), "Menu".to_owned())]),
    }
}

fn headless(ticks: u64) -> RunOptions {
    RunOptions {
        headless: true,
        ticks,
        ..Default::default()
    }
}

fn services(h: &Harness, required: bool) -> Vec<Box<dyn HostService>> {
    vec![Box::new(SteamPlugin::new(
        h.session.clone(),
        manifest(required),
    ))]
}

#[test]
fn headless_run_pumps_exactly_once_per_tick_and_inits_first() {
    let h = harness(FakeBackend::ready(AppId(480), "tester"));
    let report = run_with_services(runtime(), headless(5), None, services(&h, true)).unwrap();
    assert_eq!(report.ticks, 5);
    let calls = h.calls();
    assert_eq!(&calls[..2], ["restart_app_if_necessary", "init"]);
    assert_eq!(calls.iter().filter(|c| *c == "run_callbacks").count(), 5);
    assert_eq!(h.fake.lock().unwrap().rich_presence()["status"], "Menu");
}

#[test]
fn backend_is_dropped_only_after_the_last_session_holder() {
    let h = harness(FakeBackend::ready(AppId(480), "tester"));
    let Harness {
        dropped,
        session,
        fake,
    } = h;
    let service: Vec<Box<dyn HostService>> =
        vec![Box::new(SteamPlugin::new(session.clone(), manifest(false)))];
    run_with_services(runtime(), headless(1), None, service).unwrap();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "the host still holds the session"
    );
    drop(session);
    assert!(dropped.load(Ordering::SeqCst));
    drop(fake);
}

#[test]
fn two_generations_share_one_init() {
    let h = harness(FakeBackend::ready(AppId(480), "tester"));
    run_with_services(runtime(), headless(2), None, services(&h, true)).unwrap();
    run_with_services(runtime(), headless(3), None, services(&h, true)).unwrap();
    let fake = h.fake.lock().unwrap();
    assert_eq!(fake.call_count("restart_app_if_necessary"), 1);
    assert_eq!(fake.call_count("init"), 1);
    assert_eq!(fake.call_count("run_callbacks"), 5);
}

#[test]
fn steam_events_reach_the_game_event_trace() {
    let h = harness(FakeBackend::ready(AppId(480), "tester"));
    h.fake
        .lock()
        .unwrap()
        .push_event(SteamEvent::OverlayToggled(true));
    let report = run_with_services(runtime(), headless(2), None, services(&h, false)).unwrap();
    let names: Vec<_> = report
        .event_trace
        .events
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert!(names.contains(&"steam.overlay_toggled"), "{names:?}");
}

fn exit_of(result: anyhow::Result<engine_desktop::RunReport>) -> HostServiceExit {
    result
        .err()
        .expect("run must fail")
        .downcast_ref::<HostServiceExit>()
        .cloned()
        .expect("a HostServiceExit")
}

#[test]
fn required_without_steam_exits_before_any_window_or_tick() {
    for headless_run in [true, false] {
        let h = harness(FakeBackend::with_status(SteamStatus::NotRunning));
        let options = RunOptions {
            headless: headless_run,
            ticks: 3,
            ..Default::default()
        };
        let exit = exit_of(run_with_services(
            runtime(),
            options,
            None,
            services(&h, true),
        ));
        assert_eq!(exit.code, REQUIRED_EXIT_CODE);
        assert_eq!(h.calls(), ["restart_app_if_necessary", "init"]);
    }
}

#[test]
fn optional_steam_absent_is_a_soft_status() {
    let h = harness(FakeBackend::with_status(SteamStatus::NotRunning));
    let report = run_with_services(runtime(), headless(2), None, services(&h, false)).unwrap();
    assert_eq!(report.ticks, 2);
}

#[test]
fn relaunch_by_steam_exits_quietly_without_init() {
    let h = harness(FakeBackend::ready(AppId(480), "tester").with_restart_required(true));
    let exit = exit_of(run_with_services(
        runtime(),
        headless(2),
        None,
        services(&h, true),
    ));
    assert_eq!(exit.code, 0);
    assert_eq!(h.calls(), ["restart_app_if_necessary"]);
}

#[test]
fn optional_steam_never_asks_for_a_relaunch() {
    let h = harness(FakeBackend::ready(AppId(480), "tester").with_restart_required(true));
    let report = run_with_services(runtime(), headless(2), None, services(&h, false)).unwrap();
    assert_eq!(report.ticks, 2);
    let fake = h.fake.lock().unwrap();
    assert_eq!(fake.call_count("restart_app_if_necessary"), 0);
    assert_eq!(fake.call_count("init"), 1);
}

#[test]
fn the_game_world_gets_the_steam_handle_and_status() {
    let h = harness(FakeBackend::ready(AppId(480), "tester").with_achievement("WIN"));
    let mut plugin = SteamPlugin::new(h.session.clone(), manifest(false));
    let mut rt = runtime();
    plugin.before_graphics(&mut rt).unwrap();
    let world = rt.app().world();
    assert!(world.resource::<SteamRuntimeStatus>().0.is_ready());
    let handle = world.resource::<SteamSession>();
    assert_eq!(handle.unlock_achievement("WIN"), Ok(true));
    assert_eq!(h.calls()[..2], ["init", "request_current_stats"]);
}

#[test]
fn batched_stats_are_stored_when_the_run_ends() {
    let h = harness(
        FakeBackend::ready(AppId(480), "tester")
            .with_stat("games", StatValue::Int(0))
            .with_achievement("WIN"),
    );
    let mut plugin = SteamPlugin::new(h.session.clone(), manifest(false));
    let mut rt = runtime();
    plugin.before_graphics(&mut rt).unwrap();
    h.session.set_progress("games", 4, 10).unwrap();
    plugin.after_gpu_idle();
    assert_eq!(
        h.fake.lock().unwrap().stored_stats()["games"],
        StatValue::Int(4)
    );
}

fn cloud_manifest(cloud: bool) -> SteamManifest {
    SteamManifest {
        cloud,
        ..manifest(false)
    }
}

fn save_data(n: i64) -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({ "n": n }).as_object().unwrap().clone()
}

#[test]
fn cloud_pulls_before_the_first_load_and_pushes_every_save() {
    let dir = tempfile::tempdir().unwrap();
    let incoming = br#"{"schema":1,"content_version":1,"data":{"n":41}}"#;
    let h =
        harness(FakeBackend::ready(AppId(480), "tester").with_cloud_file("saves/a.json", incoming));
    let mut rt = runtime();
    rt.set_user_data_root(dir.path().to_owned()).unwrap();
    let mut plugin = SteamPlugin::new(h.session.clone(), cloud_manifest(true));
    plugin.before_graphics(&mut rt).unwrap();
    let store = rt
        .app()
        .world()
        .resource::<engine_project::runtime::Persistence>()
        .0
        .clone();
    // Nothing has loaded yet, and the cloud copy is already local.
    assert_eq!(store.load("a", None).unwrap(), save_data(41));
    assert_eq!(
        h.session.cloud_report().unwrap().slots,
        [("a".to_owned(), SyncAction::Pulled)]
    );
    store.save("b", save_data(2), None).unwrap();
    let pushed = h
        .fake
        .lock()
        .unwrap()
        .cloud_file("saves/b.json")
        .map(<[u8]>::to_vec);
    assert_eq!(
        pushed.unwrap(),
        std::fs::read(store.slot_path("b").unwrap()).unwrap()
    );
}

#[test]
fn cloud_stays_off_unless_the_manifest_asks() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(FakeBackend::ready(AppId(480), "tester"));
    let mut rt = runtime();
    rt.set_user_data_root(dir.path().to_owned()).unwrap();
    let mut plugin = SteamPlugin::new(h.session.clone(), cloud_manifest(false));
    plugin.before_graphics(&mut rt).unwrap();
    let store = rt
        .app()
        .world()
        .resource::<engine_project::runtime::Persistence>()
        .0
        .clone();
    store.save("b", save_data(2), None).unwrap();
    assert!(h.session.cloud_report().is_none());
    assert_eq!(h.fake.lock().unwrap().call_count("cloud_write"), 0);
}

#[test]
fn a_failed_cloud_push_is_an_event_and_never_a_failed_save() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(FakeBackend::ready(AppId(480), "tester").with_cloud_quota(2));
    let mut rt = runtime();
    rt.set_user_data_root(dir.path().to_owned()).unwrap();
    let mut plugin = SteamPlugin::new(h.session.clone(), cloud_manifest(true));
    plugin.before_graphics(&mut rt).unwrap();
    let store = rt
        .app()
        .world()
        .resource::<engine_project::runtime::Persistence>()
        .0
        .clone();
    store.save("b", save_data(2), None).unwrap();
    assert_eq!(store.load("b", None).unwrap(), save_data(2));
    plugin.before_tick(&mut rt, 0);
    assert!(h.fake.lock().unwrap().cloud_file("saves/b.json").is_none());
}
