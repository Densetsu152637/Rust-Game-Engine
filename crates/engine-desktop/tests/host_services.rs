//! Host service ordering: init before ticks, one call per tick, teardown exactly
//! once in reverse order, including when a later service fails to start.

use engine_desktop::{HostService, HostServiceExit, RunOptions, run_with_services};
use engine_project::{Project, Runtime};
use std::sync::{Arc, Mutex};

type Log = Arc<Mutex<Vec<String>>>;

struct Recorder {
    name: &'static str,
    log: Log,
    fail: bool,
}

impl HostService for Recorder {
    fn name(&self) -> &str {
        self.name
    }
    fn before_graphics(&mut self, _: &mut Runtime) -> Result<(), HostServiceExit> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:start", self.name));
        if self.fail {
            Err(HostServiceExit {
                code: 9,
                message: "no".into(),
            })
        } else {
            Ok(())
        }
    }
    fn before_tick(&mut self, _: &mut Runtime, tick: u64) {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:tick{tick}", self.name));
    }
    fn after_gpu_idle(&mut self) {
        self.log.lock().unwrap().push(format!("{}:idle", self.name));
    }
}

fn runtime() -> Runtime {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/assets/project.json");
    Runtime::new(Project::load(path).unwrap()).unwrap()
}

fn services(log: &Log, fail_b: bool) -> Vec<Box<dyn HostService>> {
    vec![
        Box::new(Recorder {
            name: "a",
            log: log.clone(),
            fail: false,
        }),
        Box::new(Recorder {
            name: "b",
            log: log.clone(),
            fail: fail_b,
        }),
    ]
}

fn headless(ticks: u64) -> RunOptions {
    RunOptions {
        headless: true,
        ticks,
        ..Default::default()
    }
}

#[test]
fn services_start_in_order_tick_once_and_stop_in_reverse() {
    let log = Log::default();
    run_with_services(runtime(), headless(2), None, services(&log, false)).unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        [
            "a:start", "b:start", "a:tick0", "b:tick0", "a:tick1", "b:tick1", "b:idle", "a:idle"
        ]
    );
}

#[test]
fn a_failed_start_still_tears_every_service_down_once() {
    let log = Log::default();
    let error = run_with_services(runtime(), headless(2), None, services(&log, true))
        .err()
        .expect("must fail");
    assert_eq!(
        error.downcast_ref::<HostServiceExit>().map(|e| e.code),
        Some(9)
    );
    assert_eq!(
        *log.lock().unwrap(),
        ["a:start", "b:start", "b:idle", "a:idle"]
    );
}
