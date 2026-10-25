//! Authored localization settings: a serde descriptor with validation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::assets::{LocaleAssetMap, LocaleAssets};
use crate::error::LocaleError;
use crate::locale::LocaleId;
use crate::localizer::{Localizer, LocalizerConfig};
use crate::pseudo::PseudoOptions;

/// Maximum available locales in settings.
pub const MAX_SETTINGS_LOCALES: usize = 64;
/// Maximum resource paths per locale.
pub const MAX_PATHS_PER_LOCALE: usize = 64;
/// Maximum bytes in a resource path, font name or asset key.
pub const MAX_NAME_BYTES: usize = 512;
/// Maximum fonts per locale.
pub const MAX_FONTS_PER_LOCALE: usize = 32;
/// Maximum asset overrides per locale.
pub const MAX_OVERRIDES_PER_LOCALE: usize = 1024;

/// `.ftl` files that make up one locale.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocaleResources {
    pub locale: String,
    /// Project-relative paths, loaded in order.
    pub paths: Vec<String>,
}

/// Fonts and asset overrides for one locale (see [`LocaleAssets`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocaleAssetEntry {
    pub locale: String,
    pub fonts: Vec<String>,
    pub asset_overrides: BTreeMap<String, String>,
}

/// The authored localization settings shape (future component / project setting).
///
/// ```json
/// {
///   "default_locale": "en",
///   "available_locales": ["en", "fr", "ja"],
///   "resources": [{ "locale": "en", "paths": ["strings/en/ui.ftl"] }],
///   "pseudo": { "enabled": false, "expansion_percent": 35 },
///   "assets": [{ "locale": "ja", "fonts": ["Noto Sans JP"], "asset_overrides": {} }]
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalizationSettings {
    pub default_locale: String,
    pub available_locales: Vec<String>,
    pub resources: Vec<LocaleResources>,
    pub pseudo: PseudoOptions,
    pub assets: Vec<LocaleAssetEntry>,
}

impl Default for LocalizationSettings {
    fn default() -> Self {
        Self {
            default_locale: "en".to_string(),
            available_locales: vec!["en".to_string()],
            resources: Vec::new(),
            pseudo: PseudoOptions::default(),
            assets: Vec::new(),
        }
    }
}

impl LocalizationSettings {
    /// Checks the whole descriptor; returns the first problem.
    ///
    /// Rejects: unparsable locales (`locale.invalid_id`), duplicate locales in
    /// `available_locales`, `resources` or `assets` (`locale.duplicate_locale`), a default that
    /// is not available (`locale.default_unavailable`), resources/assets for unavailable locales
    /// (`locale.unknown_locale`), empty paths
    /// (`locale.empty_path`), absolute / `..` / duplicate paths (`locale.bad_path`), bad pseudo
    /// options (`locale.invalid_pseudo`), malformed font or asset entries
    /// (`locale.invalid_asset`) and oversized lists (`locale.settings_too_large`).
    pub fn validate(&self) -> Result<(), LocaleError> {
        let default = LocaleId::parse(&self.default_locale)?;
        if self.available_locales.len() > MAX_SETTINGS_LOCALES {
            return Err(too_large("available_locales", MAX_SETTINGS_LOCALES));
        }
        let mut available = BTreeSet::new();
        for raw in &self.available_locales {
            let id = LocaleId::parse(raw)?;
            if !available.insert(id.clone()) {
                return Err(LocaleError::DuplicateLocale {
                    locale: id.to_string(),
                });
            }
        }
        if !available.contains(&default) {
            return Err(LocaleError::DefaultUnavailable {
                locale: default.to_string(),
            });
        }
        self.validate_resources(&available)?;
        self.pseudo.validate()?;
        self.validate_assets(&available)
    }

