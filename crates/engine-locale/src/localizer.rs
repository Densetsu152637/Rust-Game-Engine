//! The runtime catalog: loading, lookup with fallback, language switching and reports.

use std::collections::{BTreeMap, BTreeSet};

use crate::args::LocaleArgs;
use crate::assets::LocaleAssetMap;
use crate::catalog::Catalog;
use crate::diagnostics::{Diagnostic, DiagnosticKind, DiagnosticLog};
use crate::error::LocaleError;
use crate::limits::LocaleLimits;
use crate::locale::{LocaleId, negotiate};
use crate::pseudo::{PseudoOptions, pseudo_localize};

/// Construction options for a [`Localizer`].
#[derive(Debug, Clone)]
pub struct LocalizerConfig {
    /// Last stop of every fallback chain. Default `en`.
    pub default_locale: LocaleId,
    pub limits: LocaleLimits,
    /// Wrap placeable values in Unicode isolation marks (FSI/PDI). Off by default because game
    /// text renderers show them as boxes; turn on for mixed-direction values.
    pub isolating: bool,
    /// Text before the key in the missing-message marker. Default `{`.
    pub missing_prefix: String,
    /// Text after the key in the missing-message marker. Default `}`.
    pub missing_suffix: String,
}

impl Default for LocalizerConfig {
    fn default() -> Self {
        Self {
            default_locale: LocaleId::parse("en").expect("`en` is a valid locale"),
            limits: LocaleLimits::default(),
            isolating: false,
            missing_prefix: "{".to_string(),
            missing_suffix: "}".to_string(),
        }
    }
}

/// A successful lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub text: String,
    /// The locale whose catalog answered (may be a fallback of the current locale).
    pub locale: LocaleId,
    /// The generation the answer belongs to; compare with [`Localizer::generation`].
    pub generation: u64,
    /// Fluent resolution errors (for example a missing variable); `text` still holds the
    /// partial output. Empty on a clean format.
    pub format_errors: Vec<String>,
}

/// Translation coverage of one locale against the default locale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingReport {
    pub locale: LocaleId,
    pub reference: LocaleId,
    /// Keys in the reference locale.
    pub total: usize,
    /// Reference keys this locale defines.
    pub translated: usize,
    /// Reference keys (`msg` or `msg.attr`) this locale lacks, sorted.
    pub missing: Vec<String>,
    /// Keys this locale defines that the reference lacks, sorted.
    pub extra: Vec<String>,
}

/// Owns per-locale catalogs and the current language.
///
/// Lookups take `&self` (diagnostics use interior mutability) so the localizer is `Send + Sync`;
/// loading and switching take `&mut self`. Wrap in a lock when sharing across threads.
pub struct Localizer {
    default_locale: LocaleId,
    limits: LocaleLimits,
    isolating: bool,
    missing_prefix: String,
    missing_suffix: String,
    current: LocaleId,
    generation: u64,
    catalogs: BTreeMap<LocaleId, Catalog>,
    assets: LocaleAssetMap,
    pseudo: PseudoOptions,
    diagnostics: DiagnosticLog,
}

impl Default for Localizer {
    fn default() -> Self {
        Self::new(LocalizerConfig::default())
    }
}

impl Localizer {
    /// An empty localizer whose current locale is the default locale.
    pub fn new(config: LocalizerConfig) -> Self {
        Self {
            current: config.default_locale.clone(),
            default_locale: config.default_locale,
            limits: config.limits,
            isolating: config.isolating,
            missing_prefix: config.missing_prefix,
            missing_suffix: config.missing_suffix,
            generation: 0,
            catalogs: BTreeMap::new(),
            assets: LocaleAssetMap::new(),
            pseudo: PseudoOptions::default(),
            diagnostics: DiagnosticLog::default(),
        }
    }

    // ---- loading -------------------------------------------------------------------------

    /// Adds `.ftl` source text to `locale` (creating the catalog on first use). Returns the number
    /// of messages and terms added. Fails without changing anything on a syntax error, a limit
    /// violation or an id already defined for that locale. Bumps the generation on success.
    pub fn load_ftl(&mut self, locale: &str, source: &str) -> Result<usize, LocaleError> {
        let id = LocaleId::parse(locale)?;
        let source = source.strip_prefix('\u{feff}').unwrap_or(source);
        let added = match self.catalogs.get_mut(&id) {
            Some(catalog) => catalog.add_source(&id, source, &self.limits)?,
            None => {
                self.check_locale_capacity()?;
                let mut catalog = Catalog::new(&id, self.isolating);
                let added = catalog.add_source(&id, source, &self.limits)?;
                self.catalogs.insert(id, catalog);
                added
            }
        };
        self.generation += 1;
        Ok(added)
    }

