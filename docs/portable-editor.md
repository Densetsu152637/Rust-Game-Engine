# Portable scene editor

`engine editor <manifest>` opens an egui/eframe desktop shell using the existing
revision-aware `EditorController` and Cargo-backed `PlayHost`. The UI targets
Windows, Linux and macOS; build and interactive verification on each platform
must be reported separately. The legacy Win32 smoke entry point remains for its
existing control tests.

The hierarchy and inspector edit the same `SceneDocument` model used by MCP.
Position, quaternion rotation and scale have numeric controls. The validated
entity JSON editor exposes all authored components, including custom registered
component data. Asset IDs and diagnostics are visible beside the viewport.
Save, undo and redo retain document revision checks. Play compiles and launches
an isolated project snapshot, preserving the existing external-process boundary.
Once play is running, **Reload game** compiles current Rust sources and prepares
the latest authored scene, including unsaved edits, while the old game continues.
**Cancel reload** discards a pending candidate. Successful replacement shuts down
the old game and restarts runtime state from the new snapshot. Compilation or
readiness errors keep a still-running old game active and appear in the console;
failed teardown or activation ends play. This action does not reload scene files
from disk or transfer live runtime state. See [Native hot reload](editor-hot-reload.md)
for failure guarantees and private save isolation.

The scene viewport renders through `OffscreenSprites::render_scene`, including
actual 3D materials and custom WGSL. It reads the GPU result into an egui texture
after edits or camera motion. Its initial resolution is 640×360; this transfer
path prioritizes compatibility and is not a low-latency GPU texture bridge.
Preview construction does not start or tick gameplay callbacks.

Choose 2D or 3D in the toolbar. Right drag orbits the 3D camera, middle drag pans,
and the wheel zooms. Click rendered geometry to select; Ctrl/Command toggles,
Shift adds, and empty-space drags select visible pixels in a rectangle. The
hierarchy also supports multi-selection, including non-rendered entities.
W/E/R choose move, rotate and scale handles. Each completed viewport gesture is
one revision-checked undo transaction; Escape and focus loss cancel previews.
Grid, snapping, local/world axes, selection pivots and camera bookmarks are
configurable and saved separately in `editor.json`.
See [Scene view editing](editor-scene.md) for controls, picking semantics,
bounded CPU raster limits, transform limitations and verification coverage.

Invalid preview data leaves the last successful image visible with diagnostics.
Closing a dirty document prompts Save, Discard or Cancel. Changes are validated
through the document model before they reach disk.
