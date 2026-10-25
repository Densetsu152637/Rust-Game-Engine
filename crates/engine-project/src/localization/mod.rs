//! Authored localization: `string_table` and `locale_config` assets, the runtime
//! [`Localization`] resource and its string resolver.
//!
//! A `string_table` asset is one Project Fluent `.ftl` file; several tables may
//! make up one locale. A project declares at most one `locale_config` asset (a
//! JSON document in the `engine_locale::LocalizationSettings` shape) that lists
//! which table files belong to which locale, the default locale, pseudo
//! localization and per-locale fonts and asset overrides. The runtime builds one
//! [`Localization`] resource from them; UI text, bindings with a `message` key
//! and `engine.WorldText` read it. See `docs/localization.md`.
use crate::{
    Error, Project, Result, confined_file,
    schema::read_bounded,
    ui_tree::{UiStringResolver, validate_markup},
};
use bevy_ecs::prelude::Resource;
use engine_locale::{
    Diagnostic, DiagnosticKind, LocaleArgs, LocaleError, LocaleId, LocalizationSettings, Localizer,
    LocalizerConfig, PseudoOptions,
};
use serde::Serialize;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
    sync::{
        Arc, Mutex, PoisonError, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

pub mod ftl;

/// The localization library, re-exported so tools need no second dependency.
pub use engine_locale;

/// Largest accepted `string_table` file (the `engine-locale` resource limit).
pub const MAX_STRING_TABLE_BYTES: u64 = 1 << 20;
/// Largest accepted `locale_config` document.
pub const MAX_LOCALE_CONFIG_BYTES: u64 = 256 * 1024;
/// Name of the engine event emitted when the active locale changes.
pub const LOCALE_CHANGED_EVENT: &str = "locale.changed";
/// Entries kept in the static text cache of one generation.
const MAX_CACHE_ENTRIES: usize = 8192;

pub(crate) fn locale_error(error: LocaleError) -> Error {
    Error::new(error.code(), error.to_string())
}

fn config_error(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

fn slash(path: &Path) -> String {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Project-relative spelling used to compare `locale_config` paths to assets.
fn normalize_path(path: &str) -> String {
    slash(Path::new(&path.replace('\\', "/")))
}

// ---- assets ---------------------------------------------------------------

/// A validated `string_table` file.
#[derive(Clone, Debug)]
pub struct StringTable {
    pub text: String,
    /// Messages and terms defined.
    pub entries: usize,
}

/// Parse `.ftl` text for validation only (no locale is attached).
pub fn parse_string_table(bytes: &[u8]) -> Result<StringTable> {
    let mut scratch = Localizer::default();
    let entries = scratch.load_ftl_bytes("en", bytes).map_err(locale_error)?;
    let text = String::from_utf8_lossy(bytes).into_owned();
    Ok(StringTable { text, entries })
}

/// Load and validate a `string_table` (`.ftl`) file. Errors carry the stable
/// `locale.*` codes of `engine-locale` (`locale.parse`, `locale.resource_too_large`,
/// `locale.invalid_utf8`, `locale.duplicate_message`, ...) and the file path.
pub fn load_string_table(path: &Path) -> Result<StringTable> {
    if path.extension().and_then(|e| e.to_str()) != Some("ftl") {
        return Err(Error::new(
            "asset.string_table.extension",
            "String tables must be .ftl files",
        )
        .at(path, ""));
    }
    let bytes = read_bounded(path, MAX_STRING_TABLE_BYTES).map_err(|error| {
        if error
            .diagnostics
            .first()
            .is_some_and(|d| d.code == "project.file.too_large")
        {
            Error::new(
                "locale.resource_too_large",
                format!("String table exceeds {MAX_STRING_TABLE_BYTES} bytes"),
            )
            .at(path, "")
        } else {
            error
        }
    })?;
    parse_string_table(&bytes).map_err(|error| {
        let first = error.diagnostics.first().cloned();
        match first {
            Some(d) => Error::new(&d.code, d.message).at(path, ""),
            None => error,
        }
    })
}

fn check_keys(scope: &str, map: &Map<String, Value>, allowed: &[&str]) -> Result<()> {
    if let Some(key) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(config_error(
            "asset.locale_config.invalid",
            format!(
                "Unknown field `{key}` in {scope}; allowed: {}",
                allowed.join(", ")
            ),
        ));
    }
    Ok(())
}

/// Parse a `locale_config` document. Unknown fields are rejected (the settings
/// type itself ignores them), `schema` must be 1 when present, and
/// `LocalizationSettings::validate` runs.
pub fn parse_locale_config(bytes: &[u8]) -> Result<LocalizationSettings> {
    let invalid = |message: String| config_error("asset.locale_config.invalid", message);
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|error| invalid(error.to_string()))?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| invalid("The document must be a JSON object".into()))?;
    check_keys(
        "the document",
        root,
        &[
            "schema",
            "default_locale",
            "available_locales",
            "resources",
            "pseudo",
            "assets",
        ],
    )?;
    if let Some(schema) = root.remove("schema")
        && schema != 1
    {
        return Err(invalid("Only locale_config schema 1 is supported".into()));
    }
    for (name, allowed) in [
        ("resources", &["locale", "paths"][..]),
        ("assets", &["locale", "fonts", "asset_overrides"][..]),
    ] {
        for item in root
            .get(name)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(map) = item.as_object() {
                check_keys(name, map, allowed)?;
            }
        }
    }
    if let Some(map) = root.get("pseudo").and_then(Value::as_object) {
        check_keys(
            "pseudo",
            map,
            &[
                "enabled",
                "accents",
                "expansion_percent",
                "prefix",
                "suffix",
                "rtl",
            ],
        )?;
    }
    let settings: LocalizationSettings =
        serde_json::from_value(value).map_err(|error| invalid(error.to_string()))?;
    settings.validate().map_err(locale_error)?;
    Ok(settings)
}

