//! Font loading, face metadata, the font library and family/fallback matching.

use std::collections::BTreeMap;
use std::sync::Arc;

use skrifa::instance::{LocationRef, Size};
use skrifa::raw::FileRef;
use skrifa::string::StringId;
use skrifa::{FontRef, MetadataProvider};

use crate::{TextError, TextLimits};

/// Stable handle of a registered face inside one [`FontLibrary`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceId(pub(crate) u32);

impl FaceId {
    /// Registration index (dense, starting at zero).
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Slant of a face or of a request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontStyle {
    #[default]
    Normal,
    Italic,
    Oblique,
}

/// CSS-style numeric weights.
pub mod weight {
    pub const THIN: u16 = 100;
    pub const LIGHT: u16 = 300;
    pub const REGULAR: u16 = 400;
    pub const MEDIUM: u16 = 500;
    pub const BOLD: u16 = 700;
    pub const BLACK: u16 = 900;
}

/// Synthetic styling applied because the requested variant has no real face.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Synthesis {
    /// Outlines are emboldened (coverage double-strike, SDF edge offset) and advances widened.
    pub bold: bool,
    /// Outlines are skewed by [`SYNTHETIC_OBLIQUE_SKEW`].
    pub oblique: bool,
}

/// Horizontal skew (tan of ~12 degrees) applied for synthetic oblique.
pub const SYNTHETIC_OBLIQUE_SKEW: f32 = 0.2126;
/// Synthetic bold stroke widening as a fraction of the font size (1/24 em).
pub const SYNTHETIC_BOLD_EM: f32 = 1.0 / 24.0;

/// Face metadata read at load time.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceInfo {
    /// Typographic family (name ID 16) when present, otherwise the legacy family (ID 1),
    /// or the override given to [`FontLibrary::load_font_as`].
    pub family: String,
    /// OS/2 weight class (1..=1000).
    pub weight: u16,
    pub style: FontStyle,
    /// Width as a percentage of normal (100 = normal).
    pub stretch: f32,
    /// Face index inside its file (non-zero only for collections).
    pub index: u32,
    pub glyph_count: u16,
    pub units_per_em: u16,
}

/// Vertical metrics in font units, y-down sign convention (ascent and descent positive).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceMetrics {
    pub units_per_em: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
    /// Distance from the baseline down to the top of the underline.
    pub underline_offset: f32,
    pub underline_thickness: f32,
}

impl FaceMetrics {
    /// Metrics scaled to `size_px`.
    pub fn scaled(&self, size_px: f32) -> FaceMetrics {
        let s = size_px / self.units_per_em;
        FaceMetrics {
            units_per_em: self.units_per_em,
            ascent: self.ascent * s,
            descent: self.descent * s,
            line_gap: self.line_gap * s,
            underline_offset: self.underline_offset * s,
            underline_thickness: self.underline_thickness * s,
        }
    }
}

pub(crate) struct Face {
    data: Arc<[u8]>,
    pub(crate) info: FaceInfo,
    family_key: String,
    pub(crate) metrics: FaceMetrics,
    pub(crate) shaper_data: harfrust::ShaperData,
}

impl Face {
    pub(crate) fn font_ref(&self) -> Result<FontRef<'_>, TextError> {
        FontRef::from_index(&self.data, self.info.index).map_err(|e| TextError::FontInvalid {
            reason: e.to_string(),
        })
    }
}

/// Result of family + weight + style matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaceMatch {
    pub face: FaceId,
    pub synthesis: Synthesis,
}

/// Ordered fallback families: per-language overrides, then per-script overrides, then the
/// default list. Families are matched by name (case-insensitive) with the same weight/style
/// rules as the primary family.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FallbackConfig {
    /// Tried for every cluster after language and script overrides.
    pub default: Vec<String>,
    /// Keyed by ISO 15924 script tag in title case (`"Arab"`, `"Hani"`, `"Latn"`).
    pub by_script: BTreeMap<String, Vec<String>>,
    /// Keyed by lowercase BCP 47 tag (`"ja"`, `"zh-hant"`); lookups try the full layout
    /// language first and then shorter prefixes (`zh-hant-tw` → `zh-hant` → `zh`).
    pub by_language: BTreeMap<String, Vec<String>>,
}

