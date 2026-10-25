# Text engine (`engine-text`)

`crates/engine-text` is a pure-CPU text engine: font loading and matching, fallback chains,
OpenType shaping, bidirectional text, line breaking, rich-text markup, caret and selection
geometry for text input, glyph atlases (coverage and signed distance field) and
renderer-neutral glyph quads. It depends on no engine crate, no GPU API and no windowing
library, and never reads system fonts on its own.

Status: integrated into the game UI (UI parity phase B). The library itself stays free of
engine, GPU and windowing code; `engine-project` plugs it in as a `UiTextBackend`, loads
TrueType/OpenType faces as `font_face` assets, draws rich labels, text inputs and
world-space text through it, and `engine-desktop` forwards typed text and IME events. See
[Game UI integration](#game-ui-integration). GPU SDF rendering and 3D world text are the
`ui.sdf_text` and `ui.world` render features of [engine-ui-render](ui-render.md) (see
[SDF status](#sdf-status)).

## Pipeline

```text
font bytes ──► FontLibrary ──────────────┐
markup / builder ──► RichText ──► FontLibrary::layout ──► TextLayout ──► build_quads ──► GlyphQuad
                                   (bidi, itemise,          (glyphs, lines,   (GlyphAtlas:   (page, uv,
                                    shape, break, align)     links, carets)    coverage/SDF)  rect, colour)
```

```rust
use engine_text::*;

let mut fonts = FontLibrary::new();
fonts.load_font(std::fs::read("assets/fonts/Inter.ttf")?)?;          // TTF, OTF or TTC
fonts.load_font_as(std::fs::read("assets/fonts/NotoSansJP.otf")?, "Noto JP")?;
fonts.set_fallback(
    FallbackConfig::new()
        .with_default(["Noto Emoji"])
        .with_language("ja", ["Noto JP"])
        .with_script("Arab", ["Noto Naskh Arabic"]),
)?;

let rich = RichText::from_markup("Press [b]Start[/b] or [link=help]read the guide[/link]", &MarkupOptions::default())?;
let layout = fonts.layout(&rich, &TextStyle::new("Inter", 18.0), &LayoutOptions {
    max_width: Some(320.0),
    max_lines: Some(3),
    overflow: Overflow::Ellipsis,
    ..LayoutOptions::default()
})?;

let mut atlas = GlyphAtlas::new(AtlasConfig::default())?;   // or AtlasConfig::sdf()
atlas.begin_frame();
let quads = build_quads(&layout, &fonts, &mut atlas, glam::Vec2::new(16.0, 16.0))?;
for region in atlas.take_dirty() { /* upload region.rect of page region.page */ }
```

## Public API

| Area | Items |
| --- | --- |
| Fonts | `FontLibrary::{new, with_limits, load_font, load_font_as, faces, face_info, face_metrics, face_covers, match_face, set_fallback, fallback}`, `FaceId`, `FaceInfo` (family, weight, style, stretch, index), `FaceMetrics`, `FontStyle`, `weight::*`, `FaceMatch`, `Synthesis` |
| Fallback | `FallbackConfig { default, by_script, by_language }` with `with_default`, `with_script`, `with_language`, `chain` |
| Rich text | `RichText { text, spans }`, `RichText::{plain, builder, from_markup, validate}`, `RichTextBuilder::{text, push_style, pop, image, build}`, `StyleChange`, `Span`, `SpanStyle`, `InlineImage`, `parse_markup`, `MarkupOptions`, `UnknownTagPolicy` |
| Layout | `FontLibrary::layout`, `TextStyle`, `LayoutOptions` (`max_width`, `align`, `line_height`, `max_lines`, `overflow`, `direction`, `language`), `TextAlign`, `LineHeight`, `Overflow`, `BaseDirection` |
| Output | `TextLayout { glyphs, lines, bounds, links, images, decorations, truncated }`, `PositionedGlyph` (face, glyph id, pen x/y, size, colour, span, cluster byte range, synthesis, line), `LineMetrics`, `LinkRegion`, `InlineImageRect`, `Decoration` |
| Caret | `TextLayout::{hit_test, caret_rect, selection_rects, move_caret, line_of, snap_index, text, laid_out_end}`, `CaretMove` |
| Atlas | `GlyphAtlas::{new, begin_frame, glyph, white_uv, pages, take_dirty, evictions, cached_glyphs}`, `AtlasConfig`, `AtlasMode`, `AtlasPage::{pixels, size, generation, last_used}`, `AtlasGlyph`, `DirtyRegion` |
| Quads | `build_quads`, `transform_quads`, `GlyphQuad { page, uv, rect, colour }`, `WorldQuad { page, uv, corners, colour }` |
| Shader | `GLYPH_SHADER_WGSL`, `SHADER_MODE_COVERAGE`, `SHADER_MODE_SDF`, `SHADER_MODE_MSDF` |
| Errors | `TextError` with `code()`, `MarkupErrorKind`, `TextLimits` |

Coordinates are layout pixels with y growing downward; the layout origin is the top-left of
the layout box and glyph positions are pen positions on the baseline. Byte indices are UTF-8
offsets into `RichText::text`.

## Fonts, matching and fallback

- `load_font` accepts TTF, OTF and TrueType/OpenType collections; every face of a collection
  is registered atomically (all or none). Family is the typographic family (name ID 16) or
  the legacy family (ID 1); `load_font_as` overrides it with a logical name.
- `match_face(family, weight, style)` is case-insensitive and follows CSS font matching:
  style first (italic → oblique → normal, or the reverse order for normal requests), then
  weight (400–500 searches up to 500 first, lighter requests search down, heavier search up),
  then stretch closest to 100 %.
- Synthesis: a bold request (weight ≥ 600) served by a face lighter than 600 sets
  `Synthesis::bold` (advances widen by size/24; coverage double-strikes, SDF grows the
  edge); an italic/oblique request served by an upright face sets `Synthesis::oblique`
  (outline skew 0.2126, about 12°). Both flags are reported on every `PositionedGlyph`.
- Per-grapheme face choice: the requested family, then `by_language` lists (layout
  language, then shorter prefixes: `zh-hant-tw` → `zh-hant` → `zh`), then the `by_script`
  list for the grapheme's ISO 15924 script, then `default`. The first face whose character
  map covers every significant character of the grapheme wins (joiners, variation selectors
  and bidi controls are ignored). If none covers it, the requested family's face is used
  and the shaper emits `.notdef`. Common/Inherited characters (digits, punctuation, emoji)
  take the script of the surrounding text.

## Rich-text markup

| Tag | Meaning |
| --- | --- |
| `[b]…[/b]`, `[i]…[/i]`, `[u]…[/u]` | bold, italic (select face variants or synthesise), underline |
| `[color=#rgb]`, `[color=#rrggbb]`, `[color=#rrggbbaa]` … `[/color]` | straight-alpha sRGB colour |
| `[size=N]…[/size]` | size in pixels, within the size limits |
| `[font=Family Name]…[/font]` | family (1–256 bytes) |
| `[link=id]…[/link]` | link id (`A–Z a–z 0–9 _ . : / # -`, 1–256 bytes) reported as `LinkRegion`s |
| `[img=key width=W height=H]` | inline image placeholder (self-closing, `0 < W,H ≤ 4096`) |

Escapes: `\[`, `\]` and `\\`; any other backslash is literal (so `C:\path` needs no escaping).
Tags nest; a closing tag must match the innermost open tag.

Unknown-tag policy (`MarkupOptions::unknown_tags`):

- `Error` (default, for authored content): `text.markup.unknown_tag` with the byte offset.
- `Literal` (for player-typed text): an unknown tag or an unterminated `[` is kept verbatim;
  malformed known tags still fail.

Other errors: `text.markup.unclosed`, `text.markup.mismatched`, `text.markup.bad_value`,
`text.markup.too_deep` (nesting beyond `max_markup_depth`), `text.markup.unterminated` (no
`]` within 512 bytes). The parser is a front end over `RichTextBuilder`, so markup and the
equivalent builder calls produce identical `RichText`. Images are stored as U+FFFC with an
image span.

## Layout semantics

- Bidi: UAX #9 via `unicode-bidi`; paragraph direction from `BaseDirection` (`Auto` uses the
  first strong character). Each line is reordered visually with rules L1 (trailing
  whitespace, tabs and the ellipsis take the paragraph level) and L2.
- Itemisation and shaping: runs split on face, size, synthesis, bidi level and script and
  are shaped by harfrust (kerning, ligatures, mark positioning, Arabic joining, Indic and
  other complex shapers) with surrounding text as context. Clusters are whole graphemes.
- Line breaking: UAX #14 opportunities via `unicode-linebreak`, greedy fill against
  `max_width`; breaking whitespace hangs past the edge; a word wider than the line breaks at
  a cluster boundary (a grapheme, or a whole ligature). `\n`, `\r\n`, U+2028/U+2029, VT,
  FF and NEL are explicit breaks; text ending in a newline has a final empty line for the caret. Tabs advance four spaces.
- Alignment: `Start`/`End` follow the paragraph direction; `Left`, `Right`, `Center` are
  absolute. The alignment box is `max_width`, or the widest line when unwrapped.
- Line height: `Normal` (largest ascent + descent + line gap on the line), `Relative(f)`
  (f × largest font size), `Absolute(px)`; extra space is split evenly above and below.
- `max_lines` drops later lines; with `Overflow::Ellipsis` the last kept line ends with `…`
  (`...` if the face lacks U+2026), removing clusters until it fits `max_width`. The
  ellipsis glyph has an empty cluster range; `TextLayout::truncated` and
  `LineMetrics::ellipsized` report it.
- Inline images reserve `width` × `height` on the baseline (`InlineImageRect`); links and
  underlines produce one region per contiguous visual piece per line.

## Caret, selection and IME

Indices are snapped to grapheme boundaries. A caret index belongs to the line whose range
contains it (downstream affinity: the start of a soft-wrapped line is drawn on the new line)
and is drawn at the leading edge of the cluster that starts there: the left edge of a
left-to-right cluster, the right edge of a right-to-left one. Inside a ligature the advance is
split evenly between its graphemes.

- `hit_test(point)` picks the line by y (clamped) and the nearest caret stop by x.
- `caret_rect(index)` is a zero-width rectangle with the line's top and height; IME
  candidate windows can anchor to it.
- `selection_rects(range)` returns one rectangle per contiguous visual piece per line, so a
  selection across a direction change yields several rectangles on one line.
- `move_caret(index, CaretMove)`: logical moves (`NextGrapheme`, `PrevGrapheme`, `NextWord`,
  `PrevWord` using UAX #29 word boundaries, `LineStart`, `LineEnd`, `TextStart`, `TextEnd`)
  follow byte order; visual moves (`VisualLeft`, `VisualRight`) follow screen order, which is
  reversed inside right-to-left runs (in Hebrew text `VisualRight` moves to a smaller index),
  and continue on the adjacent line in paragraph order. `LineUp`/`LineDown` take an optional
  x so callers can keep column memory.
- IME preedit is rendered by inserting the preedit string into the text with an underline
  span and laying out again; the event model belongs to the UI layer.

## Glyph atlas

Pages are RGBA8 `page_size²` textures with white RGB and coverage or distance in alpha (UI
contract 2), so vertex colour tints them. Each page reserves a 4×4 opaque white block at its
origin; `white_uv()` returns a point inside it for solid fills (`build_quads` uses it for
underlines).

| Mode | Raster | Use |
| --- | --- | --- |
| `Coverage` | anti-aliased coverage at the exact requested size (1/64 px quantised); quads snap pens to whole pixels | crisp screen UI |
| `Sdf { base_px, spread_px }` | true signed distance at `base_px` (default 48), alpha = 0.5 + d / (2·spread) (default spread 6), inside > 0.5, non-zero winding | scalable and world-space text; one bitmap serves every size |

- Packing: shelf packing per page with `padding` empty pixels around every glyph bitmap.
- Eviction: page-granular LRU. Call `begin_frame()` once per frame; when every page is full,
  the least recently used page that was not used in the current frame is cleared (its
  `generation` increments) and reused. If every page was used this frame the request fails
  with `text.atlas.full`, so quads built during a frame never point at evicted texels.
- Upload tracking: `take_dirty()` returns the union rectangle changed per page since the
  previous call (a new or recycled page is reported whole).
- `build_quads` emits one `GlyphQuad` per outlined glyph and underline (spaces have no
  quad). `transform_quads(quads, Mat4)` maps corners to world space; layout space is y-down,
  so world text normally composes `Mat4::from_scale(Vec3::new(s, -s, 1.0))`.

## Shader

`GLYPH_SHADER_WGSL` (validated with naga in `tests/shader.rs`) binds, in group 0: `0` a
uniform `{ view_proj: mat4x4<f32>, params: vec4<f32> }`, `1` the page `texture_2d<f32>`, `2`
a filtering sampler. Vertex inputs match `runtime::Vertex` (`vec3` position, `vec2` uv, `vec4`
colour). `params.x` selects coverage (0), SDF in alpha (1) or the median of RGB for
multi-channel distance pages produced elsewhere (2); `params.y` biases the edge (positive is
bolder, for outlines or glow passes); `params.z` scales the anti-aliasing width. Distance
edges use `smoothstep` over the screen-space derivative (`fwidth`). Output is straight alpha.

## Game UI integration

The integration lives in `engine-project` (`ui_tree::EngineTextBackend`, `content::font_face`,
`world_text`) and `engine-desktop` (`input::text`). Dependency direction is unchanged:
`engine-text` depends on no engine crate.

### Font assets

| Asset kind | File | Purpose |
| --- | --- | --- |
| `font_face` | `.ttf`, `.otf`, `.ttc` or `.otc` (at most 32 MiB) | One font file; a collection registers every face. The asset ID is the default family name. |
| `font_config` | JSON, `schema: 1`, at most 256 KiB, at most one per project | Groups files into families, sets pixel sizes and atlas modes, configures fallback chains. |

Both are ordinary manifest `assets` entries (`{"id":"ui","kind":"font_face","path":"fonts/Inter.ttf"}`),
so projects edited through MCP or by hand use the same path. `Project::load` and
`Project::validate` load every font asset; `asset_list` accepts the kinds and `asset_info`
reports faces (family, weight, italic) and config counts.

```json
{
  "schema": 1,
  "defaultFamily": "ui",
  "defaultSize": 16,
  "faces": { "ui-bold": { "family": "ui" } },
  "families": { "ui": { "size": 18, "atlas": "coverage" } },
  "fallback": {
    "default": ["emoji"],
    "byScript": { "Arab": ["naskh"] },
    "byLanguage": { "ja": ["jp"] }
  }
}
```

- `faces` maps a `font_face` asset ID to the logical family it registers under (default: its
  own ID), so a regular and a bold file become one family and `[b]` picks the real bold face.
- `families` sets, per family, the pixel size at `fontScale` 1 (default `defaultSize`, 16) and
  the atlas request (`coverage` or `sdf`).
- `fallback` is the `FallbackConfig` (`default`, `byScript`, `byLanguage`) by family name. Every
  family it names must exist. `EngineTextBackend::set_fallback` and `set_language` replace the
  chains and the shaping language at run time (the hooks for per-language fonts).
- `defaultFamily` is used by nodes whose `style.font` is empty.

Errors: `asset.font_face.extension`, `asset.font_face.too_large`, `asset.font_face.invalid`
(unparseable; carries the `text.font.*` reason), `asset.font_config.invalid`,
`asset.font_config.unknown_face`, `asset.font_config.unknown_family` and
`asset.font_config.duplicate` (a second `font_config`). Unknown JSON fields are rejected.

### Backend selection

`style.font` is a family key. Keys that resolve to an engine family (case-insensitive) are
laid out by `engine-text`; any other key uses the bitmap font of that name (`font` assets), so
existing projects are unaffected, and a key that is neither is `render.font.unknown`. The
runtime passes `SelectingTextBackend` (`Runtime::ui_text()`); the engine is `Runtime::text_engine()`.

- Pixel size is the family size times `fontScale`. `[size=N]` in markup is absolute pixels.
- Plain text, text inputs and labels are laid out with the node's `foreground` colour; markup
  may override it per span. Wrapping is `max_width`, alignment is start.
- Requests are cached (up to 512 shaped and 512 rasterised layouts) so the several measure and
  layout calls per tick shape a string once. A change of fallback or language clears the cache.
- Markup uses the bracket syntax of this crate with `UnknownTagPolicy::Literal`: `[note]` stays
  text, a malformed known tag such as `[color=zz]` is `text.markup.bad_value`.
  `ui_tree::validate_markup` checks markup without laying it out. The bitmap backend instead
  strips `<...>` tags.
- Limits: 64 KiB of text per request (`ui.text.invalid`), the `TextLimits` of this crate otherwise.

Atlas pages are 1024 x 1024, at most four. The backend publishes each page as an `Arc<Texture>`
snapshot and republishes it only when the atlas reports the page dirty or recycled, so the
renderer (which uploads per `Arc` identity) uploads a page once per change.
`EngineTextBackend::stats()` reports `texture_uploads`, cached glyphs, evictions and cache sizes.
`begin_frame()` runs at the start of every UI tick and every extraction; pages unused in the current frame are recycled when
every page is full, and a frame that needs more glyphs than fit fails with `text.atlas.full`.

### Rich labels, links and inline images

`rich_label` markup supports the [tags above](#rich-text-markup):

- `[link=id]` regions come back as `UiTextLayout::links`. Clicking a link, or `ui_confirm` on
  the focused label (first link), emits `ui.link_activated` with the link ID as `value`
  (payload `{node, action, value}`). A rich label is a pointer target and focusable only when its
  markup contains `[link=`; set `nav.focusable` to `false` to keep it pointer-only. Links get no
  implicit styling; combine them with `[color]` and `[u]`.
- `[img=key width=W height=H]` reserves the size on the baseline. `key` is a runtime `texture`
  asset ID; the image is drawn as its own quad (one draw per texture, ID suffix `:2~NNN`), tinted
  by the label's opacity. A missing texture is `render.texture.unknown`.
- The backend itself is strict: malformed markup in a request is a `text.markup.*` error.
  Authored markup is checked by `UiRoot` validation ("RichLabel markup invalid"), and text
  that arrives at run time (data bindings, translations, virtual-list item texts) is made valid
  before it reaches a `rich_label`: markup the text engine rejects, or that a length limit cut
  inside a tag, is kept as literal text with its brackets escaped. The desktop runner ends the
  process on a tick or extraction error, so run-time data must not be able to raise one.

### Text input

Text inputs on a shaped font take caret, selection, hit test and Up/Down movement from
`TextLayout` (`UiTextGeometry`). The caret and `GameUi::ime_area()` are correct in wrapped and
right-to-left text, selections may be several rectangles, and the IME preedit is drawn as an
underline under its range at the caret. The preedit text is inserted into the laid-out string, so it
wraps with the value. In multiline inputs Up and Down move to the nearest caret stop on the
neighbouring wrapped line (Shift extends the selection); `ime_area()` is always inside the
input's rectangle because the view scrolls to keep the caret visible.

### Desktop forwarding: typed text and IME

`engine-desktop` (`input::text`) translates winit events with the pure functions
`translate_key`, `translate_ime` and `translate_window_event`, and queues the result with
`GameUi::push_text_event` only while a text input is focused:

| winit | `UiTextEvent` |
| --- | --- |
| `KeyEvent::text` of a press (control characters removed, cut to 1024 bytes) | `Insert` |
| Backspace, Delete, arrows, Home, End, Enter, Escape | `Key` (Shift extends; Ctrl, or Option on macOS, selects word motion) |
| Ctrl+A/C/X/V (Cmd on macOS), Shift+Insert, Ctrl+Insert, Shift+Delete | `Key` SelectAll, Copy, Cut, Paste |
| `Ime::Preedit(text, cursor)` | `Preedit` (an invalid cursor range keeps the text and drops the cursor) |
| `Ime::Commit(text)` | `Commit` |
| `Ime::Disabled` | `Preedit` with empty text (clears the composition) |

Ctrl or Cmd chords that are not editing shortcuts type nothing; AltGr (Ctrl+Alt) still types.
After each simulation tick the runner calls `Window::set_ime_allowed` when the focus state
changes and `Window::set_ime_cursor_area` with the caret rectangle mapped through the camera fit
(`ime_cursor_area`). Events the UI rejects (a full queue) are dropped.

Copy, cut and paste use a process-local in-memory clipboard by default. The cargo feature
`system-clipboard` of `engine-desktop` (`arboard`, MIT OR Apache-2.0) installs the operating
system clipboard when the window opens, falling back to the in-memory one if it cannot be opened.

### World-space text (`engine.WorldText`)

An entity with `Transform` and `engine.WorldText` (editable with `component_set`, validated by
`world_text::WorldText::validate`) is laid out at `size x pixelsPerUnit` pixels and emitted as
`DrawSpace::World` meshes, one per atlas page, textured with the coverage atlas.

| Field | Meaning |
| --- | --- |
| `text`, `markup` | up to 4096 bytes; `markup` enables the rich tags (validated when set) |
| `font` | family key; empty uses `defaultFamily` |
| `size` | em height in world units (0.001 to 1000) |
| `pixelsPerUnit` | raster density (1 to 512); `size x pixelsPerUnit` must be 1 to 512 pixels |
| `color` | straight-alpha RGBA, 0 to 1 |
| `align` | `start`, `end`, `left`, `right`, `center` inside the text box |
| `maxWidth` | wrap width in world units; absent never wraps |
| `anchor` | point of the text box at the entity origin, `[0,0]` top-left to `[1,1]` bottom-right |
| `billboard` | ignore the entity rotation so the text stays upright on screen |
| `atlas` | `coverage` or `sdf` (see below) |
| `layer`, `visible` | draw layer and visibility |

Quad corners are `world_matrix * scale(1/ppu, -1/ppu, 1) * translate(-anchor * box)` applied
with `engine_text::transform_quads`. A project with world text needs at least one `font_face`
(`render.font.unknown` otherwise). Without the `ui.world` render feature, or without a
`Camera3D`, the draws are `DrawSpace::World` meshes for the 2D world camera and `billboard` means
"do not rotate". With the feature ready and a `Camera3D` active the text is drawn in the 3D
frame instead (Transparent stage): depth-tested against the scene, sorted back to front,
`billboard` text aligned with each view's camera right and up axes, and `atlas: "sdf"` text
drawn from an SDF atlas; the 2D draws are skipped (see [ui-render.md](ui-render.md#uiworld)
and its [routing rule](ui-render.md#routing-rule-no-double-drawing)).

### SDF status

Requests for SDF text (`font_config` `atlas: "sdf"`, `WorldText.atlas: "sdf"`) are served by
the render features of [engine-ui-render](ui-render.md) when they are registered
(`engine_ui_render::install`) and prepared on a wgpu renderer:

- screen UI text of SDF families is laid out against an `AtlasConfig::sdf()` atlas (48 px base,
  6 px spread, created on first use beside the coverage atlas) and drawn by `ui.sdf_text` in the
  Overlay stage with `GLYPH_SHADER_WGSL`;
- `WorldText` with `atlas: "sdf"` in a `Camera3D` scene is drawn from the same SDF atlas by
  `ui.world`.

`EngineTextBackend::render_status()` reports `TextRenderStatus { requested, state, sdf_families,
sdf_requests, sdf_gpu_layouts, screen, world, fallback_reason }`: `state` is `sdf` when both
paths draw on the GPU, `sdf_fallback` otherwise, and `screen` / `world` carry each path's
`gpu` or `fallback` state with the precise reason (feature not registered, registered but not
prepared by a wgpu renderer, disabled for the camera through `Camera3D.render_features`, or no
active `Camera3D` for world text). In every fallback case the text is **rendered through a
coverage atlas at the pixel size actually needed**: 2D-only frames, non-wgpu backends and
headless sprite backends keep this path. Authored data is unchanged by any of this.

## Limits and errors

| `TextLimits` field | Default | Error code |
| --- | --- | --- |
| `max_font_bytes` | 32 MiB | `text.font.too_large` |
| `max_faces` / `max_collection_faces` | 256 / 64 | `text.font.limit` |
| `max_text_bytes` (text and markup source) | 256 KiB | `text.input.too_long` |
| `max_spans` (also markup tags) | 4096 | `text.input.too_many_spans` |
| `max_glyphs` | 65 536 | `text.input.too_many_glyphs` |
| `min_font_px` / `max_font_px` | 1 / 512 | `text.input.size_out_of_range`. The UI backend clamps the pixel size (family size x `fontScale` x text scale) into this range when measuring and drawing, so scale settings never produce the error; `font_face` / `font_config` sizes outside it are still rejected at load. |
| `max_markup_depth` | 32 | `text.markup.too_deep` |
| `max_fallback_entries` | 256 | `text.font.fallback_limit` |

Other codes: `text.font.invalid` (unparseable font, units per em outside 16–16384, no
glyphs), `text.font.none` (no registered face), `text.input.invalid_span` (out of range,
empty, unsorted, overlapping, off a char boundary, malformed image span),
`text.layout.invalid_option` (non-finite or negative width, line height outside (0, 10000],
`max_lines` 0, language longer than 64 bytes), `text.atlas.config` (page size outside
64–8192, 1–64 pages, padding above 8, SDF base outside 8–256 or spread outside 1–32),
`text.atlas.full`, `text.atlas.glyph_too_large` (bitmap larger than a page),
`text.atlas.glyph_too_complex` (more than 16 384 outline segments). Every bound is checked
before the corresponding work; hostile input returns an error, never a panic (covered by a
deterministic pseudo-random input test).

## Library decision record

Requirements: crates.io only, pure Rust, permissive licences, exact pins, no runtime OS font
discovery, complex-script shaping, prefer versions already in `Cargo.lock`.

| Option | Assessment |
| --- | --- |
| **harfrust 0.12 + skrifa 0.44 (chosen)** | Both already in `Cargo.lock` through `epaint` (no new download for shaping or font parsing). harfrust is the HarfBuzz organisation's maintained Rust port of the HarfBuzz shaper (successor of rustybuzz); skrifa (same read-fonts stack) provides metadata, cmap, metrics and outlines including CFF/CFF2 and variable-font defaults. harfrust is MIT; skrifa is MIT OR Apache-2.0. |
| rustybuzz + ttf-parser 0.25 | ttf-parser is in the lock, but rustybuzz (plus its unicode table crates) would be a new download, and its development has moved to harfrust. |
| cosmic-text / parley | Full stacks with their own layout, font database and (optionally) system font discovery; heavier, and they would own decisions (fallback, caret semantics, atlas) this engine needs to control. |
| fontdue / ab_glyph alone | Rasterisation only: no OpenType shaping, so no ligatures, mark positioning or complex scripts. |

Supporting crates: `ab_glyph_rasterizer` 0.1.10 (in lock; coverage accumulation),
`unicode-segmentation` 1.13.3 (in lock; graphemes, words), `glam` (workspace), and three new
small pure-Rust crates: `unicode-bidi` 0.3.18 (UAX #9), `unicode-linebreak` 0.1.5 (UAX #14)
and `unicode-script` 0.5.7 (script itemisation). SDF generation is implemented in the crate
(exact distance to the flattened outline) rather than adding a dependency. MSDF generation
was not built: multi-channel pages would need coloured RGB, which conflicts with the
white-RGB page contract; the shader keeps a median mode for externally generated pages.
Revisit if world-space text needs sharper corners than SDF provides at large magnification.

## Integration status and limitations

Integrated: `UiTextBackend`, `font_face`/`font_config` assets, rich labels (links, inline images),
text input with IME on shaped fonts, desktop typed-text and IME forwarding, world-space text.
Not integrated or limited:

- There is no MCP tool dedicated to fonts beyond the generic asset and `component_set` tools.
- SDF text has no outline, glow or soft-shadow style, and screen SDF text composites above the
  sprite/UI layer (see [SDF status](#sdf-status) and [ui-render.md](ui-render.md#not-implemented)).
- UI text is rasterised at logical pixel size and scaled by the camera fit (no physical-pixel
  rasterisation), and world text at a fixed `pixelsPerUnit`.
- World text is depth-tested in 3D only with the `ui.world` feature and a `Camera3D`; otherwise it
  draws in the 2D world pass (see [ui-render.md](ui-render.md)). Billboard text in 3D is not
  interpolated between ticks.
- Colour glyphs (COLR, CBDT, sbix, SVG) are not rendered; colour-only emoji fonts produce
  no quads. Monochrome outline emoji (Noto Emoji) work.
- Outlines are unhinted and variable fonts use their default instance (no axis selection).
- No vertical text, justification, hyphenation or tab stops.
- Lines are not re-shaped at break points, so a contextual form at a wrapped line edge keeps
  its in-paragraph shape; kerning does not apply across run boundaries (face, size, script
  or direction changes).
- A ligature spanning a style boundary takes the style of its first character; caret
  positions inside ligatures are split evenly (GDEF ligature carets are not read).
- Soft-wrap end affinity is downstream only: `LineEnd` on a soft-wrapped line without
  trailing whitespace returns the next line's start index.
- SDF distances are measured to every contour, so overlapping contours (common in variable
  fonts) can show faint interior seams under strong bias values.
- Typed keys also reach gameplay actions bound to them while a text input is focused.