/// Load a `locale_config` document (settings checks only; see
/// [`validate_project`] for the checks that need the project).
pub fn load_locale_config(path: &Path) -> Result<LocalizationSettings> {
    let bytes = read_bounded(path, MAX_LOCALE_CONFIG_BYTES)?;
    parse_locale_config(&bytes).map_err(|error| {
        let first = error.diagnostics.first().cloned();
        match first {
            Some(d) => Error::new(&d.code, d.message).at(path, ""),
            None => error,
        }
    })
}

/// The project's `locale_config` and where it lives.
#[derive(Clone, Debug)]
pub struct LocaleConfigInfo {
    /// Asset ID of the `locale_config`.
    pub asset: String,
    /// Project-relative path of the document.
    pub path: String,
    pub settings: LocalizationSettings,
}

impl LocaleConfigInfo {
    /// `string_table` paths of `locale` (compared in canonical form).
    pub fn tables_of(&self, locale: &str) -> Vec<String> {
        let Ok(wanted) = LocaleId::parse(locale) else {
            return vec![];
        };
        self.settings
            .resources
            .iter()
            .filter(|r| LocaleId::parse(&r.locale).is_ok_and(|id| id == wanted))
            .flat_map(|r| r.paths.iter().map(|p| normalize_path(p)))
            .collect()
    }
}

/// The project's `locale_config`, if it declares one (`None` otherwise).
/// More than one is `asset.locale_config.duplicate`.
pub fn locale_config(project: &Project) -> Result<Option<LocaleConfigInfo>> {
    let mut found = project
        .assets
        .values()
        .filter(|a| a.kind == "locale_config");
    let Some(asset) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(config_error(
            "asset.locale_config.duplicate",
            "A project may declare at most one locale_config asset",
        ));
    }
    let file = project.resolve_asset(&asset.id)?;
    Ok(Some(LocaleConfigInfo {
        asset: asset.id.clone(),
        path: slash(&asset.path),
        settings: load_locale_config(&file)?,
    }))
}

