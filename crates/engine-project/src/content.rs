//! CPU-only loaders and geometry. Texture pixels are top-left RGBA8.
use crate::{Error, Result, schema::read_bounded};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}
pub fn load_mesh(path: &Path) -> Result<Mesh> {
    let bytes = read_bounded(path, 64 * 1024 * 1024)?;
    let source =
        std::str::from_utf8(&bytes).map_err(|e| Error::new("asset.mesh.invalid", e.to_string()))?;
    let mut tokens = source
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .flat_map(str::split_whitespace);
    if tokens.next() != Some("CGMESH") || tokens.next() != Some("1") {
        return Err(Error::new("asset.mesh.invalid", "Expected CGMESH 1"));
    }
    let mut mesh = Mesh::default();
    while let Some(token) = tokens.next() {
        if token != "vertex" || mesh.vertices.len() >= 1_000_000 {
            return Err(Error::new(
                "asset.mesh.invalid",
                "Expected bounded vertex list",
            ));
        }
        let mut n = [0.0_f32; 5];
        for v in &mut n {
            *v = tokens
                .next()
                .and_then(|v| v.parse().ok())
                .filter(|v: &f32| v.is_finite())
                .ok_or_else(|| {
                    Error::new("asset.mesh.invalid", "Vertex requires five finite numbers")
                })?;
        }
        mesh.vertices.push(Vertex {
            position: [n[0], n[1], n[2]],
            uv: [n[3], n[4]],
            color: [1.0; 4],
        });
    }
    if mesh.vertices.is_empty() || !mesh.vertices.len().is_multiple_of(3) {
        return Err(Error::new(
            "asset.mesh.invalid",
            "Mesh requires nonempty triangle list",
        ));
    }
    mesh.indices = (0..mesh.vertices.len() as u32).collect();
    Ok(mesh)
}
pub fn load_texture(path: &Path) -> Result<Texture> {
    let bytes = read_bounded(path, 128 * 1024 * 1024)?;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut reader =
            image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(256 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|e| Error::new("asset.texture.invalid", e.to_string()))?
            .into_rgba8();
        return Ok(Texture {
            width: image.width(),
            height: image.height(),
            pixels: image.into_raw(),
        });
    }
    let source = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new("asset.texture.invalid", "Expected PNG or PPM P3"))?;
    let mut tokens = source
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .flat_map(str::split_whitespace);
    if tokens.next() != Some("P3") {
        return Err(Error::new(
            "asset.texture.invalid",
            "Expected PNG or PPM P3",
        ));
    }
    let mut number = || {
        tokens
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| Error::new("asset.texture.invalid", "Expected PPM integer"))
    };
    let width = number()?;
    let height = number()?;
    let maximum = number()?;
    if width == 0 || height == 0 || width > 8192 || height > 8192 || maximum != 255 {
        return Err(Error::new(
            "asset.texture.invalid",
            "PPM requires bounded nonempty extent and max value 255",
        ));
    }
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for _ in 0..width * height {
        for _ in 0..3 {
            let v = number()?;
            if v > 255 {
                return Err(Error::new(
                    "asset.texture.invalid",
                    "PPM channel exceeds 255",
                ));
            }
            pixels.push(v as u8);
        }
        pixels.push(255);
    }
    if tokens.next().is_some() {
        return Err(Error::new("asset.texture.invalid", "Trailing PPM data"));
    }
    Ok(Texture {
        width,
        height,
        pixels,
    })
}
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
/// Quad corners are left/bottom world units. Atlas rectangles use top-left pixels.
pub fn sprite_quad(
    position: [f32; 3],
    size: [f32; 2],
    pivot: [f32; 2],
    source: [u32; 4],
    texture: &Texture,
    tint: [f32; 4],
) -> Result<Mesh> {
    if texture.width == 0
        || texture.height == 0
        || source[2] == 0
        || source[3] == 0
        || source[0]
            .checked_add(source[2])
            .is_none_or(|n| n > texture.width)
        || source[1]
            .checked_add(source[3])
            .is_none_or(|n| n > texture.height)
    {
        return Err(Error::new(
            "asset.sprite.rectangle",
            "Sprite rectangle exceeds texture",
        ));
    }
    let x = position[0] - size[0] * pivot[0];
    let y = position[1] - size[1] * pivot[1];
    let z = position[2];
    let u0 = source[0] as f32 / texture.width as f32;
    let u1 = (source[0] + source[2]) as f32 / texture.width as f32;
    let v0 = source[1] as f32 / texture.height as f32;
    let v1 = (source[1] + source[3]) as f32 / texture.height as f32;
    Ok(Mesh {
        vertices: vec![
            Vertex {
                position: [x, y, z],
                uv: [u0, v1],
                color: tint,
            },
            Vertex {
                position: [x + size[0], y, z],
                uv: [u1, v1],
                color: tint,
            },
            Vertex {
                position: [x + size[0], y + size[1], z],
                uv: [u1, v0],
                color: tint,
            },
            Vertex {
                position: [x, y + size[1], z],
                uv: [u0, v0],
                color: tint,
            },
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    })
}
pub fn animation_frame(elapsed: f64, fps: f32, count: usize, looping: bool) -> usize {
    if !elapsed.is_finite() || elapsed < 0.0 || !fps.is_finite() || fps <= 0.0 || count == 0 {
        return 0;
    }
    let frame = (elapsed * fps as f64).floor();
    if looping {
        (frame % (count as f64)) as usize
    } else {
        frame.min((count - 1) as f64) as usize
    }
}
/// Logical-pixel text uses top-left origins. Word wrapping precedes glyph breaks.
pub fn text_mesh(
    font: &FontAtlas,
    texture: &Texture,
    text: &str,
    origin: [f32; 2],
    wrap_width: f32,
    scale: f32,
    color: [f32; 4],
) -> Result<Mesh> {
    if text.len() > 65536
        || !scale.is_finite()
        || scale <= 0.0
        || !wrap_width.is_finite()
        || wrap_width < 0.0
    {
        return Err(Error::new(
            "ui.text.invalid",
            "Invalid text geometry parameters",
        ));
    }
    let mut mesh = Mesh::default();
    let (mut x, mut y) = (origin[0], origin[1]);
    for word in text.split_inclusive(char::is_whitespace) {
        let width: f32 = word
            .chars()
            .take_while(|c| !c.is_whitespace())
            .map(|c| font.glyph(c).advance * scale)
            .sum();
        if wrap_width > 0.0 && x > origin[0] && x + width > origin[0] + wrap_width {
            x = origin[0];
            y += font.line_height * scale;
        }
        for c in word.chars() {
            if c == '\r' {
                continue;
            }
            if c == '\n' {
                x = origin[0];
                y += font.line_height * scale;
                continue;
            }
            let glyph = font.glyph(c);
            let advance = glyph.advance * scale;
            if wrap_width > 0.0 && x > origin[0] && x + advance > origin[0] + wrap_width {
                x = origin[0];
                y += font.line_height * scale;
            }
            if glyph.source[2] > 0 && glyph.source[3] > 0 {
                // sprite_quad uses bottom-left geometry; invert Y for top-left UI.
                let mut quad = sprite_quad(
                    [
                        x + glyph.bearing[0] * scale,
                        -y - glyph.bearing[1] * scale,
                        0.0,
                    ],
                    [
                        glyph.source[2] as f32 * scale,
                        glyph.source[3] as f32 * scale,
                    ],
                    [0.0, 1.0],
                    glyph.source,
                    texture,
                    color,
                )?;
                for v in &mut quad.vertices {
                    v.position[1] = -v.position[1];
                }
                let base = mesh.vertices.len() as u32;
                mesh.vertices.extend(quad.vertices);
                mesh.indices
                    .extend(quad.indices.into_iter().map(|i| i + base));
            }
            x += advance;
        }
    }
    Ok(mesh)
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
}
pub fn camera_viewport(logical: [u32; 2], framebuffer: [u32; 2]) -> Viewport {
    if logical.contains(&0) || framebuffer.contains(&0) {
        return Viewport {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            scale: 0.0,
        };
    }
    let fit =
        (framebuffer[0] as f32 / logical[0] as f32).min(framebuffer[1] as f32 / logical[1] as f32);
    let scale = if fit >= 1.0 { fit.floor() } else { fit };
    let width = logical[0] as f32 * scale;
    let height = logical[1] as f32 * scale;
    Viewport {
        x: ((framebuffer[0] as f32 - width) / 2.0).floor(),
        y: ((framebuffer[1] as f32 - height) / 2.0).floor(),
        width,
        height,
        scale,
    }
}
pub fn logical_pointer(
    pointer: [f32; 2],
    window: [u32; 2],
    framebuffer: [u32; 2],
    logical: [u32; 2],
) -> [f32; 2] {
    let viewport = camera_viewport(logical, framebuffer);
    if window.contains(&0) || viewport.scale == 0.0 || !pointer.iter().all(|n| n.is_finite()) {
        return [-1.0; 2];
    }
    [
        (pointer[0] * framebuffer[0] as f32 / window[0] as f32 - viewport.x) / viewport.scale,
        (pointer[1] * framebuffer[1] as f32 / window[1] as f32 - viewport.y) / viewport.scale,
    ]
}
