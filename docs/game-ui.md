# Retained game UI

`engine_project::ui_tree` is the game UI toolkit. A `UiRoot` ECS component
owns a serializable `UiTree`; node IDs connect hierarchy, keyboard focus,
pointer interaction, and event payloads. The portable editor uses its own UI.
Existing `UiModel` text panels keep their modal behavior and suppress game UI
interaction through the same `gameplay_frame()` input gate.

The tree is a flat, ordered vector with optional parent IDs. This keeps scene
JSON editable by MCP (`component_set` on `engine.UiRoot`) and native Rust
systems without a second presentation state. Layout, hit testing and rendering
use the same computed rectangles and the same widget chrome geometry. Rendering
reuses native `Draw`, mesh, font-atlas and clipping interfaces, avoiding an
editor toolkit dependency in game executables.

Module map (`crates/engine-project/src/ui_tree/`): `style` (layout and paint
style), `widget` (widget kinds and their state), `tree` (hierarchy and
validation), `layout` (taffy integration), `text` (text backend contract), `shaped_text` (the `engine-text` backend),
`navigation` (tab order and spatial focus), `text_edit` (text-input event
model), `chrome` (shared widget geometry), `interaction` (`GameUi`), `render`
(draw extraction), `schema` (authoring descriptor), `builder` (native
constructors) and `world` (ECS integration). The styling layer adds
`selector`, `stylesheet`, `animation`, `resolve`, `binding`, `world_panel` and
`style_schema` (themes, state selectors, transitions, data binding and
world-space panels: see [ui-styling.md](ui-styling.md)); `UiNode::classes`
are the class names those selectors match. Accessibility layers are expected
to add further sibling modules.

## Authored component

The component name is `engine.UiRoot`, version 1. Its descriptor is included
automatically in the shared component catalog and JSON inspector; native code
can also inspect `ui_tree::descriptor()`. Do not redeclare the reserved
`engine.UiRoot` descriptor in a project's custom component schema file. The
catalog schema language has no `oneOf`, so tagged values (widgets, lengths,
grid tracks) share one field schema per tag. Typed validation then rejects
variant mismatches, cycles, missing parents or navigation targets, and invalid
ranges. It is `UiRoot::validate`, run by the catalog and at every
layout/input/extraction boundary. As with other typed authored components, the
value sits in `components.custom`:

```json
{
  "custom": {
    "engine.UiRoot": {
      "version": 1,
      "data": {
        "tree": {
          "nodes": [
            {
              "id": "menu",
              "style": {
                "width": { "unit": "pixels", "value": 240 },
                "height": { "unit": "auto" },
                "offset": [20, 20],
                "layout": "column",
                "padding": 8,
                "gap": 8
              }
            },
            {
              "id": "play",
              "parent": "menu",
              "widget": { "kind": "button", "text": "Play", "action": "play" },
              "style": { "width": { "unit": "fill" }, "font": "asset:ui-font" },
              "tooltip": { "text": "Start a new game", "delay": 0.5 }
            },
            {
              "id": "name",
              "parent": "menu",
              "widget": { "kind": "text_input", "placeholder": "Name", "maxLength": 16, "action": "name" },
              "style": { "width": { "unit": "fill" }, "font": "asset:ui-font" }
            },
            {
              "id": "volume",
              "parent": "menu",
              "widget": { "kind": "slider", "value": 0.5, "min": 0, "max": 1, "step": 0.05, "action": "volume" },
              "nav": { "down": "play" }
            }
          ]
        }
      }
    }
  },
  "EventBindings": {
    "bindings": [
      { "event": "ui.clicked", "handler": "menu_action" },
      { "event": "ui.value_changed", "handler": "menu_action" },
      { "event": "ui.text_submitted", "handler": "menu_action" }
    ]
  }
}
```

Declare `asset:ui-font` as an existing font asset with its atlas texture.
Widgets that draw text require a valid font. Missing fonts and textures return
explicit `render.font.unknown` / `render.texture.unknown` errors at extraction.
During layout and input a missing font measures as empty instead of failing the
tick. `action` is application data delivered in events, not a Rust function name.

Every field has a serde default, so version-1 JSON written before these
additions loads unchanged. Serialization omits fields equal to their default.

## Layout model

