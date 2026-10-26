use super::*;
use crate::compiler::{CompileRequest, NativeCompiler};
use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    request: CompileRequest,
    events: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let scratch =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/editor-hotreload");
        fs::create_dir_all(&scratch).unwrap();
        let root = tempfile::tempdir_in(scratch).unwrap();
        let engine = root.path().join("engine");
        let game = root.path().join("game");
        write(
            &engine.join("crates/engine-core/Cargo.toml"),
            "[package]\nname='engine-core'\nversion='0.1.0'\nedition='2024'\n[workspace]\n",
        );
        write(
            &engine.join("crates/engine-core/src/lib.rs"),
            "#[derive(Default)] pub struct EngineApp { pub value: u32 }",
        );
        write(
            &game.join("Cargo.toml"),
            "[package]\nname='reload-game'\nversion='0.1.0'\nedition='2024'\n[workspace]\n[dependencies]\nengine-core={path='../engine/crates/engine-core'}\n",
        );
        write(&game.join("src/lib.rs"), "pub fn version()->u32 {1}");
        let request = CompileRequest {
            engine_root: engine, project_manifest: game.join("Cargo.toml"), cache_root: root.path().join("cache"),
            main_source: r#"fn main() {
                use std::io::Write;
                let events=std::env::args_os().nth(1).unwrap();
                let mut file=std::fs::OpenOptions::new().create(true).append(true).open(events).unwrap();
                let version=gameplay::version();
                writeln!(file,"{version}:start").unwrap(); file.flush().unwrap();
                let stop=std::env::var_os("ENGINE_GENERATION_STOP_FILE").unwrap();
                while !std::path::Path::new(&stop).exists() { std::thread::sleep(std::time::Duration::from_millis(5)); }
                writeln!(file,"{version}:stop").unwrap(); file.flush().unwrap();
            }"#.into(), features: vec![], target: None, release: false, offline: true, target_dir: None,
        };
        let events = root.path().join("events");
        Self {
            root,
            request,
            events,
        }
    }
    fn source(&self, source: &str) {
        write(
            &self
                .request
                .project_manifest
                .parent()
                .unwrap()
                .join("src/lib.rs"),
            source,
        );
    }
    fn queue(&self, host: &mut NativePlayHost) {
        assert!(host.worker.is_none());
        host.reloading = host.active.is_some();
        host.cancel = Arc::new(AtomicBool::new(false));
        let cancel = host.cancel.clone();
        let request = self.request.clone();
        let arguments = vec![self.events.clone().into_os_string()];
        host.worker = Some(thread::spawn(move || {
            let mut compiler = compiler();
            let artifact = compiler.compile(&request, &cancel)?;
            Ok(RunningGeneration::prepare(artifact, &arguments, &cancel)?)
        }));
    }
    fn events(&self) -> String {
        fs::read_to_string(&self.events).unwrap_or_default()
    }
    fn wait_event(&self, event: &str) {
        wait_until(|| self.events().contains(event));
    }
    fn start(&self) -> NativePlayHost {
        let mut host = NativePlayHost::new();
        self.queue(&mut host);
        assert_eq!(settled(&mut host), PlayStatus::Running);
        self.wait_event("1:start");
        host
    }
}
fn write(path: &Path, source: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}
fn compiler() -> NativeCompiler {
    let cargo = PathBuf::from(env!("CARGO"));
    let rustc = cargo
        .parent()
        .unwrap()
        .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    NativeCompiler::new(cargo, rustc)
}
fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !ready() {
        assert!(Instant::now() < deadline, "subprocess fixture timed out");
        thread::sleep(Duration::from_millis(10));
    }
}
fn settled(host: &mut NativePlayHost) -> PlayStatus {
    let mut result = PlayStatus::Compiling;
    wait_until(|| {
        result = host.poll().unwrap();
        !matches!(result, PlayStatus::Compiling | PlayStatus::Reloading)
    });
    result
}

#[test]
fn replacement_waits_at_barrier_then_stops_old_and_starts_once() {
    let fixture = Fixture::new();
    let mut host = fixture.start();
    let old = host.active.as_ref().unwrap().id();
    fixture.source("pub fn version()->u32 {2}");
    fixture.queue(&mut host);
    wait_until(|| host.worker.as_ref().unwrap().is_finished());
    assert_eq!(host.active.as_ref().unwrap().id(), old);
    assert_eq!(fixture.events(), "1:start\n");
    assert_eq!(host.poll().unwrap(), PlayStatus::Running);
    fixture.wait_event("2:start");
    assert_ne!(host.active.as_ref().unwrap().id(), old);
    assert_eq!(host.poll().unwrap(), PlayStatus::Running);
    host.stop().unwrap();
    assert_eq!(fixture.events(), "1:start\n1:stop\n2:start\n2:stop\n");
}