impl FallbackConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the default list.
    pub fn with_default<I, S>(mut self, families: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.default = families.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the override list for one script (`"Arab"`; case is normalised).
    pub fn with_script<I, S>(mut self, script: &str, families: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.by_script.insert(
            normalize_script(script),
            families.into_iter().map(Into::into).collect(),
        );
        self
    }

    /// Sets the override list for one language (`"ja"`, `"zh-Hant"`; case is normalised).
    pub fn with_language<I, S>(mut self, language: &str, families: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.by_language.insert(
            normalize_language(language),
            families.into_iter().map(Into::into).collect(),
        );
        self
    }

    fn entry_count(&self) -> usize {
        self.default.len()
            + self.by_script.values().map(Vec::len).sum::<usize>()
            + self.by_language.values().map(Vec::len).sum::<usize>()
    }

    fn normalized(&self) -> FallbackConfig {
        FallbackConfig {
            default: self.default.clone(),
            by_script: self
                .by_script
                .iter()
                .map(|(k, v)| (normalize_script(k), v.clone()))
                .collect(),
            by_language: self
                .by_language
                .iter()
                .map(|(k, v)| (normalize_language(k), v.clone()))
                .collect(),
        }
    }

    /// Fallback family names for a cluster, in priority order.
    pub fn chain<'a>(
        &'a self,
        language: Option<&str>,
        script: &str,
    ) -> impl Iterator<Item = &'a str> + 'a {
        let mut lists: Vec<&'a [String]> = Vec::new();
        if let Some(language) = language {
            let mut tag = normalize_language(language);
            loop {
                if let Some(list) = self.by_language.get(&tag) {
                    lists.push(list);
                }
                match tag.rfind('-') {
                    Some(cut) => tag.truncate(cut),
                    None => break,
                }
            }
        }
        if let Some(list) = self.by_script.get(&normalize_script(script)) {
            lists.push(list);
        }
        lists.push(&self.default);
        lists.into_iter().flatten().map(String::as_str)
    }
}

