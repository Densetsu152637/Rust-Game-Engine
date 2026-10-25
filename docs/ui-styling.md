# UI styling, motion, data binding and world panels

This page covers the styling layer of the retained game UI
([game-ui.md](game-ui.md)): themes with selector rules and tokens,
transitions and keyframe animation (with reduced motion), data binding, and
world-space panels. Everything is authored data (validated by the component
schema, editable through MCP `component_set`) and has a native Rust API.

Source: `crates/engine-project/src/ui_tree/{selector,stylesheet,animation,resolve,binding,world_panel,style_schema}.rs`.
Tests: `crates/engine-project/tests/ui_style.rs`.

## Components and fields

| Name | Where | Purpose |
| --- | --- | --- |
| `engine.UiTheme` | entity component, or native `UiStyleSettings::register_theme` | tokens, selector rules, keyframes |
| `engine.UiData` | entity component | authored initial/driven values merged into the `UiDataContext` |
| `engine.UiWorldPanel` | entity component next to `engine.UiRoot` and `Transform` | draws that root on a quad in the world |
| `UiRoot.theme` | field | theme name for this root (else the active theme) |
| `UiRoot.reducedMotion` | field | per-root reduced-motion override |
| `UiRoot.bindings` | field | data bindings of this root |
| `UiNode.classes` | field (already present) | classes for `.class` selectors |
| `UiStyle.opacity`, `UiStyle.scale` | fields | paint opacity and paint scale |

All new `UiRoot` and `UiStyle` fields have serde defaults and are omitted
while default, so existing version-1 scenes load and serialize unchanged.
Descriptors: `ui_tree::descriptors()` (all four components);
`ui_tree::validate_component(name, &data)` is the typed check the catalog runs.

## Themes

```json
{
  "version": 1,
  "name": "dark",
  "extends": "base",
  "tokens": {
    "colors":  { "surface": [0.1, 0.12, 0.16, 1], "accent": [0.3, 0.6, 1, 1] },
    "numbers": { "gap": 8, "button-h": 32 },
    "fonts":   { "body": "asset:ui-font" }
  },
  "rules": [
    { "selector": "button",
      "style": { "background": "$surface", "font": "$body", "height": "$button-h", "padding": "$gap" },
      "transition": [{ "property": "background", "duration": 0.15, "easing": "ease_out" }] },
    { "selector": "button:hover",   "style": { "background": [0.2, 0.25, 0.35, 1] } },
    { "selector": ".primary:pressed, #quit:pressed", "style": { "scale": 0.97 } }
  ],
  "keyframes": {
    "pulse": { "frames": [ { "at": 0, "opacity": 1 }, { "at": 0.5, "opacity": 0.6 }, { "at": 1, "opacity": 1 } ] }
  }
}
```

Native: build a `UiTheme` (`UiTheme::new(name).with_color(..).with_rule(selector, UiStyleDelta{..})`)
and register it with `UiStyleSettings::register_theme` (validates), or insert an
`engine.UiTheme` entity. An entity theme replaces a native theme of the same
name; two entity themes with one name are `ui.theme.duplicate`.

A root uses `UiRoot.theme` if set, else `UiStyleSettings::active_theme()`; a
root with neither is not styled at all and bypasses the style step entirely
(zero cost, behaviour identical to before this layer existed).

**Theme switching at runtime.** Call `UiStyleSettings::set_active_theme("light")`
(or edit `UiRoot.theme`, or replace the `engine.UiTheme` data). The next tick
re-cascades every affected root; transitions defined in the new theme play
between the old and new target values.

`extends` layers a theme over a base (chain of at most 4, cycles are
`ui.theme.cycle`): tokens are merged with the derived value winning, base rules
come first in source order, derived rules after them, keyframes merge by name.
An unknown theme name is `ui.theme.unknown`.

### Tokens

