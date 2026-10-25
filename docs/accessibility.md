# Accessibility

Accessibility for the retained game UI ([Retained game UI](game-ui.md)) has five
parts:

- a platform-neutral **accessibility tree** built from the laid-out UI;
- **action requests** that let an assistive technology (AT) drive widgets through
  the same code as keyboard and pointer input;
- a **screen-reader bridge** in `engine-desktop` that converts the tree to
  [AccessKit](https://github.com/AccessKit/accesskit) and serves it through the
  platform accessibility API;
- **accessibility settings** (`engine.AccessibilitySettings`): UI scale, text scale,
  minimum text size, reduced motion, high-contrast theme, focus ring, colour-vision
  filter, caption preferences;
- an **input-remapping screen** generated from the project's action bindings.

Captions and subtitles are described in [Captions and subtitles](#captions-and-subtitles).
The colour-vision filter is a render feature: see
[Colour-vision filter](#colour-vision-filter).

## Status

| Feature | State | Evidence |
| --- | --- | --- |
| Neutral tree: roles, names, values, states, bounds, actions, ordered children, live regions | implemented | `cargo test -p engine-project --test accessibility` |
| Modal dialogs hide the content beneath; hidden tabs and hidden subtrees are excluded | implemented | same |
| Incremental diff (only changed nodes) | implemented | same, and `engine-desktop` conversion tests |
| Action requests: focus, click, increment, decrement, set value (numeric and text), expand, collapse, scroll into view | implemented | same |
| AccessKit conversion and `accesskit_winit` adapter | implemented | `cargo test -p engine-desktop --lib accessibility` |
| Windows UI Automation | **observed** with a UI Automation client (PowerShell `System.Windows.Automation`), see [Platform status](#platform-status) | ignored window test |
| NVDA, Narrator, JAWS | **not verified** (no screen reader was run) | none |
| macOS (NSAccessibility), Linux (AT-SPI) | **not built and not run** in this work (only Windows was available) | none |
| Settings: UI scale, text scale, minimum text size, reduced motion, high-contrast theme, focus ring | implemented | `accessibility` tests |
| Colour-vision filter (protanopia, deuteranopia, tritanopia, achromatopsia; simulate or daltonize; strength) | implemented for 3D frames (PostProcess stage of `engine-ui-render`); not applied to the 2D sprite/UI layer or 2D-only frames | `cargo test -p engine-project --test ui_gpu`, `-p engine-ui-render` and the ignored GPU test `the_colour_filter_turns_red_and_green_patches_into_the_simulated_colours` |
| Remapping screen with capture, conflicts, reset, events | implemented | `accessibility` tests |
| Captions: `caption_track` asset, runtime controller, `engine.CaptionPlayer`, presentation, caption settings, live region, WebVTT/SRT import | implemented | `cargo test -p engine-project --test captions`, `-p engine-mcp --test accessibility` |
| Captions driven by an audio source's playback position | hook only (`CaptionClock`); `engine-audio` exposes no position clock yet | see [Time source](#time-source) |
| Text selection and caret exposed to AT | not implemented | see [Limits](#limits) |

## Tree model

`GameUi::accessibility_tree()` returns the `AccessTree` of the last tick. It is
plain data (`Serialize`, `Deserialize`); nothing in it depends on AccessKit.

The tree is **off by default** and costs nothing then. It is built every tick
once either of these is true:

- the desktop bridge reports a connected AT (`GameUi::set_accessibility(true)`);
- `AccessibilitySettings.screenReader` is set (a hint authors or a settings
  screen can use, for example in tests or when another bridge is used).

Structure: the root is a `window` node. Each visible `UiRoot` is an unnamed
`group` child of it, in paint order (layer, then root ID). Every visible widget
node becomes one `AccessNode` below its root group, in vector order of siblings.
Open modal dialogs are appended to their root group (they are laid out above
the base tree). Extra synthetic nodes exist where a widget has parts that
assistive technology needs to address:

| Parent | Synthetic child | `AccessPart` |
| --- | --- | --- |
| `tabs` | one `tab` per label, before the selected panel | `Tab(n)` |
| `modal_dialog` | one `button` per footer button, after the content | `Button(n)` |
| open `dropdown` | a `list_box` holding one `list_box_option` per option | `Popup`, `Option(n)` |

### Node IDs

`AccessId` is the root ID, the node ID, the virtual-list item index (for
replicas of a template) and the synthetic part. It is stable across ticks and
needs no registration. `AccessId::numeric()` is a 64-bit FNV-1a hash of the four
parts for platform APIs that want a number (the window is `1`; no other ID is
`0` or `1`). If two IDs ever collided the later node would be dropped and counted
in `AccessTree::dropped`; that cannot be hit by accident (2^-64 per pair).

### Visibility rules

A node is **excluded** (with its subtree) when:

- it or an ancestor has `visible: false` (no layout box);
- it is a panel of a `tabs` widget that is not selected;
- its `accessibility.hidden` is set;
- a modal dialog is open and the node is not inside the topmost open dialog.
  Everything beneath the modal is removed, **including other roots**. The
  tree's `modal` field names the dialog and the focus is inside it, matching
  `GameUi`'s focus trap.

Disabled nodes stay in the tree with `disabled: true` and no input actions.
Nodes scrolled out of a scroll view stay in the tree; their parents have
`clips_children: true` so a platform can tell they are not visible.

### Node data

| Field | Meaning |
| --- | --- |
| `role` | See [Roles](#roles-names-and-states). |
| `name` | `accessibility.label`, else the widget text, else the tooltip text. |
| `description` | `accessibility.description`, else the tooltip text when the name came from elsewhere. |
| `placeholder` | Placeholder of an empty input or dropdown. |
| `value` | `Text` (text input, dropdown choice) or `Number {value, min, max, step}` (slider, progress; step `0` is read-only). A password is **never** exposed. |
| `states` | `checked`, `expanded`, `selected` (tri-state: `None` means not applicable), `disabled`, `focused`, `focusable`, `password`, `read_only` (disabled text input), `modal`. |
| `bounds` | Border box in **window logical pixels** (after UI scale). |
| `clips_children`, `scroll` | Scroll views, virtual lists and clipped nodes; scroll offset and maximum. |
| `actions` | Subset of `focus`, `click`, `increment`, `decrement`, `set_value`, `scroll_into_view`, `expand`, `collapse`. |
| `children` | Reading order. |
| `live` | `off`, `polite` or `assertive` (`accessibility.live`). |
| `position` | 1-based position and set size of list items, tabs and options. Virtual lists report the full item count. |

`AccessTree` also carries `focus` (the focused node, else the window) and `modal`.

### Incremental updates

`tree.diff(previous)` returns an `AccessUpdate`: the nodes that are new or whose
data changed, the IDs removed, and the focus. With no previous tree it returns
the whole tree (`full`). One slider value change emits exactly one node; a node
that appears or disappears also emits its parent (its `children` changed). The
window node's focus is not a node state, so moving focus changes only the nodes
that gained or lost `focused`.

## Roles, names and states

| Widget | Role | Name | Value / states | Actions |
| --- | --- | --- | --- | --- |
| `container` | `group` | label only | | |
| `label`, `rich_label` | `label` | text; for `rich_label` the plain text of the markup (bracket tags removed, escapes resolved, `<...>` tags and entities of the bitmap dialect removed, inline images dropped; unknown tags stay literal as they do when drawn, and markup that does not parse still has every `[...]` group removed) | | `click` when the markup has a `[link=` tag |
| `image`, `nine_slice` | `image` | label, else tooltip | | |
| `button` | `button` | text | | `focus`, `click` |
| `toggle` | `check_box` | text | `checked` | `focus`, `click` |
| `slider` | `slider` | label, else tooltip | number with step | `focus`, `increment`, `decrement`, `set_value` |
| `progress` | `progress_bar` | label, else tooltip | number, step 0 | |
| `text_input` | `text_input`, `multiline_text_input` or `password_input` | label, else tooltip | text (not for passwords), `placeholder`, `password`, `read_only` when disabled | `focus`, `click`, `set_value` |
| `dropdown` | `combo_box` | label, else tooltip | chosen option text, `expanded` | `focus`, `click`, `expand` or `collapse` |
| `scroll_view` | `scroll_view` | label only | scroll offsets | |
| `list` | `list` | label only | | children below |
| list child (container, label or image) | `list_item` | own text, else descendant text | `selected`, position | `click` selects |
| `virtual_list` | `list` | label only | size of set | replicas as `list_item` with `item` IDs; a replica is named from its bound per-item text (`itemText` bindings, see [ui-styling.md](ui-styling.md)), as it is drawn, so rows have distinct names |
| `tabs` | `tab_list` | label only | | synthetic `tab` children; selected panel as `tab_panel` named by its tab |
| `modal_dialog` | `dialog` | title | `modal` | synthetic `button` children |

Every node also supports `scroll_into_view`. A list item, tab panel or button
without text takes its name from descendant text (at most 1024 bytes). The
authored `accessibility.role` replaces any role in the table.

## Authored `accessibility` field

`UiNode.accessibility` is optional and omitted when unset, so existing version-1
scenes load unchanged:

```json
{ "id": "volume", "widget": {"kind": "slider", "value": 0.5, "min": 0, "max": 1, "step": 0.1},
  "accessibility": { "label": "Music volume", "description": "Arrow keys change it in tenths" } }
```

| Field | Rule |
| --- | --- |
| `label` | Accessible name, at most 256 bytes, no control characters. |
| `description` | At most 1024 bytes. |
| `role` | Any role except `window`: `group`, `label`, `heading`, `image`, `button`, `link`, `check_box`, `switch`, `slider`, `progress_bar`, `text_input`, `multiline_text_input`, `password_input`, `combo_box`, `list_box`, `list_box_option`, `list`, `list_item`, `scroll_view`, `tab_list`, `tab`, `tab_panel`, `dialog`, `alert`, `status`, `region`. |
| `hidden` | Exclude the node and its subtree. |
| `live` | `off`, `polite`, `assertive`. |

Both strings count toward the 64 KiB per-root text budget. It appears in the
`engine.UiRoot` descriptor and is editable with `component_set`. Native
builders: `with_access_label`, `with_access_description`, `with_access_role`,
`with_access_live`, `access_hidden`.

## Action requests

```rust,ignore
ui.request_access(UiAccessRequest {
    target: AccessId::node("hud", "volume"),
    action: UiAccessAction::SetNumeric(0.6),
})?;
// next tick applies it; the widget event fires as for the pointer
let outcomes = ui.take_access_outcomes(); // Applied / NotFound / Blocked / Unsupported / Rejected
```

`request_access` validates the request and queues it (at most 64 per tick; more
fail with `ui.access.overflow`, an over-long ID or non-finite number with
`ui.access.invalid`). The next `tick` applies the queue before pointer and
keyboard input, whether or not the window is focused, using the same functions
as input, so the events are **identical** to the input events. Tests compare a
request's event with the pointer's and the keyboard's.

| Target | Action | Effect and event |
| --- | --- | --- |
| button | `click` | `ui.clicked` |
| toggle | `click` | `ui.value_changed` (new bool) |
| slider | `increment`, `decrement` | one step, `ui.value_changed` |
| slider | `set_numeric(v)` | snapped to the step grid; outside the range is `Rejected`; `ui.value_changed` |
| text input | `set_text(s)` | replaces the text, caret to the end, `ui.text_changed`; over `maxLength`, control characters, or a newline in a single-line input are `Rejected` |
| text input | `click`, `focus` | focus (and IME) |
| dropdown | `click`, `expand` / `collapse` | opens or closes the popup |
| dropdown option part | `click` | `ui.selection_changed`, popup closes |
| tab part | `click` | `ui.tab_changed` |
| dialog button part | `click` | `ui.dialog_result` with the button index |
| list item, virtual-list replica | `click` | `ui.selection_changed` with the index |
| rich label | `click` | `ui.link_activated` for its first link |
| any node | `scroll_into_view` | scrolls every scroll view and virtual list above it; `ui.scrolled` |
| focusable | `focus` | focus; `Unsupported` otherwise |

A request for a disabled node, or for a node outside the open modal, is
`Blocked`; a missing node is `NotFound`; an action the widget lacks is
`Unsupported`. Focusable targets also take focus, as a pointer press does.

Keyboard navigation uses the same scrolling: when `ui_next`, `ui_previous` or a
direction key moves focus, the new focus is scrolled into view. To make that
possible, a focusable widget scrolled out of a scroll view is now a focus
candidate (it has no visible area, so a pointer cannot hit it).

## AccessKit integration

`engine-desktop::accessibility` holds the conversion and the bridge.

**Conversion** (pure, unit-tested): `access_role`, `access_node`,
`access_tree_update`, `access_action_request`.

- Unnamed `group` nodes become `GenericContainer`; `label` nodes carry their text
  as the AccessKit value; other names are labels.
- `bounds` map to AccessKit rectangles in UI logical pixels. The **window node
  carries the transform** (uniform scale and letterbox offset of the camera fit,
  `AccessWindow::from_camera`), so the platform sees window pixels.
- `position` becomes `position_in_set` (0-based in AccessKit) on the item and
  `size_of_set` on its container.
- An update contains only changed nodes, the focus, and `TreeInfo` once. The
  window node is re-sent when the title or transform changes (resize).

**Bridge** (`AccessBridge`, created by the desktop runner):

1. The adapter is created right after the window, **before it is shown**
   (the runner already creates the window invisible and shows it after the
   surface exists). This is a requirement of `accesskit_winit`, which panics for
   a visible window.
2. It uses `Adapter::with_direct_handlers`, so no custom winit user event is
   needed. The handlers run on platform threads (UI Automation calls from its own
   thread); they only set an atomic flag or push into a bounded mutex queue.
3. **Lazy initial tree.** The adapter calls `request_initial_tree` when an AT
   first connects. The bridge sets the connected flag and returns `None`; from
   that moment the tick builds the tree (`set_accessibility(true)`) and
   `Adapter::update_if_active` sends the full tree on the same tick. Nothing is
   built before an AT connects, and deactivation turns the tree off.
4. Each tick (after the UI tick, next to the IME sync) the bridge forwards queued
   platform actions as `UiAccessRequest`s, diffs the tree against the last one
   sent, and sends the update. `WindowEvent`s are passed to
   `Adapter::process_event` before the application handles them.
5. `AccessibilityControl.bridge` publishes `AccessBridgeStatus`:
   `Unavailable(reason)` (the default; headless runs and hosts without the
   bridge), `Idle` (installed, nothing connected) or `Connected`.

Mapped platform actions: `Click`, `Focus`, `Increment`, `Decrement`, `Expand`,
`Collapse`, `ScrollIntoView`, `SetValue` with text or a number. Others (context
menus, scroll by unit, text selection, `ReplaceSelectedText`) are dropped.

### Platform status

Observed on this machine (Windows 11):

- the adapter is created without panicking and the window shows;
- a **UI Automation client** (PowerShell `System.Windows.Automation`, started
  while the ignored test `window_probe_creates_the_adapter_and_serves_the_tree`
  ran) found the window, listed its children (text `Settings`, button `Play`,
  check box `Mute`, slider `Volume`, an image) with their names, roles and window
  rectangles, invoked `Play` (the UI received `ui.clicked`), set the slider
  through `RangeValuePattern.SetValue(1.0)` (`ui.value_changed 1.0`; the value
  read back as 1) and toggled the check box through `TogglePattern`
  (`ui.value_changed false`). The bridge reported `Connected`.

Not verified: NVDA, Narrator, JAWS and VoiceOver were not run, and macOS and
Linux were not built. `cargo build --workspace` pulls
`accesskit_macos` and `accesskit_unix` (zbus, atspi, all pure Rust, no system
libraries) on those targets by dependency metadata only. UI Automation client
success demonstrates the provider and the tree; it does not prove how a given
screen reader announces the content.

Run the window probe yourself with:

```text
cargo test -p engine-desktop --lib window_probe -- --ignored --nocapture
```

It keeps a 640x360 window open for `ENGINE_A11Y_PROBE_SECS` seconds (default 5)
and prints the events the platform caused.

## Settings

`engine.AccessibilitySettings` (version 1) is an entity component and the
native resource `AccessibilityControl { settings, bridge }`. Attach the
component to any entity (the lowest entity wins) or use the resource from code.
Each UI tick, `accessibility::sync`:

1. copies a *changed* component into `AccessibilityControl::settings` (native
   code can still write the resource between component edits);
2. applies changes of `reducedMotion` and `highContrastTheme` to
   `UiStyleSettings`, only when the setting changed, so default settings never
   overwrite values native code set on `UiStyleSettings`;
3. tells `GameUi` the `screenReader` hint;
4. returns the `UiScaling` for the tick.

| Field | Range, default | Effect |
| --- | --- | --- |
| `uiScale` | 0.5 to 4, 1 | Scales layout, drawing and hit testing together, see below. |
| `textScale` | 0.5 to 4, 1 | Multiplies every node's `fontScale` (result clamped to 16). Authored data is not modified. Applies to screen UI and world panels. |
| `minTextSize` | 0 to 96 px, 0 (off) | Smallest line height text may have **on screen**, measured per font with the active text backend (so UI scale is accounted for). Raises `fontScale` when needed. |
| `reducedMotion` | false | `UiStyleSettings::set_reduced_motion`; a root's `reducedMotion` still overrides it. |
| `screenReader` | false | Build the accessibility tree every tick. |
| `focusThickness` | 1 to 16 px, 2 | Focus ring thickness. |
| `focusHighContrast` | false | Draw the ring as a white ring inside a black one so it shows on any background. |
| `highContrastTheme` | none | Name of a `UiTheme` made the active theme while set; clearing restores the previous active theme. Roots that name their own `theme` keep it. A name no theme has fails the tick with `ui.theme.unknown`, like `set_active_theme`. |
| `colorFilter` | `none`, `protanopia`, `deuteranopia`, `tritanopia`, `achromatopsia`; `none` | Colour-vision filter of the 3D scene, see [below](#colour-vision-filter). |
| `colorFilterMode` | `simulate`, `correct`; `simulate` | Show the deficiency, or daltonize for it. `correct` with `achromatopsia` is rejected (nothing to correct). |
| `colorFilterStrength` | 0 to 1, 1 | Blend from the unfiltered scene to the full filter. |

Native API (all validated, `accessibility.invalid` on a bad value):

```rust,ignore
let mut control = world.resource_mut::<AccessibilityControl>();
control.set_ui_scale(1.5)?;
control.set_text_scale(1.25)?;
control.set_reduced_motion(true);
control.settings.high_contrast_theme = Some("high-contrast".into());
control.set_color_filter(ColorFilterKind::Deuteranopia, ColorFilterMode::Correct, 0.8)?;
```

The component is in the catalog (`component_schema`) and validated on
`component_set` like other components: wrong ranges, unknown fields and bad
theme names are rejected. Invalid values written directly to the resource are
ignored (the last applied settings stay).

### Colour-vision filter

`colorFilter`, `colorFilterMode` and `colorFilterStrength` drive the `ui.color_filter`
render feature of [engine-ui-render](ui-render.md#colour-vision-filter). Each extraction
publishes the effective settings into the UI GPU queue; the feature applies one 3x3 matrix
to the linear HDR scene colour in the PostProcess stage (after `engine.post`, before
tonemapping), so strength 0 or `none` costs nothing and a frame with the filter off is
untouched.

- **Simulate** shows the scene as a person with the deficiency sees it, for testing
  palettes. **Correct** (daltonization) moves the colour difference the deficiency loses
  into channels that stay distinguishable, for players with the deficiency. Achromatopsia
  is greyscale (Rec. 709 luminance) and has no correction.
- **Colour space.** Linear sRGB (Rec. 709) RGB, because the published matrices are defined
  there and the HDR scene is linear. Simulation uses the severity-1.0 matrices of
  G. M. Machado, M. M. Oliveira and L. A. F. Fernandes, "A Physiologically-based Model for
  Simulation of Color Vision Deficiency", IEEE TVCG 15(6), 2009; `strength` interpolates
  linearly from the identity, as the paper does between severities. The correction shifts
  the simulation error with `[[0,0,0],[.7,1,0],[.7,0,1]]` for protanopia and deuteranopia
  (Fidaner, Lin and Dhar, "Daltonize", 2005); the tritanopia shift `[[1,0,.7],[0,1,.7],[0,0,0]]`
  is this engine's analogue and not a published constant.
- **Scope.** Only the 3D scene is filtered. The 2D sprite/UI layer is composed after
  tonemapping and has no pixel-read stage, and a game without a `Camera3D` has no
  PostProcess stage, so the filter is not applied there. Design 2D palettes with the
  CPU reference (`engine_ui_render::color::apply_linear`) or the simulate mode of a 3D
  preview.
- **Status.** `RuntimeSnapshot.ui_render` lists `ui.color_filter` as `gpu` or `fallback`
  with the reason (feature not registered, not prepared by a wgpu renderer, or requested
  but no 3D frame has run it).

Register the feature with `engine_ui_render::install(app)` (or `register`).

### UI scale

`uiScale = s` lays the UI out in a viewport of `ceil(window / s)` logical pixels
and scales the drawn geometry and clip rectangles by `s`. The pointer is divided
by `s` before hit testing, so layout, drawing and hit testing agree exactly; with
`s = 1.5` a button authored at (100, 50, 100x40) covers (150, 75) to (300, 135)
for drawing, hit testing and `AccessNode::bounds`. `GameUi::ime_area()`,
`GameUi::tooltip()` and the accessibility bounds are reported in window pixels;
`GameUi::scale()` returns the scale. The viewport is rounded up so the scaled UI
always covers the window (up to `s - 1` pixels of the UI fall off the right and
bottom edges). Text is scaled as geometry, like the camera fit: bitmap and
coverage-atlas glyphs are not re-rasterised, so large scales look softer than
`textScale`, which re-lays out the text at the larger size. World panels are not
affected by `uiScale`.

### Focus ring

The ring drawn on the focused widget follows `focusThickness` and
`focusHighContrast` (`UiRenderState::focus_thickness`, `focus_contrast`).

## Input-remapping screen

`engine_project::remap_ui` builds a `UiRoot` from the action bindings and drives
the "press to rebind" flow with the existing input API: `validate_bindings`,
`binding_registry` and `InputSettings::rebind` (which bumps `revision`; the
physical `ActionMapper` rebinds on the next tick). No input code was changed.

```rust,ignore
let entity = world.spawn_empty().id();
let mut controller = RemapController::new(world, entity, RemapOptions {
    font: "asset:ui-font".into(),
    labels: [("jump".into(), "Jump".into())].into(),
    ..Default::default()
})?;
// every tick, after Runtime::tick:
for event in controller.tick(world)? { /* RemapEvent */ }
```

- `RemapScreen` is the pure model (`new(defaults, current, options)` or
  `from_world`): `root()` generates the `UiRoot`; `press(node)`,
  `capture_token(token)`, `reset(action)`, `reset_all()`, `cancel()` change it
  and return `RemapEvent`s. Defaults come from the project manifest, current
  bindings from `InputSettings`.
- Layout: a title, a vertical scroll view with one row per action (action name,
  one button per binding showing the key as people read it, a `+` button to add a
  binding, a `Reset` button and a conflict line), then a footer with `Reset all`
  and a live status line. Node IDs are `remap.<action>.slot.<n>`,
  `remap.<action>.add`, `remap.<action>.reset`, `remap.<action>.conflict`,
  `remap.reset_all`, `remap.status`.
- Flow: activating a binding button (click, `ui_confirm`, an AT `click`) starts a
  capture and the button reads `Press a key...`. The next new key, mouse button
  or gamepad button replaces that binding (or appends for `+`). The cancel token
  (default `Key:Escape`) cancels, so it cannot be bound from the screen. Tokens
  not in `binding_registry()` are refused.
- **Conflicts.** The screen lists tokens used by two actions on both rows
  (`Conflict: E also used by use`) and in the status line. `RemapConflicts::Warn`
  (default) applies the binding and shows the conflict; `Reject` refuses it and
  names the owner. Binding a token the action already has is refused.
- **Events.** `RemapEvent` values are returned and also emitted as engine
  events `input.rebound` (action, previous, token, bindings), `input.rebind_conflict`,
  `input.rebind_reset` and `input.rebind_rejected`, with the controller's entity as
  source and target, so `EventBindings` handlers and scripts can persist the
  result.
- **Raw tokens.** Capture needs the key and button tokens, not mapped actions.
  A host publishes `RawInputTokens { down }` each tick; the desktop runner does
  (one line in `DesktopRunner::tick`). The press that opened the capture is
  swallowed, and a tap shorter than one simulation tick can be missed.
- **Accessibility.** Every token button has the name `<Action>, binding <n>: <key>.
  Activate to change` (or `waiting for a key or button` while capturing), the root
  is a `region`, the title a `heading`, the status line a polite live `status`.
  All controls are ordinary buttons, so `ui_next`/`ui_previous`, directions and
  `ui_confirm` (keyboard or gamepad) reach them, and the scroll view follows focus.
- Limits: 48 actions, 16 bindings per action, action names of 48 bytes; a larger
  screen is refused with `remap.invalid` rather than overflowing the 1024-node root.
  Axis (stick) capture is not supported, and there is no per-binding remove
  button (reset restores the defaults).

## Limits

- `AccessTree` size is bounded by the UI: at most 1024 nodes per root, 64 roots and
  16384 laid-out boxes, plus synthetic nodes (at most 32 tabs, 4 dialog buttons,
  64 options per widget).
- Names composed from descendants are at most 1024 bytes.
- At most 64 AT requests per tick and 64 retained outcomes; 128 platform requests
  are held between ticks.
- The tree for the focused text input exposes its value but **not the caret or
  selection**, and no text runs, so a screen reader reads the field value as one
  string and does not announce per-character caret movement. Setting the value
  works. This needs text-run nodes with character metrics from the text backend.
- Changes made by a request show in the tree one tick later (the tree is built
  from the layout the tick started with).
- Only whole-window UI is exposed: world-space panels and world text have no
  tree.
- `SetValue` on a numeric value must be inside the range; out-of-range values are
  refused, not clamped.
- `uiScale` text sharpness: see [UI scale](#ui-scale).

## Captions and subtitles

Captions are timed text with speakers. They have three parts: the `caption_track`
asset, the runtime `Captions` controller with the authored `engine.CaptionPlayer`
component, and a generated caption UI. Code is in `engine_project::captions`.

### The `caption_track` asset

A JSON file registered with kind `caption_track` (editable with the generic asset and file
tools; `asset_info` reports cue and speaker counts and the duration):

```json
{ "version": 1,
  "speakers": [ {"id": "ana", "name": "Ana", "color": [1.0, 0.8, 0.2, 1.0]},
                {"id": "bo",  "name": "@speaker-bo"} ],
  "cues": [
    {"id": "a", "start": 0.0, "end": 2.0, "speaker": "ana", "text": "Hello [b]there[/b]"},
    {"id": "b", "start": 1.0, "end": 3.0, "speaker": "bo",  "text": "@greeting", "priority": 2},
    {"id": "c", "start": 1.5, "end": 2.5, "text": "door creaks", "category": "sound_effect"} ] }
```

| Field | Rule |
| --- | --- |
| `version` | `1`. |
| `speakers[].id` | 1 to 64 characters of `A-Za-z0-9_-.`, unique. At most 64 speakers. |
| `speakers[].name` | 1 to 128 bytes; a message reference (`@key`) is localized. |
| `speakers[].color` | RGBA 0 to 1, default white; colours the name. |
| `cues[].id` | Same rule, unique. At most 20000 cues. |
| `cues[].start`, `end` | Seconds within 0 to 1000000, `end` after `start`. A cue is visible while `start <= t < end` (start inclusive, end exclusive). |
| `cues[].speaker` | Optional; must name a speaker of the track. |
| `cues[].text` | 1 to 1024 bytes; [rich-text markup](text.md), or a message reference (`@key`, `@@` escapes an `@`). Markup is validated at load unless the text is a reference. |
| `cues[].priority` | Integer, default 0. |
| `cues[].category` | `dialogue` (default), `sound_effect` or `music`. |

Overlapping cues are allowed. Files are at most 4 MiB. Errors have stable codes:
`captions.track.parse`, `.version`, `.bounds`, `captions.file.too_large`,
`captions.speaker.id`, `.name`, `.color`, `.unknown`, `captions.cue.id`, `.time`, `.range`,
`.text`, `.markup`, `.priority`. Project loading (`Project::load`) validates every
`caption_track` asset and reports the file.

**Import.** `captions::import::import_captions(text)` (and `import_captions_file(path)`)
converts WebVTT (starts with `WEBVTT`) or SRT into the same model: timings
(`hh:mm:ss.mmm`, `mm:ss.mmm`, SRT `hh:mm:ss,mmm`), identifiers, multi-line text,
`<b>`, `<i>`, `<u>` (to `[b]`, `[i]`, `[u]`), HTML entities `&amp; &lt; &gt; &nbsp;`.
Unbalanced style tags are repaired rather than failing the import: an unclosed `<i>` is
closed at the end of the cue, a stray closing tag is dropped and a closing tag that skips
over open inner tags closes them first (nesting is kept to 16 levels).
**Voice tags** `<v Speaker>text</v>` create a speaker (ID from the name, colours cycling
through a six-colour palette) and set the cue's speaker; WebVTT classes are ignored, so
`<v.loud Bob>` names the speaker `Bob`. A cue whose whole text is in square
brackets (`[door creaks]`) becomes a `sound_effect` cue without the brackets. Other tags,
cue settings after the end time, `NOTE`, `STYLE` and `REGION` blocks are dropped. The result
is validated; bad timings give `captions.import.timing`. The importer is a library function;
there is no dedicated MCP tool, write the result with the file tools.

### Runtime controller

`Captions` is a resource (`world.resource_mut::<Captions>()`), loaded with every
`caption_track` asset of the project under its asset ID.

```rust,ignore
let mut captions = world.resource_mut::<Captions>();
let id = captions.play("intro", PlayOptions { offset: 4.0, looping: false, clock: None })?;
captions.seek(id, 12.5)?;      // takes effect on the next advance
captions.stop(id);
captions.push(AdHocCue::new("Look out!", 2.0).spoken(guard_speaker))?; // expires after 2 s
captions.clear();               // stops everything and drops ad-hoc cues
captions.insert_track("custom", track)?;   // a track built in code
```

| Call | Result |
| --- | --- |
| `play(track, options)` | `PlaybackId`; `captions.track.unknown`, `captions.playback.limit` (32 at once) |
| `seek(id, seconds)` | `captions.playback.unknown`, `captions.playback.offset` |
| `push(cue)` | ad-hoc ID; duration within (0, 3600] seconds, at most 64 at once |
| `active()` | cues visible now, oldest first (`ActiveCue`: speaker resolved, authored text) |
| `advance(dt)` | moves clocks, returns `CaptionEvent`s; the runtime calls it every tick |

A finished non-looping playback is removed. `captions.cue_started` and
`captions.cue_ended` are emitted as engine events (payload `cue`, `playback`, `speaker`,
`text`, `category`) for every cue that appears or disappears, including cues hidden by the
display preferences; handlers see them one tick later like every engine event.

#### Time source

By default a playback's position advances with the runtime tick time (`dt`). `PlayOptions.clock`
takes a `CaptionClock` (`Arc<dyn Fn() -> Option<f64> + Send + Sync>`) returning the position
in seconds; it replaces tick time for that playback (`None` holds the last position; such a
playback stays until stopped). This is the hook for audio: `engine-audio` exposes no playback
position yet (a streaming source with a playback clock is a pending cross-domain request), so
today the clock must come from your own code. `advance(0.0)` re-evaluates without moving time.

### `engine.CaptionPlayer`

Plays a track while its entity exists. Editable with `component_set`.

| Field | Range, default | Effect |
| --- | --- | --- |
| `track` | asset ID, empty | The `caption_track` to play; empty plays nothing. An unknown track is recorded in `Captions::errors()`. |
| `autoplay` | true | Start when the component appears or changes. With false, start tracks from code. |
| `loop` | false | Restart at 0 after the last cue. |
| `offset` | 0 to 1000000 s, 0 | Starting position. |
| `font` | font key, empty | Font of the caption text (a `font_face` family or bitmap font). |
| `bottomMargin` | 0 to 2000 px, 48 | Safe-area margin below the captions (UI pixels). |
| `maxWidth` | 0.2 to 1, 0.8 | Widest caption as a fraction of the viewport; longer text wraps. |

The lowest entity carrying the component supplies `font`, `bottomMargin` and `maxWidth`.
`Captions.style` holds the same values (plus text and background colour) for native code.

### Presentation

Each tick, before the UI tick, the runtime builds a `UiRoot` with the authored ID
`captions` and writes it to a generated entity (not part of the scene). It is an ordinary
root: themes and [stylesheets](ui-styling.md) can target `captions` and its `captions.cue.N`
labels, and it is drawn by the same extraction as all UI, so `uiScale` applies.

- **Layer.** `CAPTION_LAYER` (1000000): above ordinary UI roots, below the modal-dialog
  layer (`UI_DIALOG_LAYER`) and popup layer (`UI_POPUP_LAYER`), so dialogs and dropdowns draw
  over captions.
- **Layout.** A transparent full-screen column, cues centred and stacked bottom up with
  `bottomMargin` below, one rich label per cue with padding 8 and a box of
  `captionBackgroundOpacity` in the vertex colour alpha (black by default), at most
  `maxWidth` of the viewport wide.
- **Order.** Cues appear oldest first (top) to newest (bottom). When more cues are active
  than `captionMaxLines`, the highest `priority` stay, and among equal priorities the most
  recently started.
- **Text.** Dialogue is `[color=#rrggbb]Name:[/color] text`, the name in the speaker's
  colour (when `captionSpeakerNames` is on). `sound_effect` and `music` cues are shown as
  `[text]`, with the brackets escaped, unless `captionSoundEffects` is off.
- **Localization.** Cue text and speaker names that are `@key` references resolve through
  `Localization` each tick, so a language switch re-resolves them on the next tick; an
  invalid translation is shown literally.
- **Bitmap fonts.** The bracket markup needs the `engine-text` backend (`font_face` assets).
  A project without any renders cue labels as plain text (no speaker colour, no emphasis).

Caption settings are fields of `engine.AccessibilitySettings` (and of
`AccessibilityControl::settings`):

| Field | Range, default | Effect |
| --- | --- | --- |
| `captionsEnabled` | true | Off removes the caption root; playback and events continue. |
| `captionTextScale` | 0.5 to 4, 1 | Caption `fontScale`, multiplied by `textScale` and the minimum text size rule. |
| `captionBackgroundOpacity` | 0 to 1, 0.6 | Alpha of the box behind each cue. |
| `captionSpeakerNames` | true | Show the speaker prefix. |
| `captionMaxLines` | 1 to 8, 3 | Most cues shown at once (each cue may wrap onto several lines). |
| `captionSoundEffects` | true | Show `sound_effect` and `music` cues. |

### Screen readers

The caption root's panel is a `status` node named "Captions" with a **polite live region**
(`accessibility.live`). Each visible cue is a child label whose accessible name is the plain
text, speaker prefix included, with the markup removed. When the cue set changes the
platform adapter announces the change. The root entity is created when the first cue appears
(a project that never shows a caption has no extra entity) and then stays. Verified in the neutral `AccessTree`
(`the_live_region_carries_the_active_cue_text_in_the_access_tree`); how a screen reader
speaks it is not verified (see [Platform status](#platform-status)). The region is absent
when `captionsEnabled` is off, and removed while a modal dialog is open (see
[Visibility rules](#visibility-rules)).

## Deferred

- **Captions from an audio playback position.** `engine-audio` has no
  playback-position clock yet; use a `CaptionClock` (see [Time source](#time-source)).
- **Colour-vision filter on the 2D sprite/UI layer and in 2D-only games.** The filter
  needs a stage that reads the composited image; the Overlay stage cannot (see
  [ui-render.md](ui-render.md#not-implemented)).
- Reading operating-system preferences (reduced motion, high contrast, text
  size, "screen reader running") and applying them to the settings.
- Caret, selection and text-run exposure (above), and tree support for world-space
  UI panels.
- Heading levels, form relationships (labelled-by), and relations between a
  slider and an adjacent label: author them with `accessibility.label` for now.
- Verification with NVDA, Narrator, JAWS, VoiceOver and Orca; macOS and Linux
  builds.

## Decision record

- **Neutral tree first, AccessKit second.** The tree is built from the UI
  layout in `engine-project` and has no platform dependency, so it is testable
  without a window and reusable by hosts other than the desktop runner (the
  editor, a test harness). The alternative, building AccessKit nodes inside
  `GameUi`, would have made `engine-project` depend on a platform crate and the
  conversion untestable without it.
- **`accesskit_winit` 0.34.1 with `accesskit` 0.25.1.** This adapter version
  depends on `accesskit ^0.25.1`, which is exactly the version already in
  `Cargo.lock` (via egui), and on `winit ^0.30.5`, which matches the pinned
  `winit 0.30.13`; no second `accesskit` is built. Older adapters (0.29 to 0.33)
  would have added a second `accesskit`. Both are exact-pinned in
  `crates/engine-desktop/Cargo.toml`. Licences: `accesskit` Apache-2.0 OR MIT,
  `accesskit_winit` Apache-2.0; their platform crates are permissive. It is
  always on and pure Rust: Windows uses UI Automation through the `windows` crate,
  macOS `objc2`, Linux AT-SPI over `zbus` (async-io, no system libraries or C
  compiler). The lockfile gained 44 crates, none of which replaced an existing
  version. Making it optional would have left the default desktop build without
  accessibility; the cost when no AT is connected is one atomic load per tick.
- **Direct handlers, not an event-loop proxy.** `with_event_loop_proxy` requires
  the runner's event loop to carry a user event type. Direct handlers touch the
  runner in two small places and keep the initial tree lazy.
- **Scale as a smaller layout viewport plus geometric scaling.** Scaling
  every authored length would have needed to convert scroll offsets and
  item extents and would have made authored data depend on the setting. The
  viewport approach reuses layout, hit testing and drawing unchanged and keeps
  `UiRoot` values independent of the user's scale.
- **Component plus a separate resource.** `bevy_ecs` 0.20 stores resources as
  components, so one type cannot be both a `Component` and a `Resource` without
  the resource entity matching component queries. The resource wraps the settings.
- **Scrolled-out focus candidates.** Reaching items in a scroll view by
  keyboard is a WCAG 2.1 requirement (2.1.1, 2.4.7) and the remap screen is a long
  list, so focusable widgets scrolled out of a scroll view now take focus and are
  revealed, instead of being skipped.