    /// [`Self::load_ftl`] for raw bytes (must be UTF-8; the size limit is checked first).
    pub fn load_ftl_bytes(&mut self, locale: &str, bytes: &[u8]) -> Result<usize, LocaleError> {
        if bytes.len() > self.limits.max_resource_bytes {
            return Err(LocaleError::ResourceTooLarge {
                bytes: bytes.len(),
                max: self.limits.max_resource_bytes,
            });
        }
        let text = std::str::from_utf8(bytes).map_err(|_| LocaleError::InvalidUtf8)?;
        self.load_ftl(locale, text)
    }

    /// Replaces the whole catalog of `locale` with `source` atomically (hot reload, editing).
    pub fn replace_ftl(&mut self, locale: &str, source: &str) -> Result<usize, LocaleError> {
        let id = LocaleId::parse(locale)?;
        let source = source.strip_prefix('\u{feff}').unwrap_or(source);
        if !self.catalogs.contains_key(&id) {
            self.check_locale_capacity()?;
        }
        let mut catalog = Catalog::new(&id, self.isolating);
        let added = catalog.add_source(&id, source, &self.limits)?;
        self.catalogs.insert(id, catalog);
        self.generation += 1;
        Ok(added)
    }

    /// Drops a locale's catalog. Returns whether it existed.
    pub fn remove_locale(&mut self, locale: &str) -> Result<bool, LocaleError> {
        let id = LocaleId::parse(locale)?;
        let existed = self.catalogs.remove(&id).is_some();
        if existed {
            self.generation += 1;
        }
        Ok(existed)
    }

    fn check_locale_capacity(&self) -> Result<(), LocaleError> {
        if self.catalogs.len() >= self.limits.max_locales {
            return Err(LocaleError::TooManyLocales {
                max: self.limits.max_locales,
            });
        }
        Ok(())
    }

    // ---- language switch -----------------------------------------------------------------

    /// The current locale.
    pub fn locale(&self) -> &LocaleId {
        &self.current
    }

    /// The default (last-resort) locale.
    pub fn default_locale(&self) -> &LocaleId {
        &self.default_locale
    }

    /// Counter bumped by every change that can alter output: a locale switch, loading or removing
    /// messages, changing pseudo-localization or the asset map. UI caches the value it resolved
    /// at and re-resolves when it differs.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Switches language at runtime; nothing is reloaded. Fails with `locale.unavailable` when
    /// neither the locale nor its fallbacks have a catalog. Setting the current locale again is a
    /// no-op that returns the unchanged generation. Returns the generation.
    pub fn set_locale(&mut self, locale: &str) -> Result<u64, LocaleError> {
        self.set_locale_id(LocaleId::parse(locale)?)
    }

    /// [`Self::set_locale`] for an already parsed identifier.
    pub fn set_locale_id(&mut self, locale: LocaleId) -> Result<u64, LocaleError> {
        if locale == self.current {
            return Ok(self.generation);
        }
        let chain = locale.fallback_chain(&self.default_locale);
        if !chain.iter().any(|l| self.catalogs.contains_key(l)) {
            return Err(LocaleError::Unavailable {
                locale: locale.to_string(),
            });
        }
        self.current = locale;
        self.generation += 1;
        Ok(self.generation)
    }

    /// The current locale's fallback chain (`fr-CA`, `fr`, default).
    pub fn fallback_chain(&self) -> Vec<LocaleId> {
        self.current.fallback_chain(&self.default_locale)
    }

    /// Loaded locales in order.
    pub fn locales(&self) -> Vec<LocaleId> {
        self.catalogs.keys().cloned().collect()
    }

    /// Best loaded locale for an ordered preference list; see [`negotiate`].
    pub fn negotiate(&self, requested: &[LocaleId]) -> LocaleId {
        negotiate(requested, &self.locales(), &self.default_locale)
    }

    // ---- lookup --------------------------------------------------------------------------

    /// Formats `key` (`message` or `message.attribute`) for the current locale, walking the
    /// fallback chain. A missing key yields the marker (`{key}` by default) and a diagnostic;
    /// this never fails.
    pub fn format(&self, key: &str, args: &LocaleArgs) -> String {
        self.resolve(key, args).text
    }

