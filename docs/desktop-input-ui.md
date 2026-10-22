# Desktop action input and gameplay UI

Input mapping and the modal UI model are CPU-only types in `engine-project`
(`input` and `ui` modules); the desktop host in `engine-desktop` supplies physical
samples. Everything here is deterministic and testable without a window.

## Action mapping

`ActionMapper` converts one `RawInput` sample per simulation tick into an
`ActionFrame` (`InputSnapshot` is an alias). `RawInput.down` contains exact registry
tokens (not OS key-repeat events), `axes` carries `LeftX/LeftY/RightX/RightY` in
`[-1,1]` and `LeftTrigger/RightTrigger` in `[0,1]`, plus pointer, wheel, focus and
gamepad-connected state. `ActionFrame` exposes `pressed(action)`, `held(action)`,
`released(action)` and `value(action)` plus the logical pointer, wheel and focus.

- **Tokens.** `binding_registry()` lists every valid token: `Key:A`..`Key:Z`,
  `Key:0`..`Key:9`, named keys (`Key:Space`, `Key:Enter`, `Key:Escape`, arrows,
  modifiers, punctuation), `Key:F1`..`Key:F25`, `Key:Keypad0`..`Keypad9` and keypad
  operators, `Mouse:Left|Right|Middle|4..8`, `Mouse:WheelUp|WheelDown|WheelLeft|
  WheelRight`, `Gamepad:A|B|X|Y`, bumpers, `Back|Start|Guide`, thumbs, `DpadUp|
  DpadRight|DpadDown|DpadLeft`, `Gamepad:LeftTrigger|RightTrigger`, and stick
  directions such as `Gamepad:LeftXPositive` and `Gamepad:LeftYNegative`. Tokens are
  independent of winit and gilrs integer codes; the desktop host translates devices
  to tokens (`engine_desktop::key_token`).
- **Edges.** An action's value is the maximum over its bindings. Press is a
  zero-to-positive transition, release is positive-to-zero, and `held` is also set
  on the press tick. Wheel input is a one-tick pulse. Axis values above the
  deadzone (default 0.2, settable in `[0,1)`) are rescaled to `[0,1]`. NaN and
  infinity are discarded.
- **Focus and devices.** Losing focus releases held actions and quarantines them
  until a neutral sample after focus returns. A newly connected gamepad's held
  controls are quarantined until neutral without suppressing a keyboard binding of
  the same action. Neither key repeat nor reconnection synthesizes a press.
- **Rebinding.** `BindingMap` is a `BTreeMap<String, Vec<String>>`, suitable for
  host JSON settings. `validate_bindings(map, strict)` accepts up to 256 actions
  and 1-16 unique tokens each, and reports sharing a token between actions as
  advisory conflicts (errors when `strict`). `rebind` is atomic and quarantines
  currently held actions until neutral. Action names are non-empty opaque strings.
- **Injection.** `merge_input(physical, injected, previous)` combines physical input
  with injected actions (CLI `--input action:tick` or tests). Injected actions are
  one-tick pulses; physical holds persist until released.

Authored bindings come from the manifest's `inputActions` and `inputBindings` (see
[Project format](project-format.md)); `runtime::InputSettings::rebind` increments a
revision so the host can synchronize changes before the next sample.

## Gameplay UI model

`UiModel` owns an ordered stack of panels and emits `confirm`, `cancel` and
`selection` events (`UiEvent { panel, kind, selection }`, polled with
`poll_event`, at most 256 queued). A `PanelOptions` carries the panel ID, font asset
ID, text, logical-pixel `Rect`, colour, scale, line height, glyph advance, layer,
`modal`, `dismissible`, an optional `choices` list and the action names used for
navigation. Defaults are `ui_confirm`, `ui_back`, `ui_up`, `ui_down` and
`ui_click`. Controller support follows from mapping those actions.

- Only the top panel receives navigation; closing it restores the parent's focus.
  Pointer coordinates must use the same logical space as panel rectangles, and the
  wheel scrolls only inside the focused panel.
- Choice rows follow the text lines, one row per choice (CR/LF rejected). Up/down
  wraps the selection and scrolls the selected row into view; a panel without
  choices scrolls with up/down instead. Opening a panel and manual scrolling keep
  the reading position until a choice navigation action.
- Text must be valid Unicode. Layout wraps at scalar boundaries (including long
  unbroken strings) and honors newlines; `GlyphAdvance` callbacks supply font-atlas
  metrics per font asset, otherwise a configurable monospaced advance is used.
  Snapshots expose `lines`, `content_height`, `clip` and `scroll_offset`; the
  renderer must clip to `clip` and show `focused` and `selected` visibly.
- Limits: 64 panels, 64 choices per panel (each at most 4096 bytes), fewer than
  4096 laid-out lines, 64 KiB of text and 16,384 logical pixels per rectangle
  dimension. Over-limit text is rejected, not truncated, and invalid text or
  geometry is rejected atomically.

### Modal input capture

Feed the original action frame into `tick`, then read `gameplay_frame()` for world
input. While a modal panel is open it consumes all gameplay controls. Opening,
closing or clearing a panel also masks the current frame immediately, including
opens during a script callback; controls held across the transition stay suppressed
through their release frame and are rearmed by an explicit release or a neutral
frame. This keeps a dismissal from interacting with the object beneath or reopening
the same overlay. `clear()` removes panels and queued events (scene change or fault
teardown) while retaining the transition quarantine. After UI or control-lock
changes inside one callback use `runtime::gameplay_input(world)`; scheduled systems
should order UI writers, `refresh_gameplay_input` and gameplay readers explicitly.

The model stores only the visible text it is given; hiding locked narrative content
is the authoring layer's job. It selects no visual design, font, language or
resolution.

## Tests

Behavior is exercised by `crates/engine-project/tests/parity.rs`
(`actions_focus_and_rebind_quarantine_held_keys`,
`gamepad_deadzone_disconnect_reconnect_suppresses_stuck_controls`,
`modal_capture_consumes_release_after_closing`,
`ui_choice_navigation_scrolls_and_emits_events`,
`custom_glyph_metrics_survive_text_updates_and_resize`) and by the
`examples/desktop-2d` gameplay tests (modal movement blocking, control locks,
rebinding persistence). Physical keyboard, mouse and gamepad interaction needs
manual checks; see [Testing and CI](testing.md).
