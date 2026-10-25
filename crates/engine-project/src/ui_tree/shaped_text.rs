//! [`EngineTextBackend`]: the shaping [`UiTextBackend`] over `engine-text`
//! (TrueType/OpenType faces, fallback chains, bidirectional text, rich-text
//! markup, caret geometry and glyph atlases).
//!
//! Font selection: `UiStyle::font` names a logical family, which is the asset
//! ID of a `font_face` asset unless a `font_config` asset groups several files
//! under one family. A key that resolves here is drawn by this backend; any
//! other key falls through to the bitmap fonts (see [`SelectingTextBackend`]).
//!
//! Atlas pages are CPU `Arc<Texture>` snapshots. A page's snapshot is
//! re-published only when the atlas reports it dirty, so the renderer (which
//! keys uploads on `Arc` identity) uploads a page once per change, not per frame.
use super::*;
use crate::content::{
    DEFAULT_FONT_SIZE, FONT_SIZE_RANGE, FontAtlasMode, FontConfig, FontFace, FontFamilySettings,
    Texture,
};
use super::gpu::{UiFeatureState, UiGpuFeature, UiGpuQueue};
use engine_text::{
    AtlasConfig, CaretMove, Color, FallbackConfig, FontLibrary, FontStyle, GlyphAtlas, GlyphQuad,
    LayoutOptions, MarkupOptions, RichText, TextAlign, TextError, TextLayout, TextStyle,
    UnknownTagPolicy, build_quads,
};
use glam::Vec2;
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

/// Cached shaped or laid-out requests; the caches are cleared when full.
const MAX_CACHED_REQUESTS: usize = 512;
/// Longest text one request may carry (matches the bitmap backend).
const MAX_REQUEST_BYTES: usize = 65_536;

/// Which glyph pipeline draws text right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextRenderState {
    /// Every family uses (and gets) a coverage atlas.
    Coverage,
    /// SDF was requested and every SDF path draws it on the GPU from an SDF atlas.
    Sdf,
    /// SDF was requested and at least one path (see [`TextRenderStatus::screen`]
    /// and [`TextRenderStatus::world`]) is served by a coverage atlas.
    SdfFallback,
}

/// One SDF drawing path: screen UI text (`ui.sdf_text`) or world text
/// (`ui.world`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SdfPathStatus {
    pub state: UiFeatureState,
    /// Why the path uses a coverage atlas, when `state` is `fallback`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Reported text rendering mode, in the style of `UpscalerStatus`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TextRenderStatus {
    /// `Sdf` when any family requests it.
    pub requested: FontAtlasMode,
    pub state: TextRenderState,
    /// Families that requested SDF.
    pub sdf_families: Vec<String>,
    /// World-text draws that asked for SDF since the backend was created.
    pub sdf_requests: u64,
    /// SDF layouts handed to the GPU features since the backend was created.
    pub sdf_gpu_layouts: u64,
    /// Screen UI text of SDF families (`ui.sdf_text`, Overlay stage).
    pub screen: SdfPathStatus,
    /// `engine.WorldText` with `atlas: "sdf"` (`ui.world`, Transparent stage).
    pub world: SdfPathStatus,
    /// Why SDF requests are served by a coverage atlas (the reasons of the
    /// paths in fallback), when `state` is `sdf_fallback`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

/// Cache and upload counters, for tests and telemetry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UiTextStats {
    /// Atlas page snapshots published (each one is a renderer upload).
    pub texture_uploads: u64,
    pub atlas_pages: usize,
    pub cached_glyphs: usize,
    pub evictions: u64,
    pub cached_layouts: usize,
    pub families: usize,
    pub faces: usize,
}

#[derive(Clone, Debug)]
struct Family {
    name: String,
    size: f32,
    atlas: FontAtlasMode,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    /// Atlas kind of laid-out quads; shaping ignores it (always `false`).
    sdf: bool,
    text: String,
    markup: bool,
    family: String,
    px: u32,
    color: [u32; 4],
    width: Option<u32>,
    align: u8,
}