`tokens.colors` (RGBA 0..=1), `tokens.numbers` (spacing and sizes) and
`tokens.fonts` (font keys); names use `[A-Za-z0-9_-]`, at most 64 bytes, 256 per
kind. A rule style value written as the string `"$name"` reads the token:

- colors (`background`, `foreground`, `accent`): color tokens;
- numbers (`padding`, `gap`, `rowGap`, `columnGap`, `grow`, `shrink`,
  `fontScale`, `opacity`, `scale`): number tokens;
- lengths (`width`, `height`): a number token read as pixels;
- `font`: font tokens (any `font` string not starting with `$` is literal).

An unknown token is `ui.style.token_unknown`. Token references cannot appear in
`UiNode.style` (inline) or in keyframes; they exist to restyle by theme.

### Rule styles (`UiStyleDelta`)

A rule's `style` is a partial `UiStyle`: every `UiStyle` field can be set
(same names and shapes; a test proves every field is reachable). Fields a rule
does not mention are left to lower-priority rules, so rules compose by
overriding only what they need.

## Selectors

```
list      := selector ("," selector)*              (at most 8)
selector  := compound ( combinator compound )*     (at most 4 compounds)
combinator:= whitespace            descendant
           | ">"                    child
compound  := ( "*" | kind )? ( "#id" | ".class" | ":state" )*     at least one part
kind      := container | label | rich_label | image | nine_slice | button | toggle
           | slider | progress | text_input | dropdown | scroll_view | list
           | virtual_list | tabs | modal_dialog
state     := hover | pressed | focus | disabled | enabled | checked | open | selected
```

Identifiers use `[A-Za-z0-9_-]`; a node ID with other characters cannot be
targeted by `#id` (use a class). Aliases: `hovered`, `active` (= pressed),
`focused`. `:enabled` means not `:disabled`. At most 256 bytes per selector
text. Malformed selectors are `ui.style.selector`.

State flags come from `GameUi` (`HOVERED`, `PRESSED`, `FOCUSED`, `DISABLED`,
`CHECKED`, `OPEN`, `SELECTED`): hover/press/focus come from pointer and
keyboard interaction, `:disabled` from the node's or an ancestor's `enabled`
flag or the root's, `:checked` from toggles, `:open` from open dialogs and
dropdown popups, `:selected` from the selected child of a `list`. Template
nodes of a `virtual_list` exist only as per-item replicas and carry no state
flags, so state pseudo-classes never match inside a virtual list (class and
kind rules still apply).

### Specificity and cascade order

Specificity is CSS-like: `ids << 16 | (classes + state pseudo-classes) << 8 |
kinds`, summed over all compounds of the selector. A selector list contributes
one entry per selector, so each selector has its own specificity. The cascade
for one node, in order (later steps override earlier ones):

1. `UiStyle::DEFAULT`.
2. Every matching rule, ascending by (specificity, source order). Source order
   is the position in the rules array, with a base theme's rules before the
   derived theme's. Equal specificity: the later rule wins.
3. **Inline style.** Every field of `UiNode.style` that differs from the
   default overrides the cascade. `UiStyle` fields are not optional, so a
   field equal to its default counts as "unset" (this matches serialization,
   which omits defaults). Consequence: an inline field set to exactly its
   default value cannot beat a rule; set any other value, or drop the field
   and use a rule.
4. Inheritance of `foreground`, `font` and `fontScale`: if the value is still
   the default, the node takes its parent's effective value.
5. Motion: the shown value of transitions in flight, then keyframe
   animation values.

