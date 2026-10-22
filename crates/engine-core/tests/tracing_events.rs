//! Lifecycle transitions are reported as `tracing` events.
use engine_core::prelude::*;
use engine_log::{Level, LogConfig, LogHistory, LogQuery};

fn capture(run: impl FnOnce()) -> LogHistory {
    let (subscriber, history) = LogConfig::new()
        .with_env_directives(Some("trace"), None)
        .with_writer(std::io::sink)
        .build();
    tracing::subscriber::with_default(subscriber, run);
    history
}

fn messages(history: &LogHistory) -> Vec<String> {
    history.records().into_iter().map(|r| r.message).collect()
}

#[test]
fn startup_and_shutdown_are_logged() {
    let history = capture(|| {
        let mut app = EngineApp::new();
        app.run_fixed(2, 0.25).unwrap();
    });
    let messages = messages(&history);
    assert!(messages.contains(&"engine app starting".to_owned()));
    assert!(messages.contains(&"engine app shutting down".to_owned()));
    assert!(messages.contains(&"engine app stopped".to_owned()));
}

#[test]
fn host_stop_request_is_logged() {
    let history = capture(|| {
        let mut app = EngineApp::new();
        app.request_stop();
        app.tick(0.25).unwrap();
    });
    assert!(messages(&history).contains(&"stop requested by host".to_owned()));
}

#[test]
fn stage_failure_is_logged_as_an_error() {
    #[derive(Resource)]
    struct Needed;
    let history = capture(|| {
        let mut app = EngineApp::new();
        app.schedule_mut(Stage::Update)
            .add_systems(|_: Res<Needed>| {})
            .add_systems(|_: ResMut<Time>| {})
            .add_systems(|_: ResMut<Time>| {});
        assert!(app.tick(0.25).is_err());
    });
    let errors = history.query(&LogQuery {
        level: Some(Level::ERROR),
        ..Default::default()
    });
    assert!(
        errors
            .iter()
            .any(|r| r.message.contains("stage failed") && r.field("stage") == Some("Update")),
        "{errors:?}"
    );
}