/// Shaped text: everything that does not depend on the atlas.
struct Shaped {
    layout: Arc<TextLayout>,
    metrics: UiTextMetrics,
    carets: Vec<UiCaretStop>,
    links: Vec<UiLinkRegion>,
    images: Vec<UiInlineImage>,
    geometry: Arc<LayoutGeometry>,
    base: Color,
    base_color: [f32; 4],
}

/// Glyph quads of one request and the atlas pages (with generations) they use.
struct Laid {
    quads: Vec<GlyphQuad>,
    pages: Vec<(u32, u64)>,
}

/// Shaped text placed in layout pixels (y down), for callers that transform it
/// themselves (world-space text).
pub(crate) struct PlacedText {
    pub quads: Vec<GlyphQuad>,
    /// Current atlas page snapshots; `GlyphQuad::page` indexes them.
    pub pages: Vec<Arc<Texture>>,
    /// `pages` are SDF pages (distance in alpha).
    pub sdf: bool,
    /// Union of the visible line boxes.
    pub bounds: engine_text::Rect,
}

struct Snapshot {
    generation: u64,
    texture: Arc<Texture>,
}

/// One glyph atlas and the published snapshots of its pages.
struct Pages {
    atlas: GlyphAtlas,
    snapshots: Vec<Snapshot>,
}

impl Pages {
    fn new(config: AtlasConfig) -> Result<Self> {
        Ok(Self {
            atlas: GlyphAtlas::new(config).map_err(text_error)?,
            snapshots: Vec::new(),
        })
    }

    /// Re-publish the snapshot of every page that changed since the last call;
    /// returns how many were published.
    fn sync(&mut self) -> u64 {
        let dirty: Vec<u32> = self.atlas.take_dirty().iter().map(|d| d.page).collect();
        let mut uploads = 0;
        for (index, page) in self.atlas.pages().iter().enumerate() {
            let stale = self.snapshots.get(index).is_none_or(|s| {
                s.generation != page.generation() || dirty.contains(&(index as u32))
            });
            if !stale {
                continue;
            }
            let snapshot = Snapshot {
                generation: page.generation(),
                texture: Arc::new(Texture {
                    width: page.size(),
                    height: page.size(),
                    pixels: page.pixels().to_vec(),
                }),
            };
            uploads += 1;
            match self.snapshots.get_mut(index) {
                Some(slot) => *slot = snapshot,
                None => self.snapshots.push(snapshot),
            }
        }
        uploads
    }

    fn textures(&self) -> Vec<Arc<Texture>> {
        self.snapshots.iter().map(|s| s.texture.clone()).collect()
    }

    /// Whether every page a laid-out request used is unchanged.
    fn is_current(&self, laid: &Laid) -> bool {
        laid.pages.iter().all(|(page, generation)| {
            self.atlas
                .pages()
                .get(*page as usize)
                .is_some_and(|p| p.generation() == *generation)
        })
    }
}

struct Inner {
    library: FontLibrary,
    /// Exact-size coverage pages (crisp screen text).
    coverage: Pages,
    /// Signed-distance pages, created when SDF text is first laid out.
    sdf: Option<Pages>,
    language: Option<String>,
    shaped: HashMap<Key, Arc<Shaped>>,
    laid: HashMap<Key, Arc<Laid>>,
    uploads: u64,
}

impl Inner {
    fn clear_caches(&mut self) {
        self.shaped.clear();
        self.laid.clear();
    }

    fn pages(&self, sdf: bool) -> &Pages {
        match (&self.sdf, sdf) {
            (Some(pages), true) => pages,
            _ => &self.coverage,
        }
    }
}