`transition` and `animation` come from the winning rule that defines them (a
rule's non-empty `transition` list replaces lower lists; `animation` likewise).

The result is an *effective* `UiRoot` (same nodes, effective styles) that
layout, hit testing and drawing consume. There is a single resolution step
(`ui_tree::resolve`); interaction and layout run on the effective root.

**Timing.** In `tick_world`, interaction and layout use the effective root
built from the *previous* tick's state flags and motion values, so hit areas
always match what was last drawn; transitions then start on the tick that sees
the new state. Drawing (`extract_world`) re-resolves with the *current* flags,
so a hover look appears in the same frame the pointer enters.

## New style properties

- `opacity` (0..=1): multiplied into the node's and all its descendants' draws
  (ancestor product). Opacity 0 skips the subtree's draws.
- `scale` (0..=16): paint scale about the node's own border-box centre,
  inherited by descendants (each ancestor scales about its own centre).
  It never changes layout or hit testing; a scaled button keeps its unscaled
  hit area. Scaled clips scale with the geometry.

With a theme active the built-in "pressed darkens the background" tint is
disabled (`UiRenderState::themed`); the `:pressed` rule is the pressed look.

## Transitions

`rule.transition` is a list of `{ property, duration, delay?, easing? }`.
`property` is `all` or one of `background`, `foreground`, `accent`, `opacity`,
`scale`, `offset`, `width`, `height` (`width`/`height` animate only between
pixel values; other unit changes snap). A specific property overrides `all`.
When the cascaded target of a property changes between steps, a transition
starts from the *currently shown* value; a transition interrupted by another
change restarts from the interrupted value. The first time a node is seen it
takes its target with no transition. Delay holds the starting value;
`duration` and `delay` are 0..=3600 s. Put the list on the base rule (not on
`:hover`) so it also plays when the state is left.

Easing (`easing`): `linear` (default), `ease_in`/`ease_out`/`ease_in_out`
(quadratic), `cubic_in`/`cubic_out`/`cubic_in_out`, `{"bezier":[x1,y1,x2,y2]}`
(CSS `cubic-bezier`, `x1`,`x2` in 0..=1) and `{"steps":n}` (jump-end, 1..=64).
`Easing::apply` is public.

## Keyframe animation

`rule.animation` = `{ name, duration, delay?, iterations?, direction?, easing?,
hold?, essential? }` plays the theme keyframes `name` on matching nodes.

- Keyframes: `frames: [{ at, background?, foreground?, accent?, opacity?, scale?,
  offset? }]` with `at` strictly increasing in 0..=1 (at most 16 frames, 64 sets
  per theme). A property missing at 0 or 1 starts/ends at the node's base value.
- `iterations`: a count (1..=1000, default 1) or `"infinite"`.
- `direction`: `normal` (default), `reverse`, `alternate`, `alternate_reverse`.
- `easing` applies to the iteration progress before the frame lookup.
- `hold` keeps the final frame after the last iteration; otherwise the node
  returns to its base style.
- The animation restarts when its spec changes or its rule stops matching
  (for example a hover-only pulse restarts at each hover).
- `UiAnimation::phase(elapsed)` returns `Waiting`, `Running(progress)` or
  `Finished(progress)`; it is a pure function used by the runtime and tests.

Both are driven by the tick's `dt` (clamped to 0..=1 s per step, non-finite
counts as 0) with `f64` elapsed time for animations and `f32` for transitions;
there is no other clock, so the same `dt` sequence reproduces bit-identical
styles.

### Reduced motion

`UiStyleSettings::set_reduced_motion(bool)` / `reduced_motion()` is the global
setting (a resource; the accessibility settings write it). `UiRoot.reducedMotion`
overrides it per root (`UiStyleSettings::reduced_motion_for(root)`). When on:
every transition is instant (the target is shown on the step that sees the
change) and animations with `essential: false` do not play. `essential: true`
animations (progress spinners and the like) still run.

## Data binding

The `UiDataContext` resource holds JSON data (an object); `UiRoot.bindings`
connect widget properties to paths in it.

```json
"bindings": [
  { "node": "hp",   "property": "text",      "path": "player.hp", "format": "HP {}/{player.max:.0}" },
  { "node": "vol",  "property": "value",     "path": "audio.volume", "mode": "two_way" },
  { "node": "mute", "property": "checked",   "path": "audio.mute",   "mode": "two_way" },
  { "node": "inv",  "property": "itemCount", "path": "items" },
  { "node": "rowtext", "property": "itemText", "path": "items", "itemPath": "name", "format": "{#}: {}" }
]
```

