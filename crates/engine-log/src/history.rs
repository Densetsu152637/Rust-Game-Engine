use std::{
    collections::VecDeque,
    fmt::Write as _,
    sync::{Arc, Mutex, MutexGuard},
    time::SystemTime,
};

use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context};

/// One captured event.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRecord {
    pub timestamp: SystemTime,
    pub level: Level,
    pub target: String,
    /// The event's `message` field (empty if it had none).
    pub message: String,
    /// All other fields in declaration order, values rendered with `Debug`
    /// (strings are kept unquoted).
    pub fields: Vec<(String, String)>,
}

impl LogRecord {
    /// Returns the rendered value of a structured field.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Filter for [`LogHistory::query`]. All set criteria must match.
#[derive(Debug, Clone, Default)]
pub struct LogQuery {
    /// Keep records at this level or more severe (`WARN` keeps WARN and ERROR).
    pub level: Option<Level>,
    /// Keep records whose target starts with this prefix.
    pub target_prefix: Option<String>,
    /// Keep records whose message contains this text.
    pub contains: Option<String>,
    /// Keep only the most recent `limit` matches (still oldest first).
    pub limit: Option<usize>,
}

impl LogQuery {
    fn matches(&self, record: &LogRecord) -> bool {
        // In `tracing`, a more verbose level compares greater.
        self.level.is_none_or(|level| record.level <= level)
            && self
                .target_prefix
                .as_deref()
                .is_none_or(|prefix| record.target.starts_with(prefix))
            && self
                .contains
                .as_deref()
                .is_none_or(|text| record.message.contains(text))
    }
}

#[derive(Debug)]
struct Inner {
    capacity: usize,
    records: Mutex<VecDeque<LogRecord>>,
}

/// Bounded, thread-safe ring buffer of recent records. Clones share storage.
#[derive(Debug, Clone)]
pub struct LogHistory {
    inner: Arc<Inner>,
}

impl LogHistory {
    /// A capacity of zero retains nothing.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                capacity,
                records: Mutex::new(VecDeque::with_capacity(capacity.min(4096))),
            }),
        }
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Appends a record, evicting the oldest when full.
    pub fn push(&self, record: LogRecord) {
        if self.inner.capacity == 0 {
            return;
        }
        let mut records = self.lock();
        while records.len() >= self.inner.capacity {
            records.pop_front();
        }
        records.push_back(record);
    }

    /// All retained records, oldest first.
    pub fn records(&self) -> Vec<LogRecord> {
        self.lock().iter().cloned().collect()
    }

    /// The most recent `count` records, oldest first.
    pub fn recent(&self, count: usize) -> Vec<LogRecord> {
        let records = self.lock();
        let skip = records.len().saturating_sub(count);
        records.iter().skip(skip).cloned().collect()
    }

    /// Matching records, oldest first, truncated to the most recent `limit`.
    pub fn query(&self, query: &LogQuery) -> Vec<LogRecord> {
        let records = self.lock();
        let mut matched: Vec<_> = records
            .iter()
            .filter(|record| query.matches(record))
            .cloned()
            .collect();
        if let Some(limit) = query.limit {
            let excess = matched.len().saturating_sub(limit);
            matched.drain(..excess);
        }
        matched
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<LogRecord>> {
        // A panic while holding the lock cannot leave the deque inconsistent.
        self.inner
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// `tracing` layer that feeds a [`LogHistory`].
pub struct HistoryLayer {
    history: LogHistory,
}

impl HistoryLayer {
    pub fn new(history: LogHistory) -> Self {
        Self { history }
    }
}

#[derive(Default)]
struct RecordVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl Visit for RecordVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        } else {
            self.fields
                .push((field.name().to_owned(), value.to_owned()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut rendered = String::new();
        let _ = write!(rendered, "{value:?}");
        if field.name() == "message" {
            self.message = rendered;
        } else {
            self.fields.push((field.name().to_owned(), rendered));
        }
    }
}

impl<S: Subscriber> Layer<S> for HistoryLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = RecordVisitor::default();
        event.record(&mut visitor);
        let metadata = event.metadata();
        self.history.push(LogRecord {
            timestamp: SystemTime::now(),
            level: *metadata.level(),
            target: metadata.target().to_owned(),
            message: visitor.message,
            fields: visitor.fields,
        });
    }
}