/// Shaping text backend over `engine-text`.
pub struct EngineTextBackend {
    families: BTreeMap<String, Family>,
    default_family: Option<String>,
    sdf_requests: AtomicU64,
    sdf_gpu_layouts: AtomicU64,
    /// Screen layouts of SDF families return SDF pages (set by the draw
    /// extraction that hands them to `ui.sdf_text`).
    sdf_output: AtomicBool,
    /// The UI GPU queue, once attached by the runtime (status only).
    gpu: Mutex<Option<UiGpuQueue>>,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for EngineTextBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineTextBackend")
            .field("families", &self.families.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

fn text_error(error: TextError) -> Error {
    Error::new(error.code(), error.to_string())
}

fn normalize(name: &str) -> String {
    name.trim().to_lowercase()
}

fn byte(channel: f32) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn rect(r: engine_text::Rect) -> Rect {
    Rect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

impl EngineTextBackend {
    /// An empty backend; add fonts with [`Self::register_font`].
    pub fn new(atlas: AtlasConfig) -> Result<Self> {
        Ok(Self {
            families: BTreeMap::new(),
            default_family: None,
            sdf_requests: AtomicU64::new(0),
            sdf_gpu_layouts: AtomicU64::new(0),
            sdf_output: AtomicBool::new(false),
            gpu: Mutex::new(None),
            inner: Mutex::new(Inner {
                library: FontLibrary::new(),
                coverage: Pages::new(atlas)?,
                sdf: None,
                language: None,
                shaped: HashMap::new(),
                laid: HashMap::new(),
                uploads: 0,
            }),
        })
    }

    /// Build from loaded `font_face` assets (asset ID to face) and the optional
    /// `font_config`.
    pub fn from_content(
        faces: &BTreeMap<String, Arc<FontFace>>,
        config: &FontConfig,
    ) -> Result<Self> {
        let mut backend = Self::new(AtlasConfig::default())?;
        for (asset, face) in faces {
            let family = config.family_of(asset);
            let settings = config.family_settings(family);
            backend.register_font(
                face.bytes.clone(),
                family,
                FontFamilySettings {
                    size: Some(settings.size.unwrap_or(config.default_size)),
                    atlas: settings.atlas,
                },
            )?;
        }
        backend.set_fallback(config.fallback_config())?;
        if let Some(family) = &config.default_family {
            backend.set_default_family(Some(family));
        }
        Ok(backend)
    }

    /// Register a TTF/OTF/TTC file under the logical `family`. Several files may
    /// share a family (regular, bold, italic); the first registration fixes the
    /// family's settings.
    pub fn register_font(
        &mut self,
        bytes: impl Into<Arc<[u8]>>,
        family: &str,
        settings: FontFamilySettings,
    ) -> Result<()> {
        let size = settings.size.unwrap_or(DEFAULT_FONT_SIZE);
        if !(size.is_finite() && FONT_SIZE_RANGE.contains(&size)) {
            return Err(Error::new(
                "text.input.size_out_of_range",
                "Font family size must be within 1..=512 pixels",
            ));
        }
        self.lock()
            .library
            .load_font_as(bytes, family)
            .map_err(text_error)?;
        self.families
            .entry(normalize(family))
            .or_insert_with(|| Family {
                name: family.trim().to_owned(),
                size,
                atlas: settings.atlas,
            });
        Ok(())
    }

    /// Family used by nodes with an empty `style.font`.
    pub fn set_default_family(&mut self, family: Option<&str>) {
        self.default_family = family.map(normalize);
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replace the fallback chains (the localization hook: per-language and
    /// per-script lists). Cached layouts are discarded.
    pub fn set_fallback(&self, fallback: FallbackConfig) -> Result<()> {
        let mut inner = self.lock();
        inner.library.set_fallback(fallback).map_err(text_error)?;
        inner.clear_caches();
        Ok(())
    }

    pub fn fallback(&self) -> FallbackConfig {
        self.lock().library.fallback().clone()
    }

    /// BCP 47 language of the text: selects shaping features and per-language
    /// fallback fonts. Cached layouts are discarded.
    pub fn set_language(&self, language: Option<&str>) {
        let mut inner = self.lock();
        inner.language = language.map(str::to_owned);
        inner.clear_caches();
    }

    /// Whether `font` (a `style.font` key) is drawn by this backend.
    pub fn resolves(&self, font: &str) -> bool {
        self.family(font).is_some()
    }

    /// Logical family names in registration-normalised form.
    pub fn families(&self) -> Vec<String> {
        self.families.values().map(|f| f.name.clone()).collect()
    }

    fn family(&self, font: &str) -> Option<&Family> {
        let key = if font.is_empty() {
            self.default_family.as_deref()?.to_owned()
        } else {
            normalize(font)
        };
        self.families.get(&key)
    }

    /// Cache and upload counters.
    pub fn stats(&self) -> UiTextStats {
        let inner = self.lock();
        let sdf = inner.sdf.as_ref();
        UiTextStats {
            texture_uploads: inner.uploads,
            atlas_pages: inner.coverage.atlas.pages().len()
                + sdf.map_or(0, |p| p.atlas.pages().len()),
            cached_glyphs: inner.coverage.atlas.cached_glyphs()
                + sdf.map_or(0, |p| p.atlas.cached_glyphs()),
            evictions: inner.coverage.atlas.evictions() + sdf.map_or(0, |p| p.atlas.evictions()),
            cached_layouts: inner.shaped.len(),
            families: self.families.len(),
            faces: inner.library.face_count(),
        }
    }

    /// Connect the status to the runtime's UI GPU queue, which knows whether
    /// the `ui.sdf_text` and `ui.world` render features are registered and
    /// prepared on a renderer.
    pub fn attach_gpu(&self, queue: UiGpuQueue) {
        *self.gpu.lock().unwrap_or_else(PoisonError::into_inner) = Some(queue);
    }

    /// Whether SDF is requested and what actually draws it: `Sdf` when every
    /// path draws from an SDF atlas on the GPU, otherwise `SdfFallback` with
    /// the precise reason per path (feature not registered, not prepared by a
    /// wgpu renderer, disabled for the camera, or no 3D camera for world text).
    pub fn render_status(&self) -> TextRenderStatus {
        let sdf: Vec<String> = self
            .families
            .values()
            .filter(|f| f.atlas == FontAtlasMode::Sdf)
            .map(|f| f.name.clone())
            .collect();
        let sdf_requests = self.sdf_requests.load(Ordering::Relaxed);
        let requested = !sdf.is_empty() || sdf_requests > 0;
        let queue = self
            .gpu
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_default();
        let path = |feature| {
            let (state, reason) = queue.state_of(feature);
            SdfPathStatus { state, reason }
        };
        let (screen, world) = (path(UiGpuFeature::SdfText), path(UiGpuFeature::World));
        let active = screen.state == UiFeatureState::Gpu && world.state == UiFeatureState::Gpu;
        let reasons: Vec<&str> = [&screen, &world]
            .into_iter()
            .filter_map(|p| p.reason.as_deref())
            .collect();
        TextRenderStatus {
            requested: if requested {
                FontAtlasMode::Sdf
            } else {
                FontAtlasMode::Coverage
            },
            state: match (requested, active) {
                (false, _) => TextRenderState::Coverage,
                (true, true) => TextRenderState::Sdf,
                (true, false) => TextRenderState::SdfFallback,
            },
            sdf_families: sdf,
            sdf_requests,
            sdf_gpu_layouts: self.sdf_gpu_layouts.load(Ordering::Relaxed),
            fallback_reason: (requested && !active).then(|| reasons.join("; ")),
            screen,
            world,
        }
    }

    /// Pixel size a request is drawn at.
    pub fn pixel_size(&self, font: &str, scale: f32) -> Option<f32> {
        self.family(font).map(|f| f.size * scale)
    }

    /// Record that a draw asked for SDF (reported by [`Self::render_status`]).
    pub(crate) fn note_sdf_request(&self) {
        self.sdf_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Whether `font` was configured for the SDF seam.
    pub fn wants_sdf(&self, font: &str) -> bool {
        self.family(font)
            .is_some_and(|f| f.atlas == FontAtlasMode::Sdf)
    }

    fn shape(
        &self,
        inner: &mut Inner,
        request: &UiTextRequest<'_>,
        align: TextAlign,
    ) -> Result<(Key, Arc<Shaped>)> {
        if request.text.len() > MAX_REQUEST_BYTES
            || !request.scale.is_finite()
            || request.scale <= 0.0
            || request.max_width.is_some_and(|w| !w.is_finite())
        {
            return Err(Error::new(
                "ui.text.invalid",
                "Invalid text geometry parameters",
            ));
        }
        let family = self
            .family(request.font)
            .ok_or_else(|| Error::new("render.font.unknown", request.font))?;
        let px = family.size * request.scale;
        let markup = request.kind == UiTextKind::Markup;
        let key = Key {
            sdf: false,
            text: request.text.to_owned(),
            markup,
            family: normalize(&family.name),
            px: px.to_bits(),
            color: request.color.map(f32::to_bits),
            width: request.max_width.map(f32::to_bits),
            align: align as u8,
        };
        if let Some(hit) = inner.shaped.get(&key) {
            return Ok((key, hit.clone()));
        }
        let rich = parse(request.text, markup)?;
        let base = Color::rgba(
            byte(request.color[0]),
            byte(request.color[1]),
            byte(request.color[2]),
            byte(request.color[3]),
        );
        let style = TextStyle {
            color: base,
            ..TextStyle::new(family.name.clone(), px)
        };
        let options = LayoutOptions {
            max_width: request.max_width.map(|w| w.max(0.0)),
            align,
            language: inner.language.clone(),
            ..LayoutOptions::default()
        };
        let layout = inner
            .library
            .layout(&rich, &style, &options)
            .map_err(text_error)?;
        let line_height = layout
            .lines
            .first()
            .map(|l| l.height)
            .filter(|h| *h > 0.0)
            .unwrap_or_else(|| {
                inner
                    .library
                    .match_face(&family.name, 400, FontStyle::Normal)
                    .and_then(|m| inner.library.face_metrics(m.face))
                    .map(|m| m.scaled(px))
                    .map_or(px, |m| m.ascent + m.descent + m.line_gap)
            });
        let empty = layout.text().is_empty();
        let metrics = UiTextMetrics {
            width: layout.lines.iter().map(|l| l.width).fold(0.0, f32::max),
            height: if empty {
                0.0
            } else {
                layout.lines.iter().map(|l| l.height).sum()
            },
            line_height,
            lines: if empty { 0 } else { layout.lines.len() as u32 },
            min_width: min_content(&layout),
        };
        let carets = if markup {
            vec![]
        } else {
            caret_stops(&layout, line_height)
        };
        let links = layout
            .links
            .iter()
            .map(|l| UiLinkRegion {
                id: l.id.clone(),
                rect: rect(l.rect),
            })
            .collect();
        let images = layout
            .images
            .iter()
            .map(|i| UiInlineImage {
                key: i.key.clone(),
                rect: rect(i.rect),
            })
            .collect();
        let layout = Arc::new(layout);
        let shaped = Arc::new(Shaped {
            geometry: Arc::new(LayoutGeometry(layout.clone())),
            layout,
            metrics,
            carets,
            links,
            images,
            base,
            base_color: request.color,
        });
        if inner.shaped.len() >= MAX_CACHED_REQUESTS {
            inner.shaped.clear();
        }
        inner.shaped.insert(key.clone(), shaped.clone());
        Ok((key, shaped))
    }

    /// Must be called once per rendered frame before drawing: ages the glyph
    /// atlas so unused pages can be recycled.
    pub fn begin_frame(&self) {
        let mut inner = self.lock();
        inner.coverage.atlas.begin_frame();
        if let Some(sdf) = &mut inner.sdf {
            sdf.atlas.begin_frame();
        }
    }
}

fn parse(text: &str, markup: bool) -> Result<RichText> {
    if !markup {
        return Ok(RichText::plain(text));
    }
    RichText::from_markup(
        text,
        &MarkupOptions {
            unknown_tags: UnknownTagPolicy::Literal,
            ..MarkupOptions::default()
        },
    )
    .map_err(text_error)
}

/// Validate rich-text markup without laying it out (authoring tools).
pub fn validate_markup(markup: &str) -> Result<()> {
    parse(markup, true).map(|_| ())
}

/// Caret stops at every grapheme boundary of plain text.
fn caret_stops(layout: &TextLayout, line_height: f32) -> Vec<UiCaretStop> {
    let end = layout.laid_out_end();
    let mut stops = Vec::new();
    let mut index = 0;
    loop {
        let r = layout.caret_rect(index);
        stops.push(UiCaretStop {
            index: index as u32,
            x: r.x,
            y: r.y,
            height: if r.height > 0.0 {
                r.height
            } else {
                line_height
            },
        });
        let next = layout.move_caret(index, CaretMove::NextGrapheme);
        if next <= index || next > end {
            break;
        }
        index = next;
    }
    stops
}

/// Widest unbreakable run: segments between UAX #14 opportunities, measured
/// along the line they sit on (a segment that wrapped is breakable, so skipped).
fn min_content(layout: &TextLayout) -> f32 {
    let end = layout.laid_out_end();
    let text = &layout.text()[..end];
    let mut widest = 0.0_f32;
    let mut start = 0;
    for (index, _) in unicode_linebreak::linebreaks(text) {
        let segment = text[start..index].trim_end();
        if !segment.is_empty() {
            let last = start + segment.len();
            if layout.line_of(start) == layout.line_of(last) {
                let (a, b) = (layout.caret_rect(start).x, layout.caret_rect(last).x);
                widest = widest.max((b - a).abs());
            }
        }
        start = index;
    }
    widest
}

#[derive(Debug)]
struct LayoutGeometry(Arc<TextLayout>);

impl UiTextGeometry for LayoutGeometry {
    fn caret_rect(&self, index: usize) -> Rect {
        rect(self.0.caret_rect(index))
    }
    fn selection_rects(&self, start: usize, end: usize) -> Vec<Rect> {
        self.0
            .selection_rects(start..end)
            .into_iter()
            .map(rect)
            .collect()
    }
    fn hit_test(&self, point: [f32; 2]) -> usize {
        self.0.hit_test(Vec2::new(point[0], point[1]))
    }
    fn move_vertical(&self, index: usize, down: bool) -> usize {
        let movement = if down {
            CaretMove::LineDown { x: None }
        } else {
            CaretMove::LineUp { x: None }
        };
        self.0.move_caret(index, movement)
    }
}

impl EngineTextBackend {
    /// Shape (cached) and rasterise (cached) a request into the coverage
    /// atlas, or into the SDF atlas when `sdf`: the shaped layout, its atlas
    /// quads and the current page snapshots.
    fn place(
        &self,
        inner: &mut Inner,
        request: &UiTextRequest<'_>,
        align: TextAlign,
        sdf: bool,
    ) -> Result<(Arc<Shaped>, Arc<Laid>)> {
        let (shaped_key, shaped) = self.shape(inner, request, align)?;
        let key = Key {
            sdf,
            ..shaped_key
        };
        if let Some(laid) = inner
            .laid
            .get(&key)
            .filter(|l| inner.pages(sdf).is_current(l))
            .cloned()
        {
            return Ok((shaped, laid));
        }
        let built;
        {
            // `build_quads` needs the library and the atlas at once.
            let Inner {
                library,
                coverage,
                sdf: sdf_pages,
                uploads,
                ..
            } = &mut *inner;
            let pages = if sdf {
                if sdf_pages.is_none() {
                    *sdf_pages = Some(Pages::new(AtlasConfig::sdf())?);
                }
                sdf_pages.as_mut().expect("created above")
            } else {
                coverage
            };
            built = build_quads(&shaped.layout, library, &mut pages.atlas, Vec2::ZERO);
            *uploads += pages.sync();
        }
        let quads = built.map_err(text_error)?;
        let mut pages: Vec<(u32, u64)> = vec![];
        for quad in &quads {
            if !pages.iter().any(|(p, _)| *p == quad.page)
                && let Some(page) = inner.pages(sdf).atlas.pages().get(quad.page as usize)
            {
                pages.push((quad.page, page.generation()));
            }
        }
        let laid = Arc::new(Laid { quads, pages });
        if inner.laid.len() >= MAX_CACHED_REQUESTS {
            inner.laid.clear();
        }
        inner.laid.insert(key, laid.clone());
        Ok((shaped, laid))
    }

    /// Lay out `request` with `align` and return quads in layout pixels, for
    /// callers that place the text themselves (world-space text). The font
    /// size is `request.scale` times the family's pixel size. With `sdf` the
    /// quads and pages come from the SDF atlas.
    pub(crate) fn place_text(
        &self,
        request: &UiTextRequest<'_>,
        align: TextAlign,
        sdf: bool,
    ) -> Result<PlacedText> {
        let mut inner = self.lock();
        let (shaped, laid) = self.place(&mut inner, request, align, sdf)?;
        if sdf {
            self.sdf_gpu_layouts.fetch_add(1, Ordering::Relaxed);
        }
        Ok(PlacedText {
            quads: laid.quads.clone(),
            pages: inner.pages(sdf).textures(),
            sdf,
            bounds: shaped.layout.bounds,
        })
    }
}

impl UiTextBackend for EngineTextBackend {
    fn measure(&self, request: &UiTextRequest<'_>) -> Result<UiTextMetrics> {
        let mut inner = self.lock();
        Ok(self.shape(&mut inner, request, TextAlign::Start)?.1.metrics)
    }

    fn layout(&self, request: &UiTextRequest<'_>) -> Result<UiTextLayout> {
        let mut inner = self.lock();
        let sdf = self.sdf_output.load(Ordering::Relaxed)
            && self
                .family(request.font)
                .is_some_and(|f| f.atlas == FontAtlasMode::Sdf);
        let (shaped, laid) = self.place(&mut inner, request, TextAlign::Start, sdf)?;
        if sdf {
            self.sdf_gpu_layouts.fetch_add(1, Ordering::Relaxed);
        }
        let glyphs = laid
            .quads
            .iter()
            .map(|q| UiGlyphQuad {
                rect: rect(q.rect),
                uv: [q.uv.u0, q.uv.v0, q.uv.u1, q.uv.v1],
                color: if q.color == shaped.base {
                    shaped.base_color
                } else {
                    q.color.to_f32()
                },
                page: q.page,
            })
            .collect();
        Ok(UiTextLayout {
            metrics: shaped.metrics,
            glyphs,
            pages: inner.pages(sdf).textures(),
            sdf,
            carets: shaped.carets.clone(),
            links: shaped.links.clone(),
            images: shaped.images.clone(),
            geometry: Some(shaped.geometry.clone()),
        })
    }

    fn begin_frame(&self) {
        EngineTextBackend::begin_frame(self);
    }

    fn set_sdf_output(&self, enabled: bool) {
        self.sdf_output.store(enabled, Ordering::Relaxed);
    }
}
