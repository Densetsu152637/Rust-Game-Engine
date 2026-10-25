//! Typed errors with stable `locale.*` codes.

use std::fmt;

/// Every failure the crate reports. [`LocaleError::code`] is stable and machine readable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocaleError {
    /// A locale identifier did not parse (`locale.invalid_id`).
    InvalidLocale { input: String, reason: String },
    /// A resource exceeded `max_resource_bytes` (`locale.resource_too_large`).
    ResourceTooLarge { bytes: usize, max: usize },
    /// Resource bytes were not UTF-8 (`locale.invalid_utf8`).
    InvalidUtf8,
    /// The `.ftl` source had syntax errors (`locale.parse`).
    Parse {
        count: usize,
        line: usize,
        detail: String,
    },
    /// More messages and terms than `max_messages_per_locale` (`locale.too_many_messages`).
    TooManyMessages { count: usize, max: usize },
    /// Placeables or call arguments nested deeper than the limit (`locale.too_deep`).
    TooDeep { depth: usize, max: usize },
    /// More locales loaded than `max_locales` (`locale.too_many_locales`).
    TooManyLocales { max: usize },
    /// A message or term id is defined twice for one locale (`locale.duplicate_message`).
    DuplicateMessage { locale: String, id: String },
    /// The locale has no loaded catalog (`locale.unknown_locale`).
    UnknownLocale { locale: String },
    /// Neither the locale nor its fallbacks have a catalog (`locale.unavailable`).
    Unavailable { locale: String },
    /// No locale in the chain defines the message (`locale.missing_message`).
    MissingMessage { key: String },
    /// A message key or attribute name is malformed (`locale.invalid_key`).
    InvalidKey { key: String, reason: String },
    /// Fluent reported resolution errors (`locale.format_failed`).
    Format { key: String, detail: String },
    /// A formatted message exceeded `max_output_bytes` (`locale.output_too_large`).
    OutputTooLarge {
        key: String,
        bytes: usize,
        max: usize,
    },
    /// A locale is listed twice in settings (`locale.duplicate_locale`).
    DuplicateLocale { locale: String },
    /// The default locale is not an available locale (`locale.default_unavailable`).
    DefaultUnavailable { locale: String },
    /// A resource path is empty (`locale.empty_path`).
    EmptyPath { locale: String },
    /// A resource path is absolute, contains `..` or is duplicated (`locale.bad_path`).
    BadPath { path: String, reason: String },
    /// Pseudo-localization options are out of range (`locale.invalid_pseudo`).
    InvalidPseudo { reason: String },
    /// A font or asset-override entry is malformed (`locale.invalid_asset`).
    InvalidAsset { locale: String, reason: String },
    /// Settings list more entries than allowed (`locale.settings_too_large`).
    SettingsTooLarge { what: String, max: usize },
    /// The caller-supplied loader failed for a path (`locale.load_failed`).
    LoadFailed { path: String, detail: String },
}

impl LocaleError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidLocale { .. } => "locale.invalid_id",
            Self::ResourceTooLarge { .. } => "locale.resource_too_large",
            Self::InvalidUtf8 => "locale.invalid_utf8",
            Self::Parse { .. } => "locale.parse",
            Self::TooManyMessages { .. } => "locale.too_many_messages",
            Self::TooDeep { .. } => "locale.too_deep",
            Self::TooManyLocales { .. } => "locale.too_many_locales",
            Self::DuplicateMessage { .. } => "locale.duplicate_message",
            Self::UnknownLocale { .. } => "locale.unknown_locale",
            Self::Unavailable { .. } => "locale.unavailable",
            Self::MissingMessage { .. } => "locale.missing_message",
            Self::InvalidKey { .. } => "locale.invalid_key",
            Self::Format { .. } => "locale.format_failed",
            Self::OutputTooLarge { .. } => "locale.output_too_large",
            Self::DuplicateLocale { .. } => "locale.duplicate_locale",
            Self::DefaultUnavailable { .. } => "locale.default_unavailable",
            Self::EmptyPath { .. } => "locale.empty_path",
            Self::BadPath { .. } => "locale.bad_path",
            Self::InvalidPseudo { .. } => "locale.invalid_pseudo",
            Self::InvalidAsset { .. } => "locale.invalid_asset",
            Self::SettingsTooLarge { .. } => "locale.settings_too_large",
            Self::LoadFailed { .. } => "locale.load_failed",
        }
    }
}

impl fmt::Display for LocaleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.code())?;
        match self {
            Self::InvalidLocale { input, reason } => {
                write!(f, "invalid locale {input:?}: {reason}")
            }
            Self::ResourceTooLarge { bytes, max } => {
                write!(f, "resource is {bytes} bytes, limit is {max}")
            }
            Self::InvalidUtf8 => write!(f, "resource is not valid UTF-8"),
            Self::Parse {
                count,
                line,
                detail,
            } => write!(f, "{count} syntax error(s), first at line {line}: {detail}"),
            Self::TooManyMessages { count, max } => {
                write!(f, "{count} messages and terms, limit is {max}")
            }
            Self::TooDeep { depth, max } => write!(f, "nesting depth {depth} exceeds limit {max}"),
            Self::TooManyLocales { max } => write!(f, "more than {max} locales"),
            Self::DuplicateMessage { locale, id } => {
                write!(f, "message or term {id:?} is already defined for {locale}")
            }
            Self::UnknownLocale { locale } => write!(f, "no catalog for locale {locale}"),
            Self::Unavailable { locale } => {
                write!(f, "locale {locale} has no catalog in its fallback chain")
            }
            Self::MissingMessage { key } => write!(f, "no message {key:?} in any fallback locale"),
            Self::InvalidKey { key, reason } => write!(f, "invalid key {key:?}: {reason}"),
            Self::Format { key, detail } => write!(f, "formatting {key:?} failed: {detail}"),
            Self::OutputTooLarge { key, bytes, max } => {
                write!(f, "output of {key:?} is {bytes} bytes, limit is {max}")
            }
            Self::DuplicateLocale { locale } => write!(f, "locale {locale} is listed twice"),
            Self::DefaultUnavailable { locale } => {
                write!(f, "default locale {locale} is not in available_locales")
            }
            Self::EmptyPath { locale } => write!(f, "empty resource path for locale {locale}"),
            Self::BadPath { path, reason } => write!(f, "bad resource path {path:?}: {reason}"),
            Self::InvalidPseudo { reason } => write!(f, "invalid pseudo options: {reason}"),
            Self::InvalidAsset { locale, reason } => {
                write!(f, "invalid asset entry for {locale}: {reason}")
            }
            Self::SettingsTooLarge { what, max } => write!(f, "too many {what} (limit {max})"),
            Self::LoadFailed { path, detail } => write!(f, "could not load {path:?}: {detail}"),
        }
    }
}

impl std::error::Error for LocaleError {}
