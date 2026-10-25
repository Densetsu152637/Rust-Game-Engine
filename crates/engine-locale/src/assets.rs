//! Per-locale font and asset mapping (plain data).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::locale::LocaleId;

/// Fonts and asset overrides for one locale.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocaleAssets {
    /// Font family names in priority order. The text engine's
    /// `FallbackConfig::with_language(locale, fonts)` consumes this later.
    pub fonts: Vec<String>,
    /// Asset key -> replacement asset key (for example a localized logo).
    pub asset_overrides: BTreeMap<String, String>,
}

/// Locale -> [`LocaleAssets`] table with fallback resolution.
///
/// Lookups walk the fallback chain (`fr-CA`, `fr`, default): fonts come from the first entry
/// with a non-empty list; an asset key is replaced by the first entry that overrides it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocaleAssetMap {
    entries: BTreeMap<LocaleId, LocaleAssets>,
}

impl LocaleAssetMap {
    /// Empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces the entry for `locale`.
    pub fn insert(&mut self, locale: LocaleId, assets: LocaleAssets) {
        self.entries.insert(locale, assets);
    }

    /// The entry stored for exactly `locale`.
    pub fn get(&self, locale: &LocaleId) -> Option<&LocaleAssets> {
        self.entries.get(locale)
    }

    /// Entries in locale order.
    pub fn iter(&self) -> impl Iterator<Item = (&LocaleId, &LocaleAssets)> {
        self.entries.iter()
    }

    /// `true` when no entries exist.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Font families for the first chain entry that lists any; empty when none do.
    pub fn fonts(&self, chain: &[LocaleId]) -> Vec<String> {
        chain
            .iter()
            .filter_map(|l| self.entries.get(l))
            .find(|a| !a.fonts.is_empty())
            .map(|a| a.fonts.clone())
            .unwrap_or_default()
    }

    /// The asset key to load for `key`: the first override found along `chain`, else `key`.
    pub fn resolve_asset(&self, chain: &[LocaleId], key: &str) -> String {
        chain
            .iter()
            .filter_map(|l| self.entries.get(l))
            .find_map(|a| a.asset_overrides.get(key))
            .map_or_else(|| key.to_string(), Clone::clone)
    }
}
