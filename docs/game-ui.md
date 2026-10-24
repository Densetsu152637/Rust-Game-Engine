# Retained game UI

`engine_project::ui_tree` is the game UI toolkit. A `UiRoot` ECS component
owns a serializable `UiTree`; node IDs connect hierarchy, keyboard focus,
pointer interaction, and event payloads. The portable editor uses its own UI.
Existing `UiModel` text panels keep their modal behavior and suppress game UI
interaction through the same `gameplay_frame()` input gate.

The tree is a flat, ordered vector with optional parent IDs. This keeps scene
JSON editable by MCP and native Rust systems without a second presentation
state. Layout and hit testing use the same calculated rectangles. Rendering
reuses native `Draw`, mesh, font-atlas and clipping interfaces, avoiding an
editor toolkit dependency in game executables.

## Authored component

The component name is `engine.UiRoot`, version 1. Its descriptor is returned by
`ui_tree::descriptor()` for the shared component catalog and JSON inspector.
The descriptor supplies field metadata; typed validation additionally rejects
cycles, missing parents, variant mismatches, and invalid ranges. As with other
typed authored components, the value sits in `components.custom`:

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
                "height": { "unit": "pixels", "value": 140 },
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
              "style": {
                "width": { "unit": "fill" },
                "height": { "unit": "pixels", "value": 32 },
                "font": "asset:ui-font"
              }
            },
            {
              "id": "music",
              "parent": "menu",
              "widget": { "kind": "toggle", "text": "Music", "checked": true, "action": "music" },
              "style": { "font": "asset:ui-font" }
            },
            {
              "id": "volume",
              "parent": "menu",
              "widget": { "kind": "slider", "value": 0.5, "min": 0, "max": 1, "step": 0.05, "action": "volume" }
            }
          ]
        }
      }
    }
  },
  "EventBindings": {
    "bindings": [
      { "event": "ui.clicked", "handler": "menu_action" },
      { "event": "ui.value_changed", "handler": "menu_action" }
    ]
  }
}
```

Declare `asset:ui-font` as an existing font asset with its atlas texture. Widgets
with nonempty text require a valid font; missing fonts/textures return errors.
Empty text and purely geometric controls do not require a font. Other widgets
are `container`, `label` with `text`, and `progress` with `value`, `min`, `max`.
`action` is application data delivered in the event, not a Rust function name.

## Layout, styling, and input

All coordinates are top-left logical pixels, using the camera's logical size.
Widths and heights accept `{ "unit": "pixels", "value": 120 }`,
`{ "unit": "percent", "value": 0.5 }`, or `{ "unit": "fill" }`.
Percent is a fraction of the parent's inner dimension. Row/column layouts
share remaining main-axis space equally among fill children after fixed sizes
and gaps. Cross-axis fill occupies the available inner dimension. Absolute
children use their offsets; row/column offsets move the allocated box without
changing sibling allocation. Root nodes are absolute within the viewport.
Overflow is clipped rather than implicitly shrinking fixed-size children.

Styles include `padding`, `gap`, `clip`, `background`, `foreground`, `accent`,
`font`, and `fontScale`. Colors are four floats in 0..=1. Clipping defaults on
and is inherited; disabling a node's clip lets descendants extend beyond that
node, but never beyond an earlier ancestor clip or the viewport. Visibility
and enabled state are inherited. Hidden nodes consume no layout space.
Disabled controls draw at half opacity and cannot receive focus or clicks.

Sibling order is vector order, even if a parent occurs later in the vector.
Painting is parent-before-children depth-first; the last interactive rectangle
under a pointer wins. Roots sort by `(layer, root ID)`. Keyboard traversal uses
the same stable order and skips disabled, hidden, and fully clipped controls.

Bind these logical actions through the existing input binding map:

| Action | Effect |
| --- | --- |
| `ui_click` | Activate under the pointer on the pressed edge; hold to drag a slider |
| `ui_next` or `ui_down` | Focus the next visible enabled control, wrapping |
| `ui_previous` or `ui_up` | Focus the previous control, wrapping |
| `ui_confirm` | Activate the focused button or toggle |
| `ui_left`, `ui_right` | Decrease/increase the focused slider by its step |

Slider values snap to steps from `min`, clamp at both endpoints, and emit only
when their value changes. Pointer capture continues outside the slider until
release, window focus loss, modal input capture, or removal/disablement. A
button click emits `ui.clicked`; toggles and sliders emit `ui.value_changed`.
The source and target are the owning root entity. The payload is:

```json
{ "node": "music", "action": "music", "value": false }
```

Buttons use `null` for `value`. Register a callback with
`register_event_handler(app, "menu_action", handler)` and bind it on the root
entity using `EventBindings`. Callbacks run through the ordinary bounded event
queue; they can inspect the node/action and mutate any native ECS component.
Widget values remain in the typed `UiRoot`, so runtime snapshots include them.

## Native integration and limits

`UiTree::layout(viewport)` returns `UiBox` geometry. `GameUi::tick` operates on
`(root ID, UiRoot)` pairs, retaining focus and slider capture while returning
widget events. `UiRoot::extract_draws` returns native geometry and font draws.
The runtime adapter functions are `ui_tree::register`, `tick_world`, and
`extract_world`. Register the typed adapter before authored component hydration;
tick after event dispatch using `UiModel::gameplay_frame()` to keep new UI events
for the next dispatch; append extracted draws before final frame sorting.
Clear `GameUi` on scene changes and shutdown. These hooks also support custom
hosts that own the ECS and render pipeline directly.

Bounds are 64 roots, 1024 nodes per root, hierarchy depth 64, 128-byte IDs and
actions, 4096-byte individual labels and 64 KiB of text per root. Logical
viewports and pixel lengths are bounded to 16384. JSON component values also
obey the shared component catalog's 64 KiB serialized limit. Cycles, duplicate
IDs, orphan references, nonfinite numbers, invalid colors, and degenerate
slider/progress ranges return errors. Native mutations are revalidated at
layout/input/extraction boundaries.

This toolkit does not implement editable text, IME, accessibility integration,
scroll views, responsive breakpoints, or flex shrink. There is no native text
event pipeline to support text input yet; labels and buttons render existing
font-atlas text. Modal scrolling text remains available through `UiModel`.

Run `cargo test -p engine-project --test game_ui` for layout, clipping, keyboard
focus, slider capture, mesh/font extraction, schema round-trip, and native
callback integration coverage.
