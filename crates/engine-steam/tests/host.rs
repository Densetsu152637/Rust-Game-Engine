//! Steam as a desktop host service, driven through the headless runner.

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
    fn set_rich_presence(&mut self, key: &str, value: Option<&str>) -> SteamResult<()> {
        self.fake.lock().unwrap().set_rich_presence(key, value)
    }
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
