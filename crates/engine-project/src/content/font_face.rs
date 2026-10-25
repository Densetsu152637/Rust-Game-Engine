//! TrueType/OpenType font faces (asset kind `font_face`) and their optional
//! authored configuration (asset kind `font_config`).
//!
//! A `font_face` asset is the font file itself (`.ttf`, `.otf`, `.ttc` or
//! `.otc`). Faces register with the shaping text engine under a logical family
//! name: the asset ID, unless a `font_config` asset groups several files into one
//! family (for example a regular and a bold file). The config also carries the
//! default pixel size and atlas mode per family and the fallback chains.
use super::*;
use engine_text::{FallbackConfig, FontLibrary, FontStyle};
use std::{collections::BTreeSet, sync::Arc};

/// Largest accepted font file (the `engine-text` default font limit).
pub const MAX_FONT_FACE_BYTES: u64 = 32 * 1024 * 1024;
/// Largest accepted `font_config` document.
pub const MAX_FONT_CONFIG_BYTES: u64 = 256 * 1024;
/// Face assets and families one project may configure.
pub const MAX_FONT_FAMILIES: usize = 256;
/// Fallback families per list.
pub const MAX_FONT_FALLBACK_ENTRIES: usize = 64;
/// Pixel size of a family without an authored `size` (at `fontScale` 1).
pub const DEFAULT_FONT_SIZE: f32 = 16.0;
/// Authored pixel sizes are within `engine-text`'s size limits.
pub const FONT_SIZE_RANGE: std::ops::RangeInclusive<f32> = 1.0..=512.0;

const FACE_EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];

/// Which glyph atlas a family asks for.
///
/// `Sdf` is the seam for scalable and world-space text. Until the render
/// extension API reaches this branch, SDF requests are served by a coverage
/// atlas at the pixel size actually needed; see `EngineTextBackend::sdf_status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontAtlasMode {
    #[default]
    Coverage,
    Sdf,
}

/// One face found in a font file.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceSummary {
    pub family: String,
    pub weight: u16,
    pub italic: bool,
    /// Index inside a collection (0 for a plain font).
    pub index: u32,
}

/// A validated font file, kept as bytes until a text backend registers it.
#[derive(Clone)]
pub struct FontFace {
    pub bytes: Arc<[u8]>,
    pub faces: Vec<FaceSummary>,
}

impl std::fmt::Debug for FontFace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontFace")
            .field("bytes", &self.bytes.len())
            .field("faces", &self.faces)
            .finish()
    }
}

fn face_error(code: &str, message: impl Into<String>, path: &Path) -> Error {
    Error::new(code, message).at(path, "")
}

/// Load and validate a font file. Errors: `asset.font_face.extension`
/// (not `.ttf/.otf/.ttc/.otc`), `asset.font_face.too_large` (above 32 MiB) and
/// `asset.font_face.invalid` (not a parseable font; carries the `text.font.*`
/// reason).
pub fn load_font_face(path: &Path) -> Result<FontFace> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if !extension
        .as_deref()
        .is_some_and(|e| FACE_EXTENSIONS.contains(&e))
    {
        return Err(face_error(
            "asset.font_face.extension",
            "Font faces must be .ttf, .otf, .ttc or .otc files",
            path,
        ));
    }
    let bytes = read_bounded(path, MAX_FONT_FACE_BYTES).map_err(|error| {
        if error
            .diagnostics
            .first()
            .is_some_and(|d| d.code == "project.file.too_large")
        {
            face_error(
                "asset.font_face.too_large",
                format!("Font file exceeds {MAX_FONT_FACE_BYTES} bytes"),
                path,
            )
        } else {
            error
        }
    })?;
    parse_font_face(bytes).map_err(|error| error.at(path, ""))
}

/// Validate font bytes already in memory.
pub fn parse_font_face(bytes: Vec<u8>) -> Result<FontFace> {
    let bytes: Arc<[u8]> = bytes.into();
    let mut library = FontLibrary::new();
    library.load_font(bytes.clone()).map_err(|error| {
        Error::new(
            "asset.font_face.invalid",
            format!("Not a usable font file ({error})"),
        )
    })?;
    let faces = library
        .faces()
        .map(|(_, info)| FaceSummary {
            family: info.family.clone(),
            weight: info.weight,
            italic: info.style != FontStyle::Normal,
            index: info.index,
        })
        .collect();
    Ok(FontFace { bytes, faces })
}

/// Settings of one logical family.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct FontFamilySettings {
    /// Pixel size at `fontScale` 1; absent uses [`FontConfig::default_size`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    pub atlas: FontAtlasMode,
}

/// Settings of one `font_face` asset.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct FontFaceSettings {
    /// Logical family the file registers under; absent uses the asset ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
}

/// Fallback chains by logical family name (see `engine_text::FallbackConfig`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct FontFallback {
    pub default: Vec<String>,
    /// ISO 15924 script code (for example `Arab`) to families.
    pub by_script: BTreeMap<String, Vec<String>>,
    /// BCP 47 language prefix (for example `ja`, `zh-hant`) to families.
    pub by_language: BTreeMap<String, Vec<String>>,
}

/// Authored configuration (`font_config` asset, schema 1). At most one per project.
///
/// ```json
/// {"schema":1,"defaultFamily":"ui","defaultSize":16,
///  "faces":{"ui-bold":{"family":"ui"}},
///  "families":{"ui":{"size":18,"atlas":"coverage"}},
///  "fallback":{"default":["emoji"],"byScript":{"Arab":["naskh"]},"byLanguage":{"ja":["jp"]}}}
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct FontConfig {
    pub schema: u32,
    /// Family used by nodes whose `style.font` is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_family: Option<String>,
    pub default_size: f32,
    /// Per `font_face` asset ID.
    pub faces: BTreeMap<String, FontFaceSettings>,
    /// Per logical family name.
    pub families: BTreeMap<String, FontFamilySettings>,
    pub fallback: FontFallback,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            schema: 1,
            default_family: None,
            default_size: DEFAULT_FONT_SIZE,
            faces: BTreeMap::new(),
            families: BTreeMap::new(),
            fallback: FontFallback::default(),
        }
    }
}