    fn validate_resources(&self, available: &BTreeSet<LocaleId>) -> Result<(), LocaleError> {
        if self.resources.len() > MAX_SETTINGS_LOCALES {
            return Err(too_large("resources", MAX_SETTINGS_LOCALES));
        }
        let mut seen_locales = BTreeSet::new();
        let mut seen_paths = BTreeSet::new();
        for entry in &self.resources {
            let id = LocaleId::parse(&entry.locale)?;
            if !available.contains(&id) {
                return Err(LocaleError::UnknownLocale {
                    locale: id.to_string(),
                });
            }
            if !seen_locales.insert(id.clone()) {
                return Err(LocaleError::DuplicateLocale {
                    locale: id.to_string(),
                });
            }
            if entry.paths.len() > MAX_PATHS_PER_LOCALE {
                return Err(too_large("paths", MAX_PATHS_PER_LOCALE));
            }
            for path in &entry.paths {
                validate_path(&id, path)?;
                if !seen_paths.insert(path.replace('\\', "/")) {
                    return Err(LocaleError::BadPath {
                        path: path.clone(),
                        reason: "listed more than once".to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_assets(&self, available: &BTreeSet<LocaleId>) -> Result<(), LocaleError> {
        if self.assets.len() > MAX_SETTINGS_LOCALES {
            return Err(too_large("assets", MAX_SETTINGS_LOCALES));
        }
        let mut seen = BTreeSet::new();
        for entry in &self.assets {
            let id = LocaleId::parse(&entry.locale)?;
            let bad = |reason: String| LocaleError::InvalidAsset {
                locale: id.to_string(),
                reason,
            };
            if !available.contains(&id) {
                return Err(LocaleError::UnknownLocale {
                    locale: id.to_string(),
                });
            }
            if !seen.insert(id.clone()) {
                return Err(LocaleError::DuplicateLocale {
                    locale: id.to_string(),
                });
            }
            if entry.fonts.len() > MAX_FONTS_PER_LOCALE {
                return Err(too_large("fonts", MAX_FONTS_PER_LOCALE));
            }
            if entry.asset_overrides.len() > MAX_OVERRIDES_PER_LOCALE {
                return Err(too_large("asset_overrides", MAX_OVERRIDES_PER_LOCALE));
            }
            for font in &entry.fonts {
                if font.trim().is_empty() || font.len() > MAX_NAME_BYTES {
                    return Err(bad(format!("font name {font:?} is empty or too long")));
                }
            }
            for (key, value) in &entry.asset_overrides {
                if key.trim().is_empty()
                    || value.trim().is_empty()
                    || key.len() > MAX_NAME_BYTES
                    || value.len() > MAX_NAME_BYTES
                {
                    return Err(bad(format!("asset override {key:?} is empty or too long")));
                }
            }
        }
        Ok(())
    }

    /// The asset table described by `assets`. Call [`Self::validate`] first.
    pub fn asset_map(&self) -> Result<LocaleAssetMap, LocaleError> {
        let mut map = LocaleAssetMap::new();
        for entry in &self.assets {
            map.insert(
                LocaleId::parse(&entry.locale)?,
                LocaleAssets {
                    fonts: entry.fonts.clone(),
                    asset_overrides: entry.asset_overrides.clone(),
                },
            );
        }
        Ok(map)
    }

    /// Validates, then builds a [`Localizer`]: every path is read through `load` (the crate does
    /// no file I/O), pseudo options and the asset map are applied, and the current locale is the
    /// default locale. `load` errors become `locale.load_failed`.
    pub fn build_localizer(
        &self,
        mut load: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Localizer, LocaleError> {
        self.validate()?;
        let mut localizer = Localizer::new(LocalizerConfig {
            default_locale: LocaleId::parse(&self.default_locale)?,
            ..LocalizerConfig::default()
        });
        for entry in &self.resources {
            for path in &entry.paths {
                let bytes = load(path).map_err(|detail| LocaleError::LoadFailed {
                    path: path.clone(),
                    detail,
                })?;
                localizer.load_ftl_bytes(&entry.locale, &bytes)?;
            }
        }
        localizer.set_pseudo(self.pseudo.clone())?;
        localizer.set_assets(self.asset_map()?);
        Ok(localizer)
    }
}

fn too_large(what: &str, max: usize) -> LocaleError {
    LocaleError::SettingsTooLarge {
        what: what.to_string(),
        max,
    }
}

fn validate_path(locale: &LocaleId, path: &str) -> Result<(), LocaleError> {
    let bad = |reason: &str| LocaleError::BadPath {
        path: path.chars().take(64).collect(),
        reason: reason.to_string(),
    };
    if path.trim().is_empty() {
        return Err(LocaleError::EmptyPath {
            locale: locale.to_string(),
        });
    }
    if path.len() > MAX_NAME_BYTES {
        return Err(bad("path too long"));
    }
    let normalized = path.replace('\\', "/");
    if normalized.starts_with('/') || normalized.as_bytes().get(1) == Some(&b':') {
        return Err(bad("must be project-relative"));
    }
    if normalized.split('/').any(|part| part == "..") {
        return Err(bad("must not contain `..`"));
    }
    Ok(())
}