    /// Like [`Self::format`] but also reports which locale answered. Failures produce a marker
    /// result whose `locale` is the current locale.
    pub fn resolve(&self, key: &str, args: &LocaleArgs) -> Resolved {
        self.lookup(key, args).unwrap_or_else(|_| Resolved {
            text: self.marker(key),
            locale: self.current.clone(),
            generation: self.generation,
            format_errors: Vec::new(),
        })
    }

    /// Strict lookup: a missing key, bad key, oversized output or any Fluent resolution error is
    /// an `Err`.
    pub fn try_format(&self, key: &str, args: &LocaleArgs) -> Result<String, LocaleError> {
        let resolved = self.lookup(key, args)?;
        if resolved.format_errors.is_empty() {
            Ok(resolved.text)
        } else {
            Err(LocaleError::Format {
                key: key.to_string(),
                detail: resolved.format_errors.join("; "),
            })
        }
    }

    /// Formats an attribute: same as `format("message.attribute", ...)`.
    pub fn format_attribute(&self, message: &str, attribute: &str, args: &LocaleArgs) -> String {
        self.format(&format!("{message}.{attribute}"), args)
    }

    /// `true` when some locale in the current chain defines `key`.
    pub fn contains(&self, key: &str) -> bool {
        let Ok((id, attr)) = split_key(key, &self.limits) else {
            return false;
        };
        self.fallback_chain()
            .iter()
            .filter_map(|l| self.catalogs.get(l))
            .any(|c| c.defines(id, attr))
    }

    fn marker(&self, key: &str) -> String {
        format!("{}{}{}", self.missing_prefix, key, self.missing_suffix)
    }

    fn record(&self, kind: DiagnosticKind, key: &str, detail: String) {
        self.diagnostics.record(
            self.limits.max_diagnostics,
            kind,
            self.current.as_str(),
            key,
            detail,
        );
    }

    fn lookup(&self, key: &str, args: &LocaleArgs) -> Result<Resolved, LocaleError> {
        let (id, attr) = split_key(key, &self.limits)?;
        let fluent_args = args.to_fluent(self.limits.max_arg_bytes);
        for locale in self.fallback_chain() {
            let Some(catalog) = self.catalogs.get(&locale) else {
                continue;
            };
            let Some(formatted) = catalog.format(id, attr, &fluent_args) else {
                continue;
            };
            if formatted.text.len() > self.limits.max_output_bytes {
                self.record(
                    DiagnosticKind::OutputTooLarge,
                    key,
                    format!("{} bytes", formatted.text.len()),
                );
                return Err(LocaleError::OutputTooLarge {
                    key: key.to_string(),
                    bytes: formatted.text.len(),
                    max: self.limits.max_output_bytes,
                });
            }
            if !formatted.errors.is_empty() {
                let detail = formatted
                    .errors
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ");
                self.record(DiagnosticKind::FormatError, key, detail);
            }
            if locale != self.current {
                self.record(DiagnosticKind::FellBack, key, locale.to_string());
            }
            let text = if self.pseudo.enabled {
                pseudo_localize(&formatted.text, &self.pseudo)
            } else {
                formatted.text
            };
            return Ok(Resolved {
                text,
                locale,
                generation: self.generation,
                format_errors: formatted.errors,
            });
        }
        self.record(DiagnosticKind::MissingMessage, key, String::new());
        Err(LocaleError::MissingMessage {
            key: key.to_string(),
        })
    }

    // ---- introspection (feeds later management tools) -----------------------------------

    /// Every key of `locale` (`msg` for messages with a value, `msg.attr` per attribute), sorted.
    pub fn keys(&self, locale: &str) -> Result<Vec<String>, LocaleError> {
        let id = LocaleId::parse(locale)?;
        self.catalogs
            .get(&id)
            .map(|c| flat_keys(c).into_iter().collect())
            .ok_or(LocaleError::UnknownLocale {
                locale: id.to_string(),
            })
    }

    /// Union of the keys of all loaded locales, sorted.
    pub fn all_keys(&self) -> Vec<String> {
        let mut out = BTreeSet::new();
        for c in self.catalogs.values() {
            out.extend(flat_keys(c));
        }
        out.into_iter().collect()
    }

