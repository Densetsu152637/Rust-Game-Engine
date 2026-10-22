//! Behavioural tests for history, filtering, record contents and output routing.
use std::{
    io::Write,
    process::Command,
    sync::{Arc, Mutex},
};

use engine_log::{
    Colour, InitError, Level, LogConfig, LogHistory, LogQuery, LogRecord, init, resolve_filter,
};
use tracing::level_filters::LevelFilter;

#[derive(Clone, Default)]
struct TestWriter(Arc<Mutex<Vec<u8>>>);

impl TestWriter {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TestWriter {
    type Writer = TestWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn config(writer: &TestWriter) -> LogConfig {
    LogConfig::new()
        .with_env_directives(None, None)
        .with_colour(Colour::Never)
        .with_writer(writer.clone())
}

fn capture(config: LogConfig, emit: impl FnOnce()) -> LogHistory {
    let (subscriber, history) = config.build();
    tracing::subscriber::with_default(subscriber, emit);
    history
}

fn record(level: Level, target: &str, message: &str) -> LogRecord {
    LogRecord {
        timestamp: std::time::SystemTime::now(),
        level,
        target: target.to_owned(),
        message: message.to_owned(),
        fields: Vec::new(),
    }
}

#[test]
fn history_evicts_oldest_records_at_capacity() {
    let history = LogHistory::new(3);
    for index in 0..5 {
        history.push(record(Level::INFO, "t", &format!("m{index}")));
    }
    let messages: Vec<_> = history.records().into_iter().map(|r| r.message).collect();
    assert_eq!(messages, ["m2", "m3", "m4"]);
    assert_eq!((history.len(), history.capacity()), (3, 3));
}

#[test]
fn zero_capacity_history_retains_nothing() {
    let history = LogHistory::new(0);
    history.push(record(Level::ERROR, "t", "dropped"));
    assert!(history.is_empty());
}

#[test]
fn clones_share_storage_and_clear_empties_it() {
    let history = LogHistory::new(4);
    let other = history.clone();
    history.push(record(Level::INFO, "t", "a"));
    assert_eq!(other.len(), 1);
    other.clear();
    assert!(history.is_empty());
}

#[test]
fn recent_returns_the_tail_oldest_first() {
    let history = LogHistory::new(10);
    for index in 0..6 {
        history.push(record(Level::INFO, "t", &index.to_string()));
    }
    let tail: Vec<_> = history.recent(2).into_iter().map(|r| r.message).collect();
    assert_eq!(tail, ["4", "5"]);
    assert_eq!(history.recent(99).len(), 6);
}

#[test]
fn query_filters_by_level_target_text_and_limit() {
    let history = LogHistory::new(10);
    history.push(record(Level::DEBUG, "engine::a", "chatter"));
    history.push(record(Level::WARN, "engine::a", "disk low"));
    history.push(record(Level::ERROR, "engine::b", "disk gone"));
    history.push(record(Level::INFO, "other", "disk ok"));
    let messages = |query: LogQuery| -> Vec<String> {
        history
            .query(&query)
            .into_iter()
            .map(|r| r.message)
            .collect()
    };
    assert_eq!(
        messages(LogQuery {
            level: Some(Level::WARN),
            ..Default::default()
        }),
        ["disk low", "disk gone"]
    );
    assert_eq!(
        messages(LogQuery {
            target_prefix: Some("engine::".into()),
            contains: Some("disk".into()),
            ..Default::default()
        }),
        ["disk low", "disk gone"]
    );
    assert_eq!(
        messages(LogQuery {
            contains: Some("disk".into()),
            limit: Some(1),
            ..Default::default()
        }),
        ["disk ok"]
    );
    assert_eq!(messages(LogQuery::default()).len(), 4);
}

#[test]
fn events_are_captured_with_level_target_message_and_fields() {
    let writer = TestWriter::default();
    let history = capture(
        config(&writer).with_default_level(LevelFilter::TRACE),
        || {
            tracing::info!(target: "engine::demo", entity = 7_u32, name = "hero", "spawned");
        },
    );
    let records = history.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.level, Level::INFO);
    assert_eq!(record.target, "engine::demo");
    assert_eq!(record.message, "spawned");
    assert_eq!(record.field("entity"), Some("7"));
    assert_eq!(record.field("name"), Some("hero"));
    assert_eq!(record.field("missing"), None);
    assert!(record.timestamp <= std::time::SystemTime::now());
}

#[test]
fn default_level_filters_both_history_and_console() {
    let writer = TestWriter::default();
    let history = capture(
        config(&writer).with_default_level(LevelFilter::WARN),
        || {
            tracing::info!("quiet");
            tracing::warn!("loud");
        },
    );
    let messages: Vec<_> = history.records().into_iter().map(|r| r.message).collect();
    assert_eq!(messages, ["loud"]);
    assert!(writer.text().contains("loud"));
    assert!(!writer.text().contains("quiet"));
}

#[test]
fn engine_log_wins_over_rust_log_and_default() {
    let writer = TestWriter::default();
    let history = capture(
        config(&writer).with_env_directives(Some("debug"), Some("error")),
        || {
            tracing::debug!("seen");
        },
    );
    assert_eq!(history.len(), 1);
}

#[test]
fn rust_log_is_used_when_engine_log_is_unset_or_invalid() {
    for engine_log in [None, Some(""), Some("not=a=level=")] {
        let writer = TestWriter::default();
        let history = capture(
            config(&writer).with_env_directives(engine_log, Some("trace")),
            || tracing::trace!("seen"),
        );
        assert_eq!(history.len(), 1, "engine_log={engine_log:?}");
    }
}

#[test]
fn per_target_directives_apply() {
    let writer = TestWriter::default();
    let history = capture(
        config(&writer).with_env_directives(Some("error,engine::noisy=trace"), None),
        || {
            tracing::info!(target: "engine::other", "dropped");
            tracing::trace!(target: "engine::noisy", "kept");
        },
    );
    let messages: Vec<_> = history.records().into_iter().map(|r| r.message).collect();
    assert_eq!(messages, ["kept"]);
}

#[test]
fn resolve_filter_falls_back_to_the_default_level() {
    let filter = resolve_filter(None, Some("   "), LevelFilter::ERROR);
    assert_eq!(filter.max_level_hint(), Some(LevelFilter::ERROR));
}

#[test]
fn console_output_goes_to_the_configured_writer_without_colour_when_never() {
    let writer = TestWriter::default();
    capture(config(&writer), || tracing::info!("plain text"));
    let text = writer.text();
    assert!(text.contains("plain text"));
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn always_colour_emits_ansi_escapes() {
    let writer = TestWriter::default();
    capture(config(&writer).with_colour(Colour::Always), || {
        tracing::warn!("coloured")
    });
    assert!(writer.text().contains('\u{1b}'));
}

#[test]
fn history_is_queryable_from_other_threads() {
    let writer = TestWriter::default();
    let (subscriber, history) = config(&writer).build();
    let dispatch = tracing::Dispatch::new(subscriber);
    let workers: Vec<_> = (0..4)
        .map(|index| {
            let dispatch = dispatch.clone();
            std::thread::spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    for n in 0..10 {
                        tracing::info!(worker = index, n, "tick");
                    }
                });
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(history.len(), 40);
}

const CHILD_ENV: &str = "ENGINE_LOG_TEST_CHILD";

/// Runs in a child process (see `global_init_writes_only_to_stderr`).
#[test]
fn child_global_init_emits() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let handle = init(LogConfig::new().with_colour(Colour::Never)).expect("first init succeeds");
    tracing::error!("child-marker-message");
    assert_eq!(handle.history().len(), 1);
    assert_eq!(
        init(LogConfig::new()).err(),
        Some(InitError::AlreadyInitialized)
    );
}

#[test]
fn global_init_writes_only_to_stderr_and_double_init_errors() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_global_init_emits", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env_remove("RUST_LOG")
        .env_remove("ENGINE_LOG")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "child failed: {stdout}\n{stderr}");
    assert!(stderr.contains("child-marker-message"), "stderr: {stderr}");
    assert!(!stdout.contains("child-marker-message"), "stdout: {stdout}");
}