/// Families the project's text engine knows: `font_face` asset IDs and the
/// logical families a `font_config` assigns them.
fn known_families(project: &Project) -> Result<BTreeSet<String>> {
    let faces: Vec<&str> = project
        .assets
        .values()
        .filter(|a| a.kind == "font_face")
        .map(|a| a.id.as_str())
        .collect();
    let config = project
        .assets
        .values()
        .find(|a| a.kind == "font_config")
        .map(|a| crate::content::load_font_config(&project.resolve_asset(&a.id)?))
        .transpose()?;
    let mut out = BTreeSet::new();
    for face in faces {
        out.insert(face.to_lowercase());
        if let Some(config) = &config {
            out.insert(config.family_of(face).to_lowercase());
        }
    }
    Ok(out)
}

/// Cross-check `info` against the project's assets and load every table.
fn check_against_project(project: &Project, info: &LocaleConfigInfo) -> Result<()> {
    let tables: BTreeMap<String, &str> = project
        .assets
        .values()
        .filter(|a| a.kind == "string_table")
        .map(|a| (slash(&a.path), a.id.as_str()))
        .collect();
    let at = |error: Error| error.at(&info.path, "");
    let mut listed = BTreeSet::new();
    for entry in &info.settings.resources {
        for path in &entry.paths {
            let normalized = normalize_path(path);
            if !tables.contains_key(&normalized) {
                return Err(at(config_error(
                    "asset.locale_config.unknown_table",
                    format!(
                        "resources of {} lists {path}, which is not a string_table asset",
                        entry.locale
                    ),
                )));
            }
            listed.insert(normalized);
        }
    }
    if let Some((path, id)) = tables.iter().find(|(path, _)| !listed.contains(*path)) {
        return Err(config_error(
            "asset.string_table.unlisted",
            format!("string_table {id} ({path}) is not listed in the locale_config resources"),
        ));
    }
    for locale in &info.settings.available_locales {
        if info.tables_of(locale).is_empty() {
            return Err(at(config_error(
                "asset.locale_config.no_strings",
                format!("Available locale {locale} lists no string_table in resources"),
            )));
        }
    }
    let families = known_families(project)?;
    let mut sources = project
        .assets
        .keys()
        .map(|id| id.to_lowercase())
        .collect::<BTreeSet<_>>();
    sources.extend(families.iter().cloned());
    for entry in &info.settings.assets {
        for font in &entry.fonts {
            if !families.contains(&font.to_lowercase()) {
                return Err(at(config_error(
                    "asset.locale_config.unknown_font",
                    format!(
                        "Locale {} lists font {font}, which is not a font_face asset or family",
                        entry.locale
                    ),
                )));
            }
        }
        for (from, to) in &entry.asset_overrides {
            let exists = |key: &String| sources.contains(&key.to_lowercase());
            if !exists(from) || !exists(to) {
                return Err(at(config_error(
                    "asset.locale_config.unknown_asset",
                    format!(
                        "Locale {} overrides {from} with {to}; both must be asset IDs or font families",
                        entry.locale
                    ),
                )));
            }
        }
    }
    Ok(())
}

/// Build the localizer a project describes, reading every table. File context
/// is attached to load errors.
pub fn build_localizer(project: &Project) -> Result<Option<(LocaleConfigInfo, Localizer)>> {
    let Some(info) = locale_config(project)? else {
        return Ok(None);
    };
    check_against_project(project, &info)?;
    let settings = &info.settings;
    let mut localizer = Localizer::new(LocalizerConfig {
        default_locale: LocaleId::parse(&settings.default_locale).map_err(locale_error)?,
        ..LocalizerConfig::default()
    });
    for entry in &settings.resources {
        for path in &entry.paths {
            let file = confined_file(&project.root, Path::new(&normalize_path(path)))?;
            let bytes = read_bounded(&file, MAX_STRING_TABLE_BYTES)?;
            localizer
                .load_ftl_bytes(&entry.locale, &bytes)
                .map_err(|error| locale_error(error).at(path, ""))?;
        }
    }
    localizer
        .set_pseudo(settings.pseudo.clone())
        .map_err(locale_error)?;
    localizer.set_assets(settings.asset_map().map_err(locale_error)?);
    Ok(Some((info, localizer)))
}