    /// Coverage of `locale` against the default locale's keys.
    pub fn missing_report(&self, locale: &str) -> Result<MissingReport, LocaleError> {
        let id = LocaleId::parse(locale)?;
        let own = self.catalogs.get(&id).ok_or(LocaleError::UnknownLocale {
            locale: id.to_string(),
        })?;
        let reference =
            self.catalogs
                .get(&self.default_locale)
                .ok_or(LocaleError::UnknownLocale {
                    locale: self.default_locale.to_string(),
                })?;
        let (own_keys, ref_keys) = (flat_keys(own), flat_keys(reference));
        let missing: Vec<String> = ref_keys.difference(&own_keys).cloned().collect();
        Ok(MissingReport {
            locale: id,
            reference: self.default_locale.clone(),
            total: ref_keys.len(),
            translated: ref_keys.len() - missing.len(),
            extra: own_keys.difference(&ref_keys).cloned().collect(),
            missing,
        })
    }

    /// [`Self::missing_report`] for every loaded locale except the default one.
    pub fn missing_reports(&self) -> Vec<MissingReport> {
        self.catalogs
            .keys()
            .filter(|l| **l != self.default_locale)
            .filter_map(|l| self.missing_report(l.as_str()).ok())
            .collect()
    }

    /// Recorded diagnostics (deduplicated; each carries an occurrence count).
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics.snapshot()
    }

    /// Diagnostics discarded because the log was full.
    pub fn diagnostics_dropped(&self) -> u64 {
        self.diagnostics.dropped()
    }

    /// Returns and clears the diagnostics.
    pub fn take_diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics.take()
    }

    /// The configured limits.
    pub fn limits(&self) -> &LocaleLimits {
        &self.limits
    }

    // ---- pseudo-localization -------------------------------------------------------------

    /// Current pseudo-localization options (applied to output only when `enabled`).
    pub fn pseudo(&self) -> &PseudoOptions {
        &self.pseudo
    }

    /// Replaces the pseudo-localization options and bumps the generation.
    pub fn set_pseudo(&mut self, options: PseudoOptions) -> Result<(), LocaleError> {
        options.validate()?;
        self.pseudo = options;
        self.generation += 1;
        Ok(())
    }

    // ---- fonts and assets ----------------------------------------------------------------

    /// The locale -> fonts/asset map.
    pub fn assets(&self) -> &LocaleAssetMap {
        &self.assets
    }

    /// Replaces the asset map and bumps the generation.
    pub fn set_assets(&mut self, assets: LocaleAssetMap) {
        self.assets = assets;
        self.generation += 1;
    }

    /// Font families for the current locale (first chain entry that lists any).
    pub fn fonts(&self) -> Vec<String> {
        self.assets.fonts(&self.fallback_chain())
    }

    /// Font families for an arbitrary locale.
    pub fn fonts_for(&self, locale: &LocaleId) -> Vec<String> {
        self.assets
            .fonts(&locale.fallback_chain(&self.default_locale))
    }

    /// The asset key to load for `key` under the current locale.
    pub fn resolve_asset(&self, key: &str) -> String {
        self.assets.resolve_asset(&self.fallback_chain(), key)
    }

    /// The asset key to load for `key` under an arbitrary locale.
    pub fn resolve_asset_for(&self, locale: &LocaleId, key: &str) -> String {
        self.assets
            .resolve_asset(&locale.fallback_chain(&self.default_locale), key)
    }
}

fn flat_keys(catalog: &Catalog) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (id, info) in catalog.messages() {
        if info.has_value {
            out.insert(id.clone());
        }
        for a in &info.attributes {
            out.insert(format!("{id}.{a}"));
        }
    }
    out
}

fn is_fluent_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Splits `message` or `message.attribute`, validating both parts as Fluent identifiers.
fn split_key<'k>(
    key: &'k str,
    limits: &LocaleLimits,
) -> Result<(&'k str, Option<&'k str>), LocaleError> {
    let invalid = |reason: &str| LocaleError::InvalidKey {
        key: key.chars().take(64).collect(),
        reason: reason.to_string(),
    };
    if key.len() > limits.max_key_bytes {
        return Err(invalid("key too long"));
    }
    let (id, attr) = match key.split_once('.') {
        Some((id, attr)) => (id, Some(attr)),
        None => (key, None),
    };
    if !is_fluent_ident(id) || attr.is_some_and(|a| !is_fluent_ident(a)) {
        return Err(invalid(
            "not a Fluent identifier (`message` or `message.attribute`)",
        ));
    }
    Ok((id, attr))
}