Native: `UiDataContext::set("player.hp", json!(7))`, `get`, `get_f64/get_bool/get_str`,
`remove`, `merge`, `version`; `UiBinding::one_way(node, UiBindProperty::Text, path)`,
`UiBinding::two_way(..)`, `.with_format(..)`, `.with_message(key, args)`,
`.with_item_path(..)`; push bindings to `UiRoot::bindings`. An `engine.UiData`
component (`values`: object) merges its top-level keys into the context each
time the component changes (keys are never removed).

### Path grammar

`key ( "." key | "[" index "]" )*`; keys use `[A-Za-z0-9_-]`; at most 128 bytes,
16 segments, index up to 7 digits. Writes create objects along the path; an
array index may address an element or the next free slot. A write through a
value of another type, or past the end of an array, is `ui.bind.path`.

### Properties

| `property` | widgets | value |
| --- | --- | --- |
| `text` | label, rich_label, button, toggle (text); text_input (value, clipped to its `maxLength`, control characters dropped) | any scalar (stringified) or format/message output |
| `value` | slider, progress | number, clamped to `[min,max]` |
| `checked` | toggle | bool (or number != 0) |
| `selected` | dropdown, list, virtual_list, tabs | non-negative integer; `null` clears (dropdown/list/virtual list); out-of-range dropdown/virtual-list values clear it, tabs clamp |
| `visible`, `enabled` | any node | bool |
| `itemCount` | virtual_list | array or object length, or a number (capped at 1,000,000) |
| `itemText` | label, rich_label, button, toggle inside a virtual-list template | per item: element `i` of the array at `path`, text from `itemPath` |

A binding applies only once its `path` exists (a missing value never blanks a
widget). `mode: "two_way"` is allowed for text_input `text`, slider `value`,
toggle `checked` and `selected`; it cannot carry `format`/`message`. Each
(node, property) pair may be bound once. `itemText` is resolved when drawing
(formats use `{.field}`, `{.}` and `{#}`), so it does not influence layout
measuring (a virtual list has a fixed item extent).

### Format and messages

`format`: `{}` is this binding's own value, `{path}` another data path,
`{path:.N}` fixed precision (N is one digit), `{.field}` / `{.}` the current
item (itemText), `{#}` the item index, `{{` and `}}` escape braces; at most 256
bytes. Whole numbers print without a decimal point.

**String resolver seam.** `UiStringResolver::resolve(key, args) -> Option<String>`
is the hook for localization. Install it with
`UiBindingState::set_resolver(Arc<dyn UiStringResolver>)`. A binding with
`message` (a key; `.`, `$`, `_`, `-` and alphanumerics, 128 bytes) and
`args` (name to data path, item-relative for `itemText`; at most 8) shows the
resolver's text; `None` or no resolver falls back to `format`, then to the
value. After a locale switch call `UiBindingState::invalidate_strings()` so
every binding re-renders; `set_resolver`/`clear_resolver` do this themselves.
The locale-ui leaf plugs `engine-locale` in here; nothing in this layer depends
on it.

### Change detection

`UiDataContext::version` increases only when a write changes a value (writing
an equal value does not bump it). Per root, `UiBindingState` remembers the
version it last evaluated and the last value it applied for each binding:
unchanged versions skip the root entirely; on a changed version each binding is
evaluated but only bindings whose rendered value changed touch their widget.
`UiBindingState::stats()` counts `frames_evaluated`, `frames_skipped`,
`evaluated`, `applied` and `writes`.

One-way bindings apply when the *data* changes; they do not fight edits made
to the widget by other means in between. Two-way bindings compare each input
widget against the last value they saw after interaction each tick and write
changes to the data context; the write is not re-applied on the next tick.
The first observation of a widget only records a baseline (nothing is written
before the user changes something). The state is dropped when the root's
entity changes, the binding list changes, the string resolver changes or
`reset_world` runs (scene change/shutdown).