fn config_error(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

impl FontConfig {
    /// Logical family of a face asset.
    pub fn family_of<'a>(&'a self, asset: &'a str) -> &'a str {
        self.faces
            .get(asset)
            .and_then(|s| s.family.as_deref())
            .unwrap_or(asset)
    }

    /// Settings of a family (case-insensitive name); defaults when unlisted.
    pub fn family_settings(&self, family: &str) -> FontFamilySettings {
        self.families
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(family))
            .map(|(_, s)| s.clone())
            .unwrap_or_default()
    }

    /// Pixel size of a family at `fontScale` 1.
    pub fn size_of(&self, family: &str) -> f32 {
        self.family_settings(family)
            .size
            .unwrap_or(self.default_size)
    }

    /// The `engine-text` fallback configuration these chains describe.
    pub fn fallback_config(&self) -> FallbackConfig {
        let mut config = FallbackConfig::new().with_default(self.fallback.default.iter());
        for (script, families) in &self.fallback.by_script {
            config = config.with_script(script, families.iter());
        }
        for (language, families) in &self.fallback.by_language {
            config = config.with_language(language, families.iter());
        }
        config
    }

    /// Range and shape checks that need no project context.
    pub fn validate(&self) -> Result<()> {
        let invalid = |message: &str| config_error("asset.font_config.invalid", message);
        if self.schema != 1 {
            return Err(invalid("Only font_config schema 1 is supported"));
        }
        let size_ok = |s: f32| s.is_finite() && FONT_SIZE_RANGE.contains(&s);
        if !size_ok(self.default_size) {
            return Err(invalid("defaultSize must be within 1..=512"));
        }
        if self.faces.len() > MAX_FONT_FAMILIES || self.families.len() > MAX_FONT_FAMILIES {
            return Err(invalid("At most 256 face and family entries"));
        }
        for (asset, settings) in &self.faces {
            if !valid_name(asset) || settings.family.as_deref().is_some_and(|f| !valid_name(f)) {
                return Err(invalid("Invalid face asset ID or family name"));
            }
        }
        for (family, settings) in &self.families {
            if !valid_name(family) || settings.size.is_some_and(|s| !size_ok(s)) {
                return Err(invalid("Invalid family name or size outside 1..=512"));
            }
        }
        let lists = std::iter::once(&self.fallback.default)
            .chain(self.fallback.by_script.values())
            .chain(self.fallback.by_language.values());
        let mut entries = 0;
        for list in lists {
            entries += list.len();
            if list.len() > MAX_FONT_FALLBACK_ENTRIES || list.iter().any(|f| !valid_name(f)) {
                return Err(invalid("Invalid or oversized fallback list"));
            }
        }
        if entries > MAX_FONT_FAMILIES
            || self.fallback.by_script.len() > MAX_FONT_FAMILIES
            || self.fallback.by_language.len() > MAX_FONT_FAMILIES
        {
            return Err(invalid("Too many fallback entries"));
        }
        let script_ok = |s: &String| s.len() == 4 && s.chars().all(|c| c.is_ascii_alphabetic());
        if !self.fallback.by_script.keys().all(script_ok)
            || self
                .fallback
                .by_language
                .keys()
                .any(|l| !valid_name(l) || l.len() > 64)
        {
            return Err(invalid(
                "Fallback keys: scripts are four-letter ISO 15924 codes, languages BCP 47 tags",
            ));
        }
        if self
            .default_family
            .as_deref()
            .is_some_and(|f| !valid_name(f))
        {
            return Err(invalid("Invalid defaultFamily"));
        }
        Ok(())
    }

    /// Cross-check against the project's `font_face` asset IDs: face keys must
    /// be face assets; default and fallback families must exist.
    pub fn validate_against(&self, faces: &BTreeSet<String>) -> Result<()> {
        self.validate()?;
        if let Some(asset) = self.faces.keys().find(|a| !faces.contains(*a)) {
            return Err(config_error(
                "asset.font_config.unknown_face",
                format!("faces refers to {asset}, which is not a font_face asset"),
            ));
        }
        let families: BTreeSet<String> = faces
            .iter()
            .map(|asset| self.family_of(asset).to_lowercase())
            .collect();
        let known = |family: &String| families.contains(&family.to_lowercase());
        let referenced = self
            .default_family
            .iter()
            .chain(self.families.keys())
            .chain(&self.fallback.default)
            .chain(self.fallback.by_script.values().flatten())
            .chain(self.fallback.by_language.values().flatten());
        if let Some(family) = referenced.into_iter().find(|f| !known(f)) {
            return Err(config_error(
                "asset.font_config.unknown_family",
                format!("{family} is not the family of any font_face asset"),
            ));
        }
        Ok(())
    }
}

/// Load a `font_config` document (range checks only; see
/// [`FontConfig::validate_against`]).
pub fn load_font_config(path: &Path) -> Result<FontConfig> {
    let config: FontConfig = serde_json::from_slice(&read_bounded(path, MAX_FONT_CONFIG_BYTES)?)
        .map_err(|error| Error::new("asset.font_config.invalid", error.to_string()).at(path, ""))?;
    config.validate().map_err(|error| error.at(path, ""))?;
    Ok(config)
}
