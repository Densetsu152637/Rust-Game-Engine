//! `engine-locale`: localization for the engine.
//!
//! * String tables in Project Fluent (`.ftl`) syntax with plurals, selectors (plural, gender,
//!   anything), terms, attributes and variables, via `fluent-bundle`.
//! * [`LocaleId`] parsing, fallback chains (`fr-CA` -> `fr` -> default) and [`negotiate`].
//! * [`Localizer`]: per-locale catalogs, `format`/attribute lookup, a visible missing-message
//!   marker plus diagnostics, a runtime language switch with a generation counter, key lists and
//!   missing-translation reports.
//! * [`LocaleAssetMap`]: per-locale font families and asset-key overrides as plain data.
//! * [`pseudo_localize`]: accents, expansion, markers and RTL mirroring that leave placeables
//!   and `[tag]` markup untouched.
//! * [`LocalizationSettings`]: the serde descriptor of authored localization settings.
//!
//! The crate has no engine, GPU, windowing or file-system dependency. See `docs/localization.md`.

mod args;
mod assets;
mod catalog;
mod diagnostics;
mod error;
mod limits;
mod locale;
mod localizer;
mod pseudo;
mod settings;

pub use args::{ArgValue, LocaleArgs};
pub use assets::{LocaleAssetMap, LocaleAssets};
pub use catalog::MessageInfo;
pub use diagnostics::{Diagnostic, DiagnosticKind};
pub use error::LocaleError;
pub use limits::LocaleLimits;
pub use locale::{LocaleId, MAX_LOCALE_BYTES, negotiate};
pub use localizer::{Localizer, LocalizerConfig, MissingReport, Resolved};
pub use pseudo::{MAX_EXPANSION_PERCENT, MAX_MARKER_CHARS, PseudoOptions, pseudo_localize};
pub use settings::{
    LocaleAssetEntry, LocaleResources, LocalizationSettings, MAX_FONTS_PER_LOCALE, MAX_NAME_BYTES,
    MAX_OVERRIDES_PER_LOCALE, MAX_PATHS_PER_LOCALE, MAX_SETTINGS_LOCALES,
};