/// Project-level validation of the localization assets (called by
/// `Project::validate`): the single `locale_config`, its references and a full
/// load of every table (duplicate message ids across a locale's tables).
pub fn validate_project(project: &Project) -> Result<()> {
    let has_tables = project.assets.values().any(|a| a.kind == "string_table");
    if locale_config(project)?.is_none() {
        if has_tables {
            return Err(config_error(
                "asset.string_table.no_config",
                "string_table assets need a locale_config asset that lists them",
            ));
        }
        return Ok(());
    }
    build_localizer(project).map(|_| ())
}

// ---- runtime --------------------------------------------------------------

/// Counters and state of the runtime localization, for tests and telemetry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LocalizationStats {
    /// Fluent `format` calls made for UI, bindings and world text.
    pub resolutions: u64,
    /// Static references answered from the per-generation cache.
    pub cache_hits: u64,
    pub generation: u64,
    /// Occurrences of references to keys no locale defines.
    pub missing_messages: u64,
}

/// Bounded summary of the active localization for run reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalizationStatus {
    pub locale: String,
    pub default_locale: String,
    pub generation: u64,
    pub pseudo: bool,
    pub locales: Vec<String>,
    pub resolutions: u64,
    pub missing_messages: u64,
}

#[derive(Default)]
struct Cache {
    generation: u64,
    texts: HashMap<String, String>,
    assets: HashMap<String, String>,
}

struct Shared {
    localizer: RwLock<Localizer>,
    cache: Mutex<Cache>,
    resolutions: AtomicU64,
    cache_hits: AtomicU64,
    /// Generation the UI last re-rendered its bindings at; `u64::MAX` before the first.
    ui_seen: AtomicU64,
    active: AtomicBool,
    has_assets: AtomicBool,
}

/// The runtime localization (resource): a shared [`Localizer`] plus a
/// per-generation cache of static references. Cheap to clone; clones share
/// state. Reads take a shared lock, mutations an exclusive one; both are short.
///
/// A default `Localization` has no catalogs and is *inactive*: UI text is left
/// as authored. It becomes active when a catalog is loaded, by the project's
/// `locale_config` or natively through [`Localization::load_ftl`].
#[derive(Clone, Resource)]
pub struct Localization {
    shared: Arc<Shared>,
}

impl Default for Localization {
    fn default() -> Self {
        Self::new(Localizer::default())
    }
}

impl std::fmt::Debug for Localization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Localization")
            .field("locale", &self.locale())
            .field("generation", &self.generation())
            .field("active", &self.active())
            .finish()
    }
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

impl Localization {
    pub fn new(localizer: Localizer) -> Self {
        let this = Self {
            shared: Arc::new(Shared {
                localizer: RwLock::new(localizer),
                cache: Mutex::new(Cache::default()),
                resolutions: AtomicU64::new(0),
                cache_hits: AtomicU64::new(0),
                ui_seen: AtomicU64::new(u64::MAX),
                active: AtomicBool::new(false),
                has_assets: AtomicBool::new(false),
            }),
        };
        this.refresh_flags();
        this
    }

    /// The localization a project's `locale_config` describes; `None` when the
    /// project declares none.
    pub fn from_project(project: &Project) -> Result<Option<Self>> {
        Ok(build_localizer(project)?.map(|(_, localizer)| Self::new(localizer)))
    }

    fn refresh_flags(&self) {
        let (active, assets) = {
            let localizer = read(&self.shared.localizer);
            (
                !localizer.locales().is_empty(),
                !localizer.assets().is_empty(),
            )
        };
        self.shared.active.store(active, Ordering::Release);
        self.shared.has_assets.store(assets, Ordering::Release);
    }

    /// Whether any catalog is loaded. Inactive localization never rewrites UI text.
    pub fn active(&self) -> bool {
        self.shared.active.load(Ordering::Acquire)
    }

    /// Whether any locale lists asset overrides or fonts.
    pub fn has_asset_overrides(&self) -> bool {
        self.shared.has_assets.load(Ordering::Acquire)
    }

    /// Run `f` with shared access to the localizer (key lists, reports, diagnostics).
    pub fn with<R>(&self, f: impl FnOnce(&Localizer) -> R) -> R {
        f(&read(&self.shared.localizer))
    }

