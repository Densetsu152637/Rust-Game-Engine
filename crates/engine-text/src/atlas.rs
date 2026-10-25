//! CPU glyph atlas: shelf-packed RGBA8 pages, page-granular LRU eviction and dirty tracking.
//!
//! Pages are RGBA8 with white RGB and coverage (or distance) in alpha, so a vertex colour
//! tints them directly (UI contract 2). Every page reserves a 4x4 opaque white block at its
//! origin for solid fills such as underlines ([`GlyphAtlas::white_uv`]).

use std::collections::HashMap;

use crate::font::{SYNTHETIC_BOLD_EM, SYNTHETIC_OBLIQUE_SKEW};
use crate::raster::{self, GlyphBitmap};
use crate::{FaceId, FontLibrary, PixelRect, Synthesis, TextError, UvRect};

/// What a page stores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AtlasMode {
    /// Anti-aliased coverage rasterised at each requested pixel size (crisp UI text).
    Coverage,
    /// Signed distance field rasterised once at `base_px` and scaled by the shader; `spread_px`
    /// is the distance (in base-size pixels) mapped across the full alpha range.
    Sdf { base_px: f32, spread_px: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasConfig {
    /// Page edge in pixels (64..=8192).
    pub page_size: u32,
    /// Most pages (1..=64); beyond this the least recently used page is evicted.
    pub max_pages: usize,
    pub mode: AtlasMode,
    /// Empty pixels around every glyph inside its bitmap (0..=8).
    pub padding: u32,
}

impl Default for AtlasConfig {
    fn default() -> Self {
        Self {
            page_size: 1024,
            max_pages: 4,
            mode: AtlasMode::Coverage,
            padding: 1,
        }
    }
}

impl AtlasConfig {
    /// SDF preset: 48 px base, 6 px spread.
    pub fn sdf() -> Self {
        Self {
            mode: AtlasMode::Sdf {
                base_px: 48.0,
                spread_px: 6.0,
            },
            ..Self::default()
        }
    }

    fn validate(&self) -> Result<(), TextError> {
        let bad = |reason| Err(TextError::AtlasConfig { reason });
        if !(64..=8192).contains(&self.page_size) {
            return bad("page_size must be in 64..=8192");
        }
        if !(1..=64).contains(&self.max_pages) {
            return bad("max_pages must be in 1..=64");
        }
        if self.padding > 8 {
            return bad("padding must be in 0..=8");
        }
        if let AtlasMode::Sdf { base_px, spread_px } = self.mode {
            if !(base_px.is_finite() && (8.0..=256.0).contains(&base_px)) {
                return bad("sdf base_px must be in 8..=256");
            }
            if !(spread_px.is_finite() && (1.0..=32.0).contains(&spread_px) && spread_px < base_px)
            {
                return bad("sdf spread_px must be in 1..=32 and below base_px");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct Shelf {
    y: u32,
    height: u32,
    x: u32,
}

/// One RGBA8 atlas page.
#[derive(Clone, Debug)]
pub struct AtlasPage {
    size: u32,
    pixels: Vec<u8>,
    shelves: Vec<Shelf>,
    next_y: u32,
    generation: u64,
    dirty: Option<PixelRect>,
    last_used: u64,
}

const WHITE_BLOCK: u32 = 4;

impl AtlasPage {
    fn new(size: u32, frame: u64) -> Self {
        let mut page = Self {
            size,
            pixels: Vec::new(),
            shelves: Vec::new(),
            next_y: 0,
            generation: 0,
            dirty: None,
            last_used: frame,
        };
        page.reset();
        page
    }

    fn reset(&mut self) {
        let texels = (self.size * self.size) as usize;
        self.pixels.clear();
        self.pixels.reserve(texels * 4);
        for _ in 0..texels {
            self.pixels.extend_from_slice(&[255, 255, 255, 0]);
        }
        self.shelves.clear();
        self.next_y = 0;
        self.generation += 1;
        self.dirty = Some(PixelRect {
            x: 0,
            y: 0,
            width: self.size,
            height: self.size,
        });
        if let Some((x, y)) = self.allocate(WHITE_BLOCK + 1, WHITE_BLOCK + 1) {
            for yy in y..y + WHITE_BLOCK {
                for xx in x..x + WHITE_BLOCK {
                    self.pixels[((yy * self.size + xx) * 4 + 3) as usize] = 255;
                }
            }
        }
    }

    /// Shelf packing: the shortest shelf that fits without wasting more than half the glyph
    /// height, else a new shelf, else any shelf tall enough.
    fn allocate(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.size || h > self.size {
            return None;
        }
        let size = self.size;
        let fits = |s: &Shelf| s.height >= h && s.x + w <= size;
        let snug = self
            .shelves
            .iter_mut()
            .filter(|s| fits(s) && s.height <= h + h / 2 + 2)
            .min_by_key(|s| s.height);
        if let Some(shelf) = snug {
            let at = (shelf.x, shelf.y);
            shelf.x += w;
            return Some(at);
        }
        if self.next_y + h <= self.size {
            let shelf = Shelf {
                y: self.next_y,
                height: h,
                x: w,
            };
            self.next_y += h;
            self.shelves.push(shelf);
            return Some((0, shelf.y));
        }
        let any = self
            .shelves
            .iter_mut()
            .filter(|s| fits(s))
            .min_by_key(|s| s.height)?;
        let at = (any.x, any.y);
        any.x += w;
        Some(at)
    }

    fn blit(&mut self, x: u32, y: u32, bitmap: &GlyphBitmap) {
        for row in 0..bitmap.height {
            for col in 0..bitmap.width {
                let a = bitmap.alpha[(row * bitmap.width + col) as usize];
                let i = (((y + row) * self.size + x + col) * 4 + 3) as usize;
                self.pixels[i] = a;
            }
        }
        let rect = PixelRect {
            x,
            y,
            width: bitmap.width,
            height: bitmap.height,
        };
        self.dirty = Some(match self.dirty {
            Some(d) => d.union(&rect),
            None => rect,
        });
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    /// RGBA8 texels, row-major, `size * size * 4` bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Increments whenever the page is cleared (creation or eviction).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Frame in which a glyph on this page was last requested.
    pub fn last_used(&self) -> u64 {
        self.last_used
    }
}

/// Region of a page that changed since the last [`GlyphAtlas::take_dirty`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRegion {
    pub page: u32,
    pub rect: PixelRect,
    pub generation: u64,
}

/// A glyph's location in the atlas and its placement relative to the pen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasGlyph {
    pub page: u32,
    pub rect: PixelRect,
    pub uv: UvRect,
    /// Bitmap left/top relative to the pen, in raster pixels (y-down).
    pub left: f32,
    pub top: f32,
    /// Pixel size the bitmap was rasterised at (the request size, or the SDF base size).
    pub raster_px: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GlyphKey {
    face: FaceId,
    glyph: u32,
    size_q: u32,
    synthesis: Synthesis,
}

#[derive(Clone, Copy, Debug)]
enum Entry {
    Empty,
    Placed(AtlasGlyph),
}

/// Glyph cache over a bounded set of pages.
///
/// Call [`GlyphAtlas::begin_frame`] once per rendered frame. Pages used in the current frame
/// are never evicted, so quads built during one frame stay valid for that frame; if every
/// page is in use and full, requests fail with `text.atlas.full`.
#[derive(Clone, Debug)]
pub struct GlyphAtlas {
    config: AtlasConfig,
    pages: Vec<AtlasPage>,
    entries: HashMap<GlyphKey, Entry>,
    frame: u64,
    evictions: u64,
}

impl GlyphAtlas {
    pub fn new(config: AtlasConfig) -> Result<Self, TextError> {
        config.validate()?;
        Ok(Self {
            config,
            pages: Vec::new(),
            entries: HashMap::new(),
            frame: 0,
            evictions: 0,
        })
    }

    pub fn config(&self) -> &AtlasConfig {
        &self.config
    }

    /// Starts a new frame for LRU accounting.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn pages(&self) -> &[AtlasPage] {
        &self.pages
    }

    /// Pages evicted so far.
    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Cached glyph count (including empty glyphs such as spaces).
    pub fn cached_glyphs(&self) -> usize {
        self.entries.len()
    }

    /// Changed regions since the previous call; renderers re-upload exactly these.
    pub fn take_dirty(&mut self) -> Vec<DirtyRegion> {
        self.pages
            .iter_mut()
            .enumerate()
            .filter_map(|(i, page)| {
                page.dirty.take().map(|rect| DirtyRegion {
                    page: i as u32,
                    rect,
                    generation: page.generation,
                })
            })
            .collect()
    }

    /// UV point inside page 0's opaque white block (creating page 0 if needed).
    pub fn white_uv(&mut self) -> (u32, UvRect) {
        if self.pages.is_empty() {
            self.pages
                .push(AtlasPage::new(self.config.page_size, self.frame));
        }
        self.pages[0].last_used = self.frame;
        let c = (WHITE_BLOCK / 2) as f32 / self.config.page_size as f32;
        (
            0,
            UvRect {
                u0: c,
                v0: c,
                u1: c,
                v1: c,
            },
        )
    }

    /// Returns the atlas entry for a glyph, rasterising and packing it on first use.
    /// `Ok(None)` means the glyph has no outline (for example a space).
    pub fn glyph(
        &mut self,
        library: &FontLibrary,
        face: FaceId,
        glyph: u32,
        size_px: f32,
        synthesis: Synthesis,
    ) -> Result<Option<AtlasGlyph>, TextError> {
        library.limits().check_size(size_px)?;
        let (raster_px, size_q) = match self.config.mode {
            AtlasMode::Coverage => {
                let q = (size_px * 64.0).round() as u32;
                (q as f32 / 64.0, q)
            }
            AtlasMode::Sdf { base_px, .. } => (base_px, 0),
        };
        let key = GlyphKey {
            face,
            glyph,
            size_q,
            synthesis,
        };
        if let Some(entry) = self.entries.get(&key).copied() {
            return Ok(match entry {
                Entry::Empty => None,
                Entry::Placed(placed) => {
                    self.pages[placed.page as usize].last_used = self.frame;
                    Some(placed)
                }
            });
        }
        let face_data = library.face(face).ok_or(TextError::NoFonts)?;
        let font = face_data.font_ref()?;
        let skew = if synthesis.oblique {
            SYNTHETIC_OBLIQUE_SKEW
        } else {
            0.0
        };
        let Some(outline) = raster::glyph_outline(&font, glyph, raster_px, skew)? else {
            self.entries.insert(key, Entry::Empty);
            return Ok(None);
        };
        let max_dim = self.config.page_size - self.config.padding - WHITE_BLOCK - 1;
        let bold = if synthesis.bold {
            raster_px * SYNTHETIC_BOLD_EM
        } else {
            0.0
        };
        let bitmap = match self.config.mode {
            AtlasMode::Coverage => {
                raster::rasterize_coverage(&outline, bold, self.config.padding, max_dim)?
            }
            AtlasMode::Sdf { spread_px, .. } => {
                raster::rasterize_sdf(&outline, spread_px, bold * 0.5, max_dim)?
            }
        };
        let (page, x, y) = self.place(bitmap.width, bitmap.height)?;
        self.pages[page].blit(x, y, &bitmap);
        self.pages[page].last_used = self.frame;
        let s = self.config.page_size as f32;
        let placed = AtlasGlyph {
            page: page as u32,
            rect: PixelRect {
                x,
                y,
                width: bitmap.width,
                height: bitmap.height,
            },
            uv: UvRect {
                u0: x as f32 / s,
                v0: y as f32 / s,
                u1: (x + bitmap.width) as f32 / s,
                v1: (y + bitmap.height) as f32 / s,
            },
            left: bitmap.left as f32,
            top: bitmap.top as f32,
            raster_px,
        };
        self.entries.insert(key, Entry::Placed(placed));
        Ok(Some(placed))
    }

    /// Finds space on an existing page, a new page, or the least recently used page that is
    /// not in use this frame (evicting it).
    fn place(&mut self, width: u32, height: u32) -> Result<(usize, u32, u32), TextError> {
        let (w, h) = (width + self.config.padding, height + self.config.padding);
        for (i, page) in self.pages.iter_mut().enumerate() {
            if let Some((x, y)) = page.allocate(w, h) {
                return Ok((i, x, y));
            }
        }
        let too_large = || TextError::GlyphTooLarge {
            width,
            height,
            page: self.config.page_size,
        };
        if self.pages.len() < self.config.max_pages {
            let mut page = AtlasPage::new(self.config.page_size, self.frame);
            let (x, y) = page.allocate(w, h).ok_or_else(too_large)?;
            self.pages.push(page);
            return Ok((self.pages.len() - 1, x, y));
        }
        let victim = self
            .pages
            .iter()
            .enumerate()
            .filter(|(_, p)| p.last_used < self.frame)
            .min_by_key(|(_, p)| p.last_used)
            .map(|(i, _)| i)
            .ok_or(TextError::AtlasFull {
                pages: self.pages.len(),
            })?;
        self.entries.retain(|_, e| match e {
            Entry::Placed(g) => g.page as usize != victim,
            Entry::Empty => true,
        });
        let page = &mut self.pages[victim];
        page.reset();
        page.last_used = self.frame;
        self.evictions += 1;
        let (x, y) = page.allocate(w, h).ok_or_else(too_large)?;
        Ok((victim, x, y))
    }
}