Layout runs on [taffy](https://github.com/DioxusLabs/taffy) (`=0.15.0`, MIT;
flexbox and CSS grid features only). All coordinates are top-left logical
pixels in the camera's logical size; rounding is disabled so results are exact.

Lengths (`width`, `height`, `minWidth`, `minHeight`, `maxWidth`, `maxHeight`,
`basis`) take one of four forms:

- `{"unit":"pixels","value":120}`
- `{"unit":"percent","value":0.5}`: a fraction 0..=1 of the parent's content box
- `{"unit":"fill"}`
- `{"unit":"auto"}`: content size, from measured text or children

`fill` is only valid for `width`/`height`. An `auto` minimum or maximum means none.

`layout` selects how a node arranges its children:

| `layout` | Meaning |
| --- | --- |
| `absolute` (default) | Overlay: each child is placed at the content-box top-left plus its margin; children never affect each other. `alignItems`/`justifyItems` align them inside the box. |
| `row`, `column`, `row_reverse`, `column_reverse` | Flexbox line(s) with `wrap` (`no_wrap`, `wrap`, `wrap_reverse`), `justify`, `alignItems`, `alignContent`, `gap`/`rowGap`/`columnGap`. |
| `grid` | CSS grid with `gridColumns`/`gridRows` tracks, `gridAutoColumns`/`gridAutoRows`, `gridAutoFlow` (`row`, `column`, `row_dense`, `column_dense`). |

Grid tracks are `{"kind":"px","value":100}`, `percent`, `fr`, `auto`,
`min_content`, `max_content` or `{"kind":"minmax","min":{...},"max":{...}}`.
A bare `fr` track is CSS `minmax(auto, Nfr)`, and `fr` is rejected as a
minimum. Items place themselves with `gridColumn`/`gridRow`:
`{"start":2,"span":3}` is CSS `2 / span 3`, and `{"start":1,"end":-1}` spans
every explicit track. Absent placement uses auto-placement. At most 64 tracks
per axis.

Item properties:

- `margin` (`{left,top,right,bottom}`).
- `padding` (uniform), or `paddingEdges` (per side, overrides `padding`).
- `grow`, `shrink`, `basis`, `alignSelf`, `justifySelf`, `aspectRatio`
  (width / height), and min/max sizes.
- `position`: `flow` (default) or `absolute`. An absolute node leaves the flow
  and is placed by `inset` (`{left,top,right,bottom}`, absent sides auto)
  against the parent's padding box.

Compatibility rules that keep version-1 trees pixel-identical:

- `fill` on a row/column main axis is `flex: 1 1 0` with a zero minimum, so fill
  siblings share the remaining space equally after fixed sizes and gaps.
- `fill` on a cross axis, or inside `absolute`/`grid` parents, stretches to the
  available content box (or takes 100 % when the node sets its own alignment).
- `grow` defaults to 0. `shrink` defaults to 0 for pixel/percent main sizes
  (legacy rows overflow and clip instead of squashing) and to 1 for `fill`/`auto`.
- `offset` is a translation applied after layout to the node and its subtree; it
  never moves siblings, in any layout mode.
- Padding is clamped to half of a pixel width/height on each side, as before.
- Root nodes are overlay children of the viewport.

Text-bearing leaves (labels, rich labels, buttons, toggles, text inputs,
dropdowns) with `auto` sizes are measured through the text backend. Intrinsic
width is max-content, min-content is the widest word, and text wraps at the
available width. Images have no intrinsic size; give them a size or an aspect
ratio.

Paint styles are `clip`, `background`, `foreground`, `accent` (four floats in
0..=1), `font` and `fontScale`. Clipping defaults on and is inherited. A node's
`clip` flag clips its descendants, not its own rectangle. Hidden nodes consume
no layout space. Visibility and enabled state are inherited; disabled nodes draw
at half opacity and receive no input.

`UiTree::layout(viewport)` measures text as empty.
`UiTree::layout_with(viewport, &dyn UiTextBackend)` measures through a backend.
Both return `UiBox`es in paint order, with these fields:

- `rect`: border box after scroll and offset.
- `content`: `rect` minus padding and widget chrome.
- `clip`: the ancestor clip.
- `enabled`.
- `item`: the replica index inside a virtual list.
- `overlay`: inside an open modal dialog.
- `scroll` and `scroll_max`.

