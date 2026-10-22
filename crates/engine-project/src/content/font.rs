use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontGlyph {
    pub source: [u32; 4],
    pub advance: f32,
    pub bearing: [f32; 2],
}
#[derive(Clone, Debug)]
pub struct FontAtlas {
    pub texture: String,
    pub line_height: f32,
    pub fallback: u32,
    pub glyphs: BTreeMap<u32, FontGlyph>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FontFile {
    schema: u32,
    texture: String,
    line_height: f32,
    fallback: u32,
    glyphs: Vec<GlyphFile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlyphFile {
    codepoint: u32,
    source: [u32; 4],
    advance: f32,
    bearing: [f32; 2],
}
pub fn load_font(path: &Path) -> Result<FontAtlas> {
    let raw: FontFile = serde_json::from_slice(&read_bounded(path, 4 * 1024 * 1024)?)?;
    if raw.schema != 1
        || raw.glyphs.is_empty()
        || raw.glyphs.len() > 65536
        || raw.texture.is_empty()
        || !raw.line_height.is_finite()
        || raw.line_height <= 0.0
    {
        return Err(Error::new(
            "asset.font.invalid",
            "Invalid font schema or metrics",
        ));
    }
    let mut glyphs = BTreeMap::new();
    for glyph in raw.glyphs {
        if char::from_u32(glyph.codepoint).is_none()
            || glyphs
                .insert(
                    glyph.codepoint,
                    FontGlyph {
                        source: glyph.source,
                        advance: glyph.advance,
                        bearing: glyph.bearing,
                    },
                )
                .is_some()
        {
            return Err(Error::new(
                "asset.font.invalid",
                "Duplicate or invalid Unicode codepoint",
            ));
        }
    }
    let font = FontAtlas {
        texture: raw.texture,
        line_height: raw.line_height,
        fallback: raw.fallback,
        glyphs,
    };
    if !font.glyphs.contains_key(&font.fallback) {
        return Err(Error::new(
            "asset.font.invalid",
            "Font fallback glyph is missing",
        ));
    }
    Ok(font)
}
impl FontAtlas {
    pub fn validate(&self, width: u32, height: u32) -> Result<()> {
        for glyph in self.glyphs.values() {
            let r = glyph.source;
            if !glyph.advance.is_finite()
                || glyph.advance < 0.0
                || !glyph.bearing.iter().all(|v| v.is_finite())
                || r[0].checked_add(r[2]).is_none_or(|v| v > width)
                || r[1].checked_add(r[3]).is_none_or(|v| v > height)
            {
                return Err(Error::new(
                    "asset.font.invalid",
                    "Glyph exceeds atlas or has invalid metrics",
                ));
            }
        }
        Ok(())
    }
    pub fn glyph(&self, c: char) -> &FontGlyph {
        self.glyphs
            .get(&(c as u32))
            .unwrap_or(&self.glyphs[&self.fallback])
    }
}