### Bounds

512 bindings per root; path 128 bytes/16 segments; format 256 bytes; 8 message
arguments; data values nest at most 16 deep; strings at most 64 KiB; arrays at
most 1,000,000 items; the whole context is estimated at most 16 MiB
(`ui.bind.invalid`). `engine.UiData` has at most 256 keys.

## World-space panels

`engine.UiWorldPanel` on an entity that also has `engine.UiRoot` and
`Transform` takes that root out of the screen-space UI and draws it on a quad:

| Field | Default | Meaning |
| --- | --- | --- |
| `size` | `[2, 1]` | quad size in world units (before the entity's scale) |
| `pixelsPerUnit` | `100` | UI pixels per world unit; `size x pixelsPerUnit` (rounded) is the UI viewport, at most 4096 per axis |
| `pivot` | `[0.5, 0.5]` | origin as a fraction of `size` from the top-left |
| `billboard` | `none` | `none`, `full` (face the active `Camera3D` on all axes) or `yaw` (about Y only) |
| `pointer` | `auto` | `auto` (active `Camera3D`, else `Camera2D`), `camera2d`, `camera3d`, `none` |
| `keyboard` | `false` | also receive `ui_*` navigation actions |

The entity's world matrix places the quad (UI pixel `(0,0)` is the top-left
corner; the UI's y axis points down, the panel's up). `UiWorldPanel::corners(matrix)`
returns the world positions of the four corners, `placement(world, camera)`
applies billboarding (position and scale are kept), `pick(matrix, origin,
direction)` converts a ray into a UI pixel, `camera3d_ray` / `camera2d_ray`
build rays from a camera and a viewport pixel.

**Rendering** stays on the existing `Draw` path: the root's UI draws are
extracted for the panel's pixel viewport, clipped on the CPU with
`clip_quads` (UI meshes are axis-aligned quads; each is cut to its clip
rectangle with UVs adjusted) and emitted as `DrawSpace::World` meshes with
ids `world:game-ui:...`. Consecutive draws are offset along the panel normal by
`PANEL_Z_STEP` (1e-4 world units, capped at 0.05) so layers do not share a plane.

