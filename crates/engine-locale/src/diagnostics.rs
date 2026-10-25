//! Bounded diagnostics log (missing messages, fallbacks, format errors).

use std::sync::Mutex;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// No locale in the chain defines the key; the visible marker was used.
    MissingMessage,
    /// The key was not defined in the requested locale and resolved from a fallback.
    FellBack,
    /// Fluent reported errors while formatting (the partial output was used).
    FormatError,
    /// The formatted output exceeded the limit; the visible marker was used.
    OutputTooLarge,
}

impl DiagnosticKind {
    /// Stable code, `locale.diag.*`.
    pub fn code(self) -> &'static str {
        match self {
            Self::MissingMessage => "locale.diag.missing_message",
            Self::FellBack => "locale.diag.fell_back",
            Self::FormatError => "locale.diag.format_error",
            Self::OutputTooLarge => "locale.diag.output_too_large",
        }
    }
}

/// One deduplicated diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    /// The locale that was requested.
    pub locale: String,
    pub key: String,
    /// Extra detail (the fallback locale that answered, or the format error text).
    pub detail: String,
    /// How many times this (kind, locale, key) occurred.
    pub count: u32,
}

#[derive(Debug, Default)]
struct Log {
    entries: Vec<Diagnostic>,
    dropped: u64,
}

/// Thread-safe bounded log owned by a [`crate::Localizer`].
#[derive(Debug, Default)]
pub(crate) struct DiagnosticLog {
    inner: Mutex<Log>,
}

impl DiagnosticLog {
    pub(crate) fn record(
        &self,
        max: usize,
        kind: DiagnosticKind,
        locale: &str,
        key: &str,
        detail: String,
    ) {
        let mut log = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = log
            .entries
            .iter_mut()
            .find(|e| e.kind == kind && e.locale == locale && e.key == key)
        {
            e.count = e.count.saturating_add(1);
            return;
        }
        if log.entries.len() >= max {
            log.dropped = log.dropped.saturating_add(1);
            return;
        }
        log.entries.push(Diagnostic {
            kind,
            locale: locale.to_string(),
            key: key.to_string(),
            detail,
            count: 1,
        });
    }

    pub(crate) fn snapshot(&self) -> Vec<Diagnostic> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .clone()
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).dropped
    }

    pub(crate) fn take(&self) -> Vec<Diagnostic> {
        let mut log = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        log.dropped = 0;
        std::mem::take(&mut log.entries)
    }
}