fn normalize_script(script: &str) -> String {
    let mut out = String::with_capacity(4);
    for (i, c) in script.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

fn normalize_language(language: &str) -> String {
    language.trim().replace('_', "-").to_ascii_lowercase()
}

/// Registered faces plus fallback configuration and input limits.
///
/// Fonts are loaded only from bytes supplied by the caller; the library never discovers
/// or reads system fonts itself.
pub struct FontLibrary {
    faces: Vec<Face>,
    fallback: FallbackConfig,
    limits: TextLimits,
}

impl Default for FontLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl FontLibrary {
    pub fn new() -> Self {
        Self::with_limits(TextLimits::default())
    }

    pub fn with_limits(limits: TextLimits) -> Self {
        Self {
            faces: Vec::new(),
            fallback: FallbackConfig::default(),
            limits,
        }
    }

    pub fn limits(&self) -> &TextLimits {
        &self.limits
    }

    /// Loads every face of a TTF, OTF or TTC/OTC file. Registration is atomic: on error no
    /// face from this file is added.
    pub fn load_font(&mut self, bytes: impl Into<Arc<[u8]>>) -> Result<Vec<FaceId>, TextError> {
        self.load(bytes.into(), None)
    }

    /// Like [`FontLibrary::load_font`] but registers every face under `family`, so authored
    /// data can refer to a logical family name independent of the file's name table.
    pub fn load_font_as(
        &mut self,
        bytes: impl Into<Arc<[u8]>>,
        family: &str,
    ) -> Result<Vec<FaceId>, TextError> {
        self.load(bytes.into(), Some(family))
    }

    fn load(&mut self, data: Arc<[u8]>, family: Option<&str>) -> Result<Vec<FaceId>, TextError> {
        if data.len() > self.limits.max_font_bytes {
            return Err(TextError::FontTooLarge {
                size: data.len(),
                max: self.limits.max_font_bytes,
            });
        }
        let invalid = |e: skrifa::raw::ReadError| TextError::FontInvalid {
            reason: e.to_string(),
        };
        let count = match FileRef::new(&data).map_err(invalid)? {
            FileRef::Font(_) => 1,
            FileRef::Collection(collection) => collection.len(),
        };
        if count == 0 {
            return Err(TextError::FontInvalid {
                reason: "collection holds no faces".into(),
            });
        }
        if count as usize > self.limits.max_collection_faces {
            return Err(TextError::FontLimit {
                max: self.limits.max_collection_faces,
            });
        }
        if self.faces.len() + count as usize > self.limits.max_faces {
            return Err(TextError::FontLimit {
                max: self.limits.max_faces,
            });
        }
        let mut parsed = Vec::with_capacity(count as usize);
        for index in 0..count {
            parsed.push(parse_face(data.clone(), index, family)?);
        }
        let first = self.faces.len() as u32;
        self.faces.extend(parsed);
        Ok((first..first + count).map(FaceId).collect())
    }

    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    pub fn face_info(&self, face: FaceId) -> Option<&FaceInfo> {
        self.faces.get(face.index()).map(|f| &f.info)
    }

    pub fn face_metrics(&self, face: FaceId) -> Option<FaceMetrics> {
        self.faces.get(face.index()).map(|f| f.metrics)
    }

    /// All faces in registration order.
    pub fn faces(&self) -> impl Iterator<Item = (FaceId, &FaceInfo)> {
        self.faces
            .iter()
            .enumerate()
            .map(|(i, f)| (FaceId(i as u32), &f.info))
    }

    pub(crate) fn face(&self, face: FaceId) -> Option<&Face> {
        self.faces.get(face.index())
    }

    pub fn fallback(&self) -> &FallbackConfig {
        &self.fallback
    }

    /// Replaces the fallback configuration (keys are normalised).
    pub fn set_fallback(&mut self, fallback: FallbackConfig) -> Result<(), TextError> {
        if fallback.entry_count() > self.limits.max_fallback_entries {
            return Err(TextError::FallbackLimit {
                max: self.limits.max_fallback_entries,
            });
        }
        self.fallback = fallback.normalized();
        Ok(())
    }

    /// True when the face's character map has a glyph for `ch`.
    pub fn face_covers(&self, face: FaceId, ch: char) -> bool {
        self.face(face)
            .and_then(|f| f.font_ref().ok())
            .is_some_and(|font| font.charmap().map(ch).is_some())
    }

    /// Best face of `family` (case-insensitive) for the requested weight and style, following
    /// the CSS Fonts font-matching order for style then weight. Synthesis flags report a
    /// requested bold (>= 600) served by a face lighter than 600, or a requested italic/oblique
    /// served by an upright face.
    pub fn match_face(&self, family: &str, weight: u16, style: FontStyle) -> Option<FaceMatch> {
        let key = family.trim().to_lowercase();
        let (id, face) = self
            .faces
            .iter()
            .enumerate()
            .filter(|(_, f)| f.family_key == key)
            .min_by(|(ia, a), (ib, b)| {
                let ka = (
                    style_rank(style, a.info.style),
                    weight_rank(weight, a.info.weight),
                );
                let kb = (
                    style_rank(style, b.info.style),
                    weight_rank(weight, b.info.weight),
                );
                ka.cmp(&kb)
                    .then_with(|| {
                        (a.info.stretch - 100.0)
                            .abs()
                            .total_cmp(&(b.info.stretch - 100.0).abs())
                    })
                    .then(ia.cmp(ib))
            })?;
        Some(FaceMatch {
            face: FaceId(id as u32),
            synthesis: Synthesis {
                bold: weight >= 600 && face.info.weight < 600,
                oblique: style != FontStyle::Normal && face.info.style == FontStyle::Normal,
            },
        })
    }

    /// First registered face, used when nothing else matches.
    pub(crate) fn any_face(&self) -> Option<FaceMatch> {
        (!self.faces.is_empty()).then_some(FaceMatch {
            face: FaceId(0),
            synthesis: Synthesis::default(),
        })
    }
}

fn style_rank(requested: FontStyle, actual: FontStyle) -> u8 {
    use FontStyle::*;
    let order = match requested {
        Normal => [Normal, Oblique, Italic],
        Italic => [Italic, Oblique, Normal],
        Oblique => [Oblique, Italic, Normal],
    };
    order.iter().position(|s| *s == actual).unwrap_or(3) as u8
}

fn weight_rank(desired: u16, actual: u16) -> (u8, u16) {
    if (400..=500).contains(&desired) {
        if actual >= desired && actual <= 500 {
            (0, actual - desired)
        } else if actual < desired {
            (1, desired - actual)
        } else {
            (2, actual - 500)
        }
    } else if desired < 400 {
        if actual <= desired {
            (0, desired - actual)
        } else {
            (1, actual - desired)
        }
    } else if actual >= desired {
        (0, actual - desired)
    } else {
        (1, desired - actual)
    }
}

fn parse_face(data: Arc<[u8]>, index: u32, family: Option<&str>) -> Result<Face, TextError> {
    let invalid = |reason: String| TextError::FontInvalid { reason };
    let font = FontRef::from_index(&data, index).map_err(|e| invalid(e.to_string()))?;
    let raw = font.metrics(Size::unscaled(), LocationRef::default());
    if !(16..=16384).contains(&raw.units_per_em) {
        return Err(invalid(format!(
            "face {index}: units per em {} outside 16..=16384",
            raw.units_per_em
        )));
    }
    if raw.glyph_count == 0 {
        return Err(invalid(format!("face {index}: no glyphs")));
    }
    let attributes = font.attributes();
    let family = match family {
        Some(name) => name.trim().to_string(),
        None => family_name(&font).unwrap_or_else(|| format!("face-{index}")),
    };
    let upem = f32::from(raw.units_per_em);
    let underline = raw.underline.unwrap_or(skrifa::metrics::Decoration {
        offset: -0.1 * upem,
        thickness: 0.05 * upem,
    });
    let metrics = FaceMetrics {
        units_per_em: upem,
        ascent: raw.ascent.max(0.0),
        descent: (-raw.descent).max(0.0),
        line_gap: raw.leading.max(0.0),
        underline_offset: -underline.offset,
        underline_thickness: underline.thickness.max(upem / 100.0),
    };
    let info = FaceInfo {
        weight: attributes.weight.value().round().clamp(1.0, 1000.0) as u16,
        style: match attributes.style {
            skrifa::attribute::Style::Normal => FontStyle::Normal,
            skrifa::attribute::Style::Italic => FontStyle::Italic,
            skrifa::attribute::Style::Oblique(_) => FontStyle::Oblique,
        },
        stretch: attributes.stretch.ratio() * 100.0,
        index,
        glyph_count: raw.glyph_count,
        units_per_em: raw.units_per_em,
        family,
    };
    let shaper_data = harfrust::ShaperData::new(&font);
    let family_key = info.family.to_lowercase();
    Ok(Face {
        data,
        info,
        family_key,
        metrics,
        shaper_data,
    })
}

fn family_name(font: &FontRef<'_>) -> Option<String> {
    [StringId::TYPOGRAPHIC_FAMILY_NAME, StringId::FAMILY_NAME]
        .into_iter()
        .find_map(|id| {
            let strings: Vec<_> = font.localized_strings(id).collect();
            let english = strings
                .iter()
                .find(|s| s.language().is_some_and(|l| l.starts_with("en")));
            english
                .or(strings.first())
                .map(|s| s.to_string().trim().to_string())
                .filter(|s| !s.is_empty())
        })
}