**Pointer.** Each tick the screen pointer becomes a ray from the chosen camera
(`ActionFrame::pointer` in the camera's logical viewport pixels), the ray is
intersected with the panel plane and, inside the quad, the hit becomes the
panel's own `ui_click` pointer in panel pixels; a miss disables pointer
actions (and clears hover). Every panel has its own `GameUi` (`UiWorldPanels`),
so focus, popups and capture are independent of the screen UI and of other
panels. Without `keyboard`, `ui_*` actions other than `ui_click` are removed.
Events are delivered with the panel entity as source, like screen roots.

**Limitation (3D scenes).** `Draw::World` meshes are composited by the 2D
sprite pass with the `Camera2D`. In a scene with a `Camera3D` the panel's
geometry is correct in world space (and `pointer: "camera3d"` picks it), but it
is not depth-tested against the 3D scene: a presenter that supports it must
consume the same `Draw` list (or `world_mesh`) through the render extension
API. Until then billboarding and 3D pointer picking are verified by geometry
tests, not by a rendered 3D frame. VR controller rays are not read from input;
a presenter can call `UiWorldPanel::pick` with its own ray.

## Limits and error codes

| Limit | Value |
| --- | --- |
| rules per theme | 512 |
| compiled selectors per composed theme | 2048 |
| tokens per kind | 256 |
| keyframe sets per theme / frames per set | 64 / 16 |
| transitions per rule | 8 |
| `extends` chain | 4 |
| selector length / compounds / list | 256 bytes / 4 / 8 |
| transition, delay, animation duration | 3600 s |
| finite animation iterations | 1000 |
| bindings per root | 512 |
| world panels | 64; surface 4096 px per axis |

Stable codes: `ui.style.invalid`, `ui.style.selector`, `ui.style.token_unknown`,
`ui.style.keyframes_unknown`, `ui.theme.unknown`, `ui.theme.cycle`,
`ui.theme.duplicate`, `ui.bind.invalid`, `ui.bind.path`, `ui.world_panel.invalid`
(plus the existing `ui.tree.invalid`). `UiTheme::validate` checks structure,
selectors, motion values and (for themes without `extends`) token and keyframe
references; themes with `extends` get the full check when composed.

## Serialized size

A tree is limited to 64 KiB as an authored component. Measured in
`tests/ui_style.rs` (JSON bytes):

| Tree | Bytes |
| --- | --- |
| 1024 fully styled buttons, inline styles (the ui-core measurement) | 144,717 |
| the same 1024-node grid, styles moved to a theme (rows by class, buttons by `.row > button`) | 70,041 |
| 1017-node card screen (127 cards of 8 nodes), classes + theme | 60,717 |
| the same card screen with the theme's look written inline | 104,731 |

The limit is about 64 bytes per node. A node costs roughly 45 bytes as a
class-only container, about 90 with a text widget and a class. Stylesheets
remove the repeated style; they cannot remove the per-node `id`, `parent` and
widget wrapper, so a 1024-node tree fits when roughly half its nodes are layout
containers (the card screen) and does not when every node is a text widget
(the grid is still 70 KB even without styles). Use a `virtual_list` for
repeated rows.

## Design record

- **Theme as a component, not an asset.** An `engine.UiTheme` entity (or a
  native registered theme) is validated by the shared catalog, snapshots, edits
  through generic `component_set`, and needs no content-pipeline hook (the
  text-ui leaf owns runtime content loading). A file asset would need a new
  asset kind plus loader and would not be MCP-editable with the existing
  tools. Several themes coexist by name; switching is a settings change.
- **Resolution produces an effective `UiRoot`.** The cascade writes effective
  styles into a clone of the root and hands that to the unchanged layout,
  interaction and render code, so there is one hook (`world.rs`) and no second
  style type in layout. The price is a tree clone per themed root per step.
- **Inline wins by "differs from default".** `UiStyle` fields are plain values;
  turning them into options would break the version-1 JSON/API surface. Treating
  default-valued inline fields as unset is exactly what serialization already
  does, and the rule is documented above.
- **Paint-only scale/opacity.** `scale` and `opacity` live in the painter so
  layout, hit testing and navigation geometry stay exact and deterministic.
- **Binding on a JSON context.** A `serde_json::Value` tree with string paths is
  simple, bounded, authorable (`engine.UiData`, MCP) and needs no reflection over
  ECS component fields. ECS state reaches the UI through `UiDataContext::set` from
  a system. Change detection is a version counter plus a per-binding last value,
  so unchanged frames cost one integer comparison per root.
- **Per-item text at draw time.** A virtual list replicates one template node,
  so per-item text cannot be written into the tree; a small `UiItemTexts` hook in
  `UiRenderState` substitutes text for each replica while drawing.
- **World panels on `Draw`.** The render extension API is not part of this
  branch; extracting `DrawSpace::World` meshes keeps the panel working on every
  presenter that draws `Draw` lists and leaves 3D composition to the rendering
  domain.

## Known gaps

- World panels are not depth-composited into 3D scenes (see above).
- `scale` does not affect hit areas or layout; opacity/scale apply to a virtual
  list's replicas through their template only.
- Only the eight listed properties transition or animate; layout properties
  (padding, gap, grid tracks) switch instantly.
- Per-item text does not take part in layout measuring.
- Selectors have no `:not`, attribute or sibling selectors; `:selected` is set by
  `GameUi` for `list` children only.
- A theme change replaces the cascade for the whole root; there is no per-node
  theme and no `@media`-style conditions.