    /// Run `f` with exclusive access to the localizer (loading, switching).
    /// Anything that changes the localizer bumps its generation, which the UI
    /// picks up on its next tick.
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut Localizer) -> R) -> R {
        let result = f(&mut self
            .shared
            .localizer
            .write()
            .unwrap_or_else(PoisonError::into_inner));
        self.refresh_flags();
        result
    }

    pub fn locale(&self) -> String {
        self.with(|l| l.locale().to_string())
    }

    pub fn default_locale(&self) -> String {
        self.with(|l| l.default_locale().to_string())
    }

    pub fn generation(&self) -> u64 {
        self.with(Localizer::generation)
    }

    /// Switch language. Nothing reloads; the generation changes, and every
    /// string re-resolves on the next UI tick. `locale.unavailable` when
    /// neither the locale nor its fallbacks have a catalog.
    pub fn set_locale(&self, locale: &str) -> Result<u64> {
        self.with_mut(|l| l.set_locale(locale))
            .map_err(locale_error)
    }

    /// Add `.ftl` source text to `locale`. Returns the entries added.
    pub fn load_ftl(&self, locale: &str, source: &str) -> Result<usize> {
        self.with_mut(|l| l.load_ftl(locale, source))
            .map_err(locale_error)
    }

    /// Atomically replace all of `locale`'s messages with `source` (hot reload).
    pub fn replace_ftl(&self, locale: &str, source: &str) -> Result<usize> {
        self.with_mut(|l| l.replace_ftl(locale, source))
            .map_err(locale_error)
    }

    pub fn pseudo(&self) -> PseudoOptions {
        self.with(|l| l.pseudo().clone())
    }

    pub fn set_pseudo(&self, options: PseudoOptions) -> Result<()> {
        self.with_mut(|l| l.set_pseudo(options))
            .map_err(locale_error)
    }

    /// Toggle pseudo-localization, keeping the other options.
    pub fn set_pseudo_enabled(&self, enabled: bool) -> Result<()> {
        let mut options = self.pseudo();
        if options.enabled == enabled {
            return Ok(());
        }
        options.enabled = enabled;
        self.set_pseudo(options)
    }

    /// Format `key` now, bypassing the cache. Counted in [`LocalizationStats`].
    pub fn format(&self, key: &str, args: &LocaleArgs) -> String {
        self.shared.resolutions.fetch_add(1, Ordering::Relaxed);
        self.with(|l| l.format(key, args))
    }

    /// Text of a static (argument-free) message key for the current locale,
    /// formatted once per generation.
    pub fn text(&self, key: &str) -> String {
        let localizer = read(&self.shared.localizer);
        let generation = localizer.generation();
        let mut cache = self
            .shared
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if cache.generation != generation {
            *cache = Cache {
                generation,
                ..Cache::default()
            };
        }
        if let Some(text) = cache.texts.get(key) {
            self.shared.cache_hits.fetch_add(1, Ordering::Relaxed);
            return text.clone();
        }
        self.shared.resolutions.fetch_add(1, Ordering::Relaxed);
        let text = localizer.format(key, &LocaleArgs::new());
        if cache.texts.len() < MAX_CACHE_ENTRIES {
            cache.texts.insert(key.to_owned(), text.clone());
        }
        text
    }

    /// The asset key to use for `key` in the current locale (`key` itself when
    /// no override applies); cached per generation.
    pub fn asset(&self, key: &str) -> String {
        if !self.has_asset_overrides() {
            return key.to_owned();
        }
        let localizer = read(&self.shared.localizer);
        let generation = localizer.generation();
        let mut cache = self
            .shared
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if cache.generation != generation {
            *cache = Cache {
                generation,
                ..Cache::default()
            };
        }
        if let Some(found) = cache.assets.get(key) {
            return found.clone();
        }
        let resolved = localizer.resolve_asset(key);
        if cache.assets.len() < MAX_CACHE_ENTRIES {
            cache.assets.insert(key.to_owned(), resolved.clone());
        }
        resolved
    }

    /// Resolve authored text: a message reference (`@key`) becomes the
    /// localized text, an escaped literal (`@@...`) loses one `@`, anything else
    /// is `None` (leave as is). `markup` marks rich text: a translation whose
    /// markup the text engine rejects is shown literally (brackets escaped)
    /// instead of failing the frame. Inactive localization returns `None`.
    pub fn resolve_text(&self, text: &str, markup: bool) -> Option<String> {
        if !self.active() {
            return None;
        }
        match classify(text) {
            TextRef::Plain => None,
            TextRef::Escaped(rest) => Some(rest.to_owned()),
            TextRef::Key(key) => {
                let resolved = self.text(key);
                Some(if markup && validate_markup(&resolved).is_err() {
                    escape_markup(&resolved)
                } else {
                    resolved
                })
            }
        }
    }

    /// Diagnostics recorded by the localizer (missing messages, fallbacks, format errors).
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.with(Localizer::diagnostics)
    }

    pub fn stats(&self) -> LocalizationStats {
        let (generation, missing) = self.with(|l| {
            let missing = l
                .diagnostics()
                .iter()
                .filter(|d| d.kind == DiagnosticKind::MissingMessage)
                .map(|d| u64::from(d.count))
                .sum();
            (l.generation(), missing)
        });
        LocalizationStats {
            resolutions: self.shared.resolutions.load(Ordering::Relaxed),
            cache_hits: self.shared.cache_hits.load(Ordering::Relaxed),
            generation,
            missing_messages: missing,
        }
    }

    /// Bounded summary for run reports; `None` while inactive.
    pub fn status(&self) -> Option<LocalizationStatus> {
        if !self.active() {
            return None;
        }
        let stats = self.stats();
        Some(self.with(|l| {
            LocalizationStatus {
                locale: l.locale().to_string(),
                default_locale: l.default_locale().to_string(),
                generation: stats.generation,
                pseudo: l.pseudo().enabled,
                locales: l
                    .locales()
                    .iter()
                    .take(64)
                    .map(ToString::to_string)
                    .collect(),
                resolutions: stats.resolutions,
                missing_messages: stats.missing_messages,
            }
        }))
    }

    /// Whether the generation changed since the UI last asked; the UI then
    /// re-renders message bindings. The first call returns `true`.
    pub fn take_ui_dirty(&self) -> bool {
        let generation = self.generation();
        self.shared.ui_seen.swap(generation, Ordering::AcqRel) != generation
    }

    /// The string resolver for data bindings with a `message` key.
    pub fn resolver(&self) -> Arc<dyn UiStringResolver> {
        Arc::new(LocalizationResolver(self.clone()))
    }
}