## Widget reference

Widget values live in the `UiRoot`, so snapshots include them and
`component_set` can change them. Field names are `camelCase`.

| `kind` | Fields | Behavior |
| --- | --- | --- |
| `container` | none | Background and children. |
| `label` | `text` | Wrapped text in the content box. |
| `rich_label` | `markup` | Markup stored as a plain string; the backend interprets it. The shaping backend reads `[b]`, `[color=#rrggbb]`, `[size=N]`, `[link=id]`, `[img=key width=W height=H]` markup (see [Rich labels](text.md#rich-labels-links-and-inline-images)); the bitmap backend strips `<...>` tags and decodes `&lt; &gt; &amp; &quot;`. Authored markup is validated with the root: a malformed known tag (an unclosed `[b]`, `[size=9999]`) is rejected like any other invalid field. Text a data binding supplies is not validated up front; a binding (and a translation) whose markup the text engine would reject, or that the 4096-byte slot cuts mid-tag, is shown literally with its brackets escaped instead of failing the frame. A rich label whose markup has a `[link=` tag is a pointer target and focusable; clicking a link, or `ui_confirm` on the focused label (first link), sends `ui.link_activated` with the link ID. |
| `image` | `texture`, `uv` `[u,v,w,h]`, `tint`, `fit` (`stretch`, `contain`, `cover`) | Textured quad in the content box; `cover` crops the UV rectangle. |
| `nine_slice` | `texture`, `slices` (texture px l,t,r,b), `border` (logical px, default = slices), `center`, `tint` | Nine textured quads over the border box (replaces the background); borders shrink proportionally when they exceed the box. |
| `button` | `text`, `action` | `ui.clicked` on the pressed edge or `ui_confirm`. |
| `toggle` | `text`, `checked`, `action` | `ui.value_changed` with the new bool. |
| `slider` | `value`, `min`, `max`, `step`, `action` | Pointer drag or `ui_left`/`ui_right`; snaps to steps, `ui.value_changed`. |
| `progress` | `value`, `min`, `max` | Fill bar. |
| `text_input` | `text`, `placeholder`, `maxLength` (chars, at most 4096; the value is also bounded in bytes, see the text-edit model), `password`, `multiline`, `caret`, `anchor`, `action` | See the text-edit model. Password inputs render `*` per character. |
| `dropdown` | `options` (at most 64), `selected`, `placeholder`, `maxVisible`, `action` | Click or `ui_confirm` opens a popup above all UI; `ui.selection_changed` with the index. |
| `scroll_view` | `offset` `[x,y]`, `axes` (`horizontal`, `vertical`, `both`), `scrollbars`, `action` | Clamped scrolling by wheel (40 px per notch), content drag and scrollbar thumb drag; `ui.scrolled` with the offset. Scrollbars reserve an 8 px gutter. |
| `list` | `selected`, `action` | Children are the items; click or `ui_up`/`ui_down` selects; `ui.selection_changed`. |
| `virtual_list` | `itemCount` (at most 1 000 000), `itemExtent`, `template`, `scroll`, `selected`, `overscan` (at most 16), `horizontal`, `action` | The template must be the only child; it is replicated for the visible range only (at most 256 replicas). Selection by click/keys scrolls the item into view. |
| `tabs` | `labels` (1 to 32), `selected`, `headerHeight`, `action` | Child *n* in vector order is the panel for `labels[n]`; only the selected panel is laid out. Header click or `ui_left`/`ui_right`; `ui.tab_changed`. |
| `modal_dialog` | `title`, `open`, `buttons` (at most 4), `titleHeight`, `buttonHeight`, `action` | While open it is laid out centred over the viewport above a dimming backdrop, blocks input to every other node in every root, and traps focus. A footer button or `ui_confirm` sends `ui.dialog_result` with the button index; `ui_cancel` sends `-1`. Closing sets `open` to false. |

A grid is a container with `layout: "grid"` (`UiNode::grid` builds one).

Each node can also carry:

- `tooltip`: `{text, delay}`, with a delay of 0..=10 s.
- `nav`: `up`/`down`/`left`/`right` node-ID overrides, `tabIndex`, and
  `focusable: false` for pointer-only widgets.
- `classes`: up to 16 identifiers, reserved for the styling layer; validated but
  not interpreted yet.
- `accessibility`: optional accessible `label`, `description`, `role`, `hidden`
  and `live` overrides (see [Accessibility](accessibility.md#authored-accessibility-field)).

### Localized text

Any text slot may hold a message reference instead of text: `label.text`, `button.text`,
`toggle.text`, `rich_label.markup`, `text_input.placeholder`, `dropdown.options` and
`placeholder`, `tabs.labels`, `modal_dialog.title` and `buttons`, `tooltip.text`, and a node's `accessibility.label` and `description`. A text
that is exactly `@message` (or `@message.attribute`) shows that message of the current locale
(`{message}` while no locale defines it); `@@...` is a literal text starting with `@`; anything
else, including `Hello @name`, is ordinary text. Plurals and other variants are expressed in the
Fluent message and driven by `message` bindings whose `args` read numbers from the data
context (see [UI styling](ui-styling.md)).

```json
{"id": "start", "widget": {"kind": "button", "text": "@menu-start", "action": "start"},
 "tooltip": {"text": "@menu-start.tooltip"}}
```

References resolve on the per-tick effective root, so the stored component keeps the key,
`set_locale` re-resolves everything on the next tick without reloading and widget state
edited by the player is unaffected. Per-locale `asset_overrides` also replace `image` and
`nine_slice` textures and `style.font` keys. Nodes with a text binding are left to the
binding. Syntax, limits, pseudo-localization, per-locale fonts and the MCP string-table tools
are in [Localization](localization.md#project-integration).

Native builders mirror the widgets: `UiNode::label`, `button`, `toggle`,
`slider`, `text_input`, `dropdown`, `scroll_view`, `list`, `virtual_list`,
`tabs`, `dialog`, `grid`, and others. Chain them with `child_of`, `with_size`,
`with_style`, `with_action` and `with_tooltip`, and add nodes with
`UiTree::with(node)`.

## Interaction and navigation

`GameUi` (an ECS resource) holds the transient interaction state:

- focus and pointer capture;
- hover and tooltip timers;
- the open popup and the active dialog's highlighted button;
- the IME preedit and the text-event queue.

`GameUi::tick_with(roots, input, &UiTickContext { viewport, dt, text })`
processes one frame. `GameUi::tick(roots, viewport, input)` is the headless
form (no fonts, `dt` = 0). Roots sort by `(layer, root ID)`; open dialogs
hit-test above all roots and dropdown popups above those. Within a root,
painting is parent-before-children depth-first and the last interactive
rectangle under the pointer wins.

| Action | Effect |
| --- | --- |
| `ui_click` | Press: activate/focus/capture under the pointer. Hold: drag sliders, scroll content, scrollbar thumbs, text selection. |
| `ui_next`, `ui_previous` | Tab order: ascending `nav.tabIndex`, ties in paint order, wrapping. |
| `ui_up`, `ui_down`, `ui_left`, `ui_right` | Directional navigation, unless the focused widget consumes the direction (sliders, tabs and dialogs consume left/right; lists consume up/down until their first/last item; text inputs consume all). With no focus, down/right focus the first and up/left the last control. |
| `ui_confirm` | Activate the focused button/toggle, open a dropdown, choose the highlighted dialog button or popup option. |
| `ui_cancel` | Close the open popup, cancel the active dialog (`-1`), or leave a text input. |

Directional search first uses an explicit `nav` override when it names a
focusable node of the same root. Otherwise the candidates are the focusable
controls whose centre lies beyond the current centre in that direction. Each is
scored as its centre distance along the direction plus twice its edge gap on
the perpendicular axis, and the lowest score wins, so aligned neighbours beat
closer diagonal ones. Disabled, hidden, fully clipped and `focusable: false`
controls are skipped; a focusable control scrolled out of a scroll view is still
a candidate and is scrolled into view when focus reaches it (it has no visible
area, so pointer hits cannot land on it). An open modal restricts both tab and directional
candidates to its subtree.

Per-node flags are recomputed every tick for styling and accessibility layers;
read them with `GameUi::state(root, node)` or `GameUi::states()`:

- `HOVERED`;
- `PRESSED`: captured and under the pointer;
- `FOCUSED`, `DISABLED` and `CHECKED`;
- `OPEN`: an open popup or dialog;
- `SELECTED`: the selected list item.

A tooltip appears once its owner has been hovered for the tooltip's delay
without a press (`GameUi::tooltip()`). It is anchored at the pointer and draws
above all UI. Losing window focus or opening a `UiModel` modal panel calls
`suspend`, which cancels capture, popups, tooltips and IME composition but keeps
focus.

Widget events are delivered as `EngineEvent`s. The owning root entity is both
source and target, and the payload is
`{ "node": ..., "action": ..., "value": ... }`:

| Event | Value |
| --- | --- |
| `ui.clicked` | `null` |
| `ui.value_changed` | toggle bool or slider number |
| `ui.text_changed`, `ui.text_submitted` | the input's text (once per tick) |
| `ui.selection_changed` | index (dropdown, list, virtual list) |
| `ui.tab_changed` | tab index |
| `ui.scrolled` | `[x, y]` for scroll views, a number for virtual lists |
| `ui.dialog_result` | button index, `-1` when cancelled |

Register a callback with `register_event_handler(app, "menu_action", handler)`
and bind it on the root entity using `EventBindings`. UI events dispatch on the
following tick through the ordinary bounded event queue.

## Text backend (UI contract 3)

All text goes through an object-safe trait:

```rust
pub trait UiTextBackend: Send + Sync {
    fn measure(&self, request: &UiTextRequest<'_>) -> Result<UiTextMetrics>;
    fn layout(&self, request: &UiTextRequest<'_>) -> Result<UiTextLayout>;
}
```

The types are:

- `UiTextRequest { text, kind: Plain | Markup, font, scale, color, max_width }`.
  `max_width: None` never wraps.
- `UiTextMetrics`: `width`, `height`, `line_height`, `lines` and `min_width`
  (the widest unbreakable run).
- `UiTextLayout`:
  - glyph quads, each with `rect`, `uv` `[u0,v0,u1,v1]`, `color` and `page`;
  - RGBA8 atlas `pages`, with white RGB and coverage in alpha;
  - ascending `carets`, each with `index` (byte offset into the laid-out plain
    text), `x`, `y` and `height`; the last stop is the end of the text;
  - `links` and `images`: clickable link rectangles and inline-image
    rectangles (rich-text backends only);
  - `geometry`: an optional `UiTextGeometry` (caret rectangle, selection
    rectangles, hit test, wrapped-line movement) that text input prefers over
    the caret stops when present.

`UiTextBackend::begin_frame` (default no-op) is called at the start of every
`tick_world` and `extract_world_with`, so a backend with a glyph cache can age it.

Implementations must be deterministic so that layout, hit testing and rendering
agree, and must return errors instead of panicking. Four implementations ship:

- `BitmapTextBackend::new(fonts, textures)` works over the bitmap `FontAtlas`
  and wraps text the same way as `content::text_mesh`.
- `NullTextBackend` measures everything as empty.
- `EngineTextBackend` shapes TrueType/OpenType text with `engine-text`: fallback
  chains, bidirectional text, rich markup, links, inline images and precise
  caret geometry. It is built from `font_face` assets; see [Game UI
  integration](text.md#game-ui-integration).
- `SelectingTextBackend::new(engine, fonts, textures)` is what the runtime passes.
  **Selection rule:** a `style.font` key that resolves to a font family of the
  engine backend (the asset ID of a `font_face` asset, or a `font_config`
  family; empty keys resolve only with a configured `defaultFamily`) is shaped
  by `engine-text`; every other key uses the bitmap font of that name, and a key
  that is neither fails with `render.font.unknown`. Projects without `font_face`
  assets behave exactly as before. `Runtime::ui_text()` returns the selecting
  backend and `Runtime::text_engine()` the engine backend.

## Text-edit event model (UI contract 4)

Platform layers translate keyboard text, editing keys and IME events into
`UiTextEvent`s and queue them with `GameUi::push_text_event`. The next tick
applies them, in order, to the focused `text_input`, or discards them when no
input is focused.

```rust
pub enum UiTextEvent {
    Insert(String),                                     // typed text
    Key { key: UiEditKey, modifiers: UiEditModifiers }, // shift, word
    Preedit(UiPreedit),                                 // { text, cursor: Option<[usize; 2]> }
    Commit(String),                                     // IME commit
}
```

`UiEditKey` is `Backspace`, `Delete`, `Left`, `Right`, `Up`, `Down`, `Home`,
`End`, `SelectAll`, `Copy`, `Cut`, `Paste`, `Enter` or `Escape`.

- **Positions:** editing works on character indices (`caret` and `anchor`); the
  selection lies between them.
- **Modifiers:** `shift` extends the selection; `word` moves or deletes by word.
- **Line keys:** in multiline inputs, Home/End go to the ends of the line and
  Up/Down move by logical (`\n`) lines with the bitmap backend, and by wrapped
  visual lines (nearest caret stop on the neighbouring line) with the shaping
  backend. In single-line inputs, Up/Down go to the start/end.
- **Enter and Escape:** Enter inserts a newline in multiline inputs and emits
  `ui.text_submitted` otherwise. Escape leaves the input.
- **Inserted text:** control characters are dropped (newlines are kept only in
  multiline inputs) and the result is truncated to `maxLength` characters, to
  the 4096-byte string limit and to what is left of the root's 64 KiB text
  budget, never splitting a character. `maxLength` counts characters, so
  multi-byte text reaches the byte limit first (1365 three-byte characters).
  Typing, paste, IME commit and `data` bindings on `text` truncate this way; an
  assistive-technology `set_text` that would exceed a bound is rejected
  (`UiAccessStatus::Rejected`) instead.
- **IME:** a preedit is drawn underlined at the caret but is not part of the
  value. `Commit` clears the preedit and inserts its text. With the shaping
  backend the caret, selection and underline come from `engine-text` geometry,
  so they are correct in bidirectional and wrapped text.
- **Clipboard:** copy and cut go through the `UiClipboard` trait (install one
  with `GameUi::set_clipboard`; the default `MemoryClipboard` is process-local).
  They are disabled for password inputs.
- **Pointer:** a press places the caret at the nearest caret stop, and dragging
  extends the selection.

For the desktop runner:

- `GameUi::text_input_focus()` says when to enable text entry and IME.
- `GameUi::ime_area()` gives the caret rectangle for candidate windows.
- `GameUi::preedit()` returns the current composition.

`engine-desktop` does this wiring: it translates winit keyboard and IME events
with `translate_window_event`, queues them while an input is focused, calls
`Window::set_ime_allowed` from `text_input_focus()` and `set_ime_cursor_area`
from `ime_area()`. See [Desktop forwarding](text.md#desktop-forwarding-typed-text-and-ime).

The queue holds at most 256 events per tick and 1024 bytes per event; beyond
that `push_text_event` fails with `ui.text_event.overflow` or
`ui.text_event.invalid`.

## Rendering

There are two extraction entry points:

- `UiRoot::extract_draws(id, viewport, focus, fonts, textures)` draws with only
  a focus highlight.
- `extract_draws_with(id, viewport, &UiRenderState, &dyn UiTextBackend, textures)`
  uses the interaction state from `GameUi::render_state(root)`: focus, pressed,
  popup, preedit, tooltip and dialog choice.

Each box emits up to four draws, in paint order:

1. Chrome geometry: background, check, fills, scrollbars, tab and dialog bars,
   selection.
2. Its textured image.
3. Its text, one draw per atlas page, then one draw per inline-image texture.
4. Top geometry: caret, preedit underline, list and dialog highlights.

Base draws use the root's layer. Open dialogs and their backdrop use
`UI_DIALOG_LAYER` (`i32::MAX - 1`), and popups and tooltips use
`UI_POPUP_LAYER` (`i32::MAX`), so they paint above every root layer. Draw IDs
sort in paint order. This is baseline styling; themes, state-dependent looks,
opacity/scale and world-space panels are described in
[ui-styling.md](ui-styling.md).

`Runtime` integrates the UI as follows:

- It registers the typed adapter before authored component hydration.
- It ticks the UI after event dispatch, using `UiModel::gameplay_frame()`, the
  selecting text backend (shaped `font_face` families, then bitmap fonts) and
  the frame's `dt`.
- It includes the UI draws in both 2D and combined 3D/HUD extraction, and the
  world-space draws of `engine.WorldText` components. When the `engine-ui-render`
  features are prepared on the renderer, SDF screen text, world panels and world text
  are routed to them instead of the 2D draw list (`UiGpuQueue`; status in
  `RuntimeSnapshot.ui_render`); see [ui-render.md](ui-render.md).
- Scene changes and shutdown clear `GameUi`.

Game configuration must not call `ui_tree::register` again. `register`,
`tick_world` and `extract_world` remain available for custom hosts.

## Limits

Every bound below returns an error and never panics:

- 64 roots, 1024 nodes per root, hierarchy depth 64, 16384 boxes per layout
  (virtual-list replicas included), 64 nodes per virtual-list template.
- 128-byte IDs, actions, fonts and texture keys; 4096-byte strings; 256-byte
  option/tab/button labels, placeholders and dialog titles; 1024-byte tooltips;
  64 KiB of text per root (widget strings, tooltips, accessibility labels and
  descriptions together).
- 64 dropdown options, 32 tabs, 4 dialog buttons, 64 grid tracks per axis,
  16 classes per node; lengths, offsets, margins and gaps within ±16384; scroll
  offsets within 0..=1e7.
- Cycles, duplicate IDs, orphan parents, unknown navigation targets, template
  mismatches, nonfinite numbers, invalid colors and degenerate ranges return
  `ui.tree.invalid`; layout failures return `ui.layout.*`.

### Serialized size

Authored JSON also obeys the shared catalog's 64 KiB limit per serialized
component, and runtime snapshots enforce it too. Default fields are omitted, so
a minimal node costs about 30 bytes, but a styled text button costs about 145.
In the measured case, 1024 styled buttons serialize to about 145 KB (144,717 bytes). Such a
tree works natively (layout, input, rendering) but cannot be authored or
snapshotted as one component. To stay under the limit:

- Split large UIs into several roots; each root is its own component.
- Use a `virtual_list` for repeated rows.

The stylesheet layer ([ui-styling.md](ui-styling.md)) removes the repeated
per-node styles: the same grid with its styles in a theme is 70,041 bytes, and
a 1017-node screen of nested layout containers with classes is 60,717 bytes
(104,731 with the look written inline).
Raising the catalog limit is a cross-domain decision and was deliberately not
taken here.

### Timing

Measured on this machine in a debug build, for 1024 nodes (31 rows of 32 text
buttons):

- layout takes about 11 ms;
- layout plus draw extraction takes about 16 ms.

Release builds were not measured.

### Known gaps

- The caret does not blink.
- Up/Down in multiline inputs follow `\n` lines with the bitmap backend (the
  shaping backend moves by visual line).
- Text is rasterised at logical pixel size and scaled with the camera fit, like
  bitmap fonts; there is no physical-pixel rasterisation yet.
- Typed keys also reach gameplay actions bound to them while an input is
  focused; the runner does not suppress them.
- Clicks on virtual-list replicas select the item rather than reaching widgets
  inside the template.
- Images have no intrinsic size.
- A modal dialog inside a hidden tab panel still opens.

## Design record

- **Layout engine.** Replacing the hand-written row/column layout with taffy
  gives CSS-equivalent flexbox, grid, absolute positioning and intrinsic text
  sizing. The alternatives were:
  - extending the old layout, which would have meant reimplementing the flex
    and grid line algorithms;
  - other layout crates, which have less complete grid support.

  The legacy modes map onto taffy, so existing scenes keep their rectangles and
  the existing `game_ui` tests pass unchanged.
- **One tree per frame.** Each layout call rebuilds a `TaffyTree`. That costs
  about 11 ms for 1024 nodes in a debug build and avoids invalidation
  bookkeeping. Revisit this with a persistent tree if profiles show layout on
  the hot path.
- **State placement.** Authored values (text, selection, scroll offsets, dialog
  `open`) live in `UiRoot`, so MCP and snapshots see them. Transient state
  (focus, capture, hover timers, popups, preedit) lives in `GameUi`.
- **Shared chrome geometry.** Popup, scrollbar, tab, dialog and text-input
  geometry is computed once, in `chrome.rs`, for both hit testing and drawing.

Run `cargo test -p engine-project --test game_ui --test ui_layout --test
ui_interaction` for coverage of layout, clipping, navigation, widgets, text
editing, rendering, schema and native callbacks.