#[test]
fn compile_failure_and_cancelled_ready_candidate_preserve_old_game() {
    let fixture = Fixture::new();
    let mut host = fixture.start();
    let old = host.active.as_ref().unwrap().id();
    fixture.source("this is invalid Rust");
    fixture.queue(&mut host);
    let status = settled(&mut host);
    assert!(
        matches!(status, PlayStatus::ReloadFailed { ref message } if message.contains("compiler.build"))
    );
    assert_eq!(host.active.as_ref().unwrap().id(), old);
    assert_eq!(host.poll().unwrap(), PlayStatus::Running);
    fixture.source("pub fn version()->u32 {2}");
    fixture.queue(&mut host);
    wait_until(|| host.worker.as_ref().unwrap().is_finished());
    host.cancel_reload().unwrap();
    assert_eq!(host.active.as_ref().unwrap().id(), old);
    assert_eq!(fixture.events(), "1:start\n");
    drop(host);
    assert_eq!(fixture.events(), "1:start\n1:stop\n");
}

#[test]
fn readiness_failure_retains_old_and_stop_discards_pending_candidate() {
    let fixture = Fixture::new();
    let mut host = fixture.start();
    let old = host.active.as_ref().unwrap().id();
    // A real process that exits without announcing readiness, unlike a mock
    // status result. No gameplay barrier is inserted for this raw executable.
    let source = fixture.root.path().join("not-ready.rs");
    let executable = fixture.root.path().join(if cfg!(windows) {
        "not-ready.exe"
    } else {
        "not-ready"
    });
    fs::write(&source, "fn main() {}").unwrap();
    assert!(
        Command::new(compiler().rustc)
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap()
            .success()
    );
    let mut artifact = host.active.as_ref().unwrap().artifact.clone();
    artifact.executable = executable;
    host.reloading = true;
    let cancel = host.cancel.clone();
    host.worker = Some(thread::spawn(move || {
        Ok(RunningGeneration::prepare(artifact, &[], &cancel)?)
    }));
    assert!(
        matches!(settled(&mut host), PlayStatus::ReloadFailed { ref message } if message.contains("process.ready"))
    );
    assert_eq!(host.active.as_ref().unwrap().id(), old);
    fixture.source("pub fn version()->u32 {2}");
    fixture.queue(&mut host);
    host.stop().unwrap();
    assert!(host.worker.is_none() && host.active.is_none());
    assert_eq!(fixture.events(), "1:start\n1:stop\n");
}

#[test]
fn worker_panic_does_not_prevent_active_cleanup() {
    let fixture = Fixture::new();
    let mut host = fixture.start();
    host.reloading = true;
    host.worker = Some(thread::spawn(|| panic!("fixture compiler panic")));
    assert!(host.stop().is_err());
    assert!(host.active.is_none() && host.worker.is_none());
    assert_eq!(fixture.events(), "1:start\n1:stop\n");
}

#[test]
fn failed_teardown_never_starts_candidate_and_reports_terminal_failure() {
    let mut fixture = Fixture::new();
    fixture.request.main_source = fixture.request.main_source.replace(
        "writeln!(file,\"{version}:stop\").unwrap(); file.flush().unwrap();",
        "writeln!(file,\"{version}:stop\").unwrap(); file.flush().unwrap(); std::process::exit(7);",
    );
    let mut host = fixture.start();
    fixture.source("pub fn version()->u32 {2}");
    fixture.queue(&mut host);
    assert!(
        matches!(settled(&mut host), PlayStatus::Stopped {success: false, ref message} if message.contains("teardown failed"))
    );
    assert!(host.active.is_none());
    assert_eq!(fixture.events(), "1:start\n1:stop\n");
}

#[test]
fn exited_old_game_is_not_reported_as_preserved_after_compile_failure() {
    let fixture = Fixture::new();
    let mut host = fixture.start();
    host.active.as_mut().unwrap().stop().unwrap();
    fixture.source("invalid Rust");
    fixture.queue(&mut host);
    assert!(matches!(
        settled(&mut host),
        PlayStatus::Stopped { success: false, .. }
    ));
    assert!(host.active.is_none());
}

#[test]
fn activation_failure_after_teardown_reports_stopped_and_reaps_candidate() {
    let fixture = Fixture::new();
    let mut host = fixture.start();
    fixture.source("pub fn version()->u32 {2}");
    fixture.queue(&mut host);
    let worker = host.worker.take().unwrap();
    let mut candidate = worker.join().unwrap().unwrap();
    // Reap the real candidate before activation so its start channel is closed.
    candidate.stop().unwrap();
    assert!(
        matches!(host.activate(candidate), PlayStatus::Stopped {success: false, ref message} if message.contains("activation failed"))
    );
    assert!(host.active.is_none());
    assert_eq!(fixture.events(), "1:start\n1:stop\n");
}