struct LocalizationResolver(Localization);

fn locale_args(args: &BTreeMap<String, Value>) -> LocaleArgs {
    let mut out = LocaleArgs::new();
    for (name, value) in args {
        match value {
            Value::Number(n) => out = out.number(name, n.as_f64().unwrap_or(0.0)),
            Value::String(s) => out = out.text(name, s.clone()),
            Value::Bool(b) => out = out.text(name, b.to_string()),
            _ => {}
        }
    }
    out
}

impl UiStringResolver for LocalizationResolver {
    fn resolve(&self, key: &str, args: &BTreeMap<String, Value>) -> Option<String> {
        self.0
            .active()
            .then(|| self.0.format(key, &locale_args(args)))
    }
}

// ---- message references ---------------------------------------------------

/// How authored text relates to message keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRef<'a> {
    /// Ordinary text.
    Plain,
    /// `@key` or `@key.attribute`: a message reference.
    Key(&'a str),
    /// `@@...`: literal text; the payload is the text with one `@` removed.
    Escaped(&'a str),
}

fn fluent_ident(part: &str) -> bool {
    let mut chars = part.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Whether `text` is a Fluent key: `message` or `message.attribute`.
pub fn is_message_key(text: &str) -> bool {
    text.len() <= 256
        && match text.split_once('.') {
            Some((id, attribute)) => fluent_ident(id) && fluent_ident(attribute),
            None => fluent_ident(text),
        }
}

/// Classify authored text. The whole string must be the reference: `@greeting`
/// is a reference, `@ greeting`, `Hello @greeting` and `@bob!` are plain text.
pub fn classify(text: &str) -> TextRef<'_> {
    match text.strip_prefix('@') {
        Some(rest) if rest.starts_with('@') => TextRef::Escaped(rest),
        Some(rest) if is_message_key(rest) => TextRef::Key(rest),
        _ => TextRef::Plain,
    }
}

fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if matches!(c, '\\' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_requires_the_whole_string_to_be_a_key() {
        assert_eq!(classify("@greeting"), TextRef::Key("greeting"));
        assert_eq!(classify("@menu.title"), TextRef::Key("menu.title"));
        assert_eq!(classify("@@greeting"), TextRef::Escaped("@greeting"));
        assert_eq!(classify("@@"), TextRef::Escaped("@"));
        for plain in [
            "greeting",
            "@",
            "@ greeting",
            "Hello @x",
            "@bob!",
            "@1x",
            "@a.b.c",
        ] {
            assert_eq!(classify(plain), TextRef::Plain, "{plain}");
        }
    }

    #[test]
    fn inactive_localization_leaves_text_alone() {
        let l = Localization::default();
        assert!(!l.active());
        assert_eq!(l.resolve_text("@greeting", false), None);
        assert_eq!(l.resolve_text("@@greeting", false), None);
        l.load_ftl("en", "greeting = Hi\n").unwrap();
        assert_eq!(l.resolve_text("@greeting", false).as_deref(), Some("Hi"));
        assert_eq!(
            l.resolve_text("@@greeting", false).as_deref(),
            Some("@greeting")
        );
    }

    #[test]
    fn static_text_is_cached_per_generation() {
        let l = Localization::default();
        l.load_ftl("en", "a = A\n").unwrap();
        l.load_ftl("fr", "a = Aa\n").unwrap();
        for _ in 0..5 {
            assert_eq!(l.text("a"), "A");
        }
        assert_eq!(l.stats().resolutions, 1);
        assert_eq!(l.stats().cache_hits, 4);
        l.set_locale("fr").unwrap();
        assert_eq!(l.text("a"), "Aa");
        assert_eq!(l.text("a"), "Aa");
        assert_eq!(l.stats().resolutions, 2);
    }

    #[test]
    fn invalid_translated_markup_is_shown_literally() {
        let l = Localization::default();
        l.load_ftl("en", "ok = [b]x[/b]\n").unwrap();
        assert_eq!(l.resolve_text("@ok", true).as_deref(), Some("[b]x[/b]"));
        assert_eq!(escape_markup("a[b]\\"), "a\\[b\\]\\\\");
    }

    #[test]
    fn locale_config_rejects_unknown_fields_and_bad_schema() {
        let ok = br#"{"schema":1,"default_locale":"en","available_locales":["en"],
            "resources":[{"locale":"en","paths":["a.ftl"]}]}"#;
        assert!(parse_locale_config(ok).is_ok());
        for (doc, code) in [
            (
                r#"{"default_locale":"en","colour":1}"#,
                "asset.locale_config.invalid",
            ),
            (r#"{"schema":2}"#, "asset.locale_config.invalid"),
            (r#"[]"#, "asset.locale_config.invalid"),
            (
                r#"{"resources":[{"locale":"en","paths":[],"x":1}]}"#,
                "asset.locale_config.invalid",
            ),
            (
                r#"{"default_locale":"fr","available_locales":["en"]}"#,
                "locale.default_unavailable",
            ),
        ] {
            let error = parse_locale_config(doc.as_bytes()).unwrap_err();
            assert_eq!(error.diagnostics[0].code, code, "{doc}");
        }
    }

    #[test]
    fn string_tables_report_typed_errors() {
        let error = parse_string_table(b"a = {").unwrap_err();
        assert_eq!(error.diagnostics[0].code, "locale.parse");
        let error = parse_string_table(&[0xff, 0xfe]).unwrap_err();
        assert_eq!(error.diagnostics[0].code, "locale.invalid_utf8");
        let error = parse_string_table(b"a = 1\na = 2\n").unwrap_err();
        assert_eq!(error.diagnostics[0].code, "locale.duplicate_message");
        assert_eq!(parse_string_table(b"a = 1\n-t = 2\n").unwrap().entries, 2);
    }
}
