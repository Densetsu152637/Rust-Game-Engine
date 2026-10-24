//! Layout → renderer-neutral glyph quads, plus a world-space transform helper.

use glam::{Mat4, Vec2, Vec3};

use crate::{AtlasMode, Color, FontLibrary, GlyphAtlas, Rect, TextError, TextLayout, UvRect};

/// One textured quad in layout pixels (y-down), sampled from atlas page `page`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphQuad {
    pub page: u32,
    pub uv: UvRect,
    pub rect: Rect,
    pub color: Color,
}

/// A quad after a 3D transform; corners are top-left, top-right, bottom-right, bottom-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldQuad {
    pub page: u32,
    pub uv: UvRect,
    pub corners: [Vec3; 4],
    pub color: Color,
}

/// Builds quads for every visible glyph and underline of `layout`, placed at `origin`.
///
/// Coverage atlases snap each pen position to whole pixels, so bitmaps rasterised at the exact
/// size map 1:1 onto screen texels. SDF atlases scale the base-size bitmap by
/// `glyph size / base_px` and keep fractional positions. Inline images are not quads; use
/// [`TextLayout::images`].
pub fn build_quads(
    layout: &TextLayout,
    library: &FontLibrary,
    atlas: &mut GlyphAtlas,
    origin: Vec2,
) -> Result<Vec<GlyphQuad>, TextError> {
    let mode = atlas.config().mode;
    let mut quads = Vec::with_capacity(layout.glyphs.len() + layout.decorations.len());
    for glyph in &layout.glyphs {
        let Some(entry) = atlas.glyph(
            library,
            glyph.face,
            glyph.glyph_id,
            glyph.size,
            glyph.synthesis,
        )?
        else {
            continue;
        };
        let pen = origin + Vec2::new(glyph.x, glyph.y);
        let (pen, scale) = match mode {
            AtlasMode::Coverage => (pen.round(), 1.0),
            AtlasMode::Sdf { base_px, .. } => (pen, glyph.size / base_px),
        };
        quads.push(GlyphQuad {
            page: entry.page,
            uv: entry.uv,
            rect: Rect::new(
                pen.x + entry.left * scale,
                pen.y + entry.top * scale,
                entry.rect.width as f32 * scale,
                entry.rect.height as f32 * scale,
            ),
            color: glyph.color,
        });
    }
    if !layout.decorations.is_empty() {
        let (page, uv) = atlas.white_uv();
        for decoration in &layout.decorations {
            let r = decoration.rect;
            quads.push(GlyphQuad {
                page,
                uv,
                rect: Rect::new(r.x + origin.x, r.y + origin.y, r.width, r.height),
                color: decoration.color,
            });
        }
    }
    Ok(quads)
}

/// Applies `transform` to every quad corner (`z = 0` in layout space). Layout space is y-down
/// pixels, so world-space text usually wants a transform such as
/// `Mat4::from_scale(Vec3::new(s, -s, 1.0))` composed with the entity's world matrix.
pub fn transform_quads(quads: &[GlyphQuad], transform: Mat4) -> Vec<WorldQuad> {
    quads
        .iter()
        .map(|q| {
            let r = q.rect;
            let corner = |x: f32, y: f32| transform.transform_point3(Vec3::new(x, y, 0.0));
            WorldQuad {
                page: q.page,
                uv: q.uv,
                corners: [
                    corner(r.x, r.y),
                    corner(r.right(), r.y),
                    corner(r.right(), r.bottom()),
                    corner(r.x, r.bottom()),
                ],
                color: q.color,
            }
        })
        .collect()
}
