# Text engine (`engine-text`)

`crates/engine-text` is a pure-CPU text engine: font loading and matching, fallback chains,
OpenType shaping, bidirectional text, line breaking, rich-text markup, caret and selection
geometry for text input, glyph atlases (coverage and signed distance field) and
renderer-neutral glyph quads. It depends on no engine crate, no GPU API and no windowing
library, and never reads system fonts on its own.

Status: standalone library (UI parity phase A). It is **not yet wired** into `ui_tree`,
`engine-project` content, the desktop renderer or MCP; see [Not yet integrated](#not-yet-integrated).

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

## Limits and errors

| `TextLimits` field | Default | Error code |
| --- | --- | --- |
| `max_font_bytes` | 32 MiB | `text.font.too_large` |
| `max_faces` / `max_collection_faces` | 256 / 64 | `text.font.limit` |
| `max_text_bytes` (text and markup source) | 256 KiB | `text.input.too_long` |
| `max_spans` (also markup tags) | 4096 | `text.input.too_many_spans` |
| `max_glyphs` | 65 536 | `text.input.too_many_glyphs` |
| `min_font_px` / `max_font_px` | 1 / 512 | `text.input.size_out_of_range` |
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

## Not yet integrated

Phase B work (other UI leaves) and current limitations:

- No `UiTextBackend` implementation, no `engine-project` font assets, no MCP tools, no
  desktop renderer pass: `GLYPH_SHADER_WGSL` is source only and the atlas is CPU memory.
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
