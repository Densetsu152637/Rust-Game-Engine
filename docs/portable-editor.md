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

The scene viewport renders through `OffscreenSprites::render_scene`, including
actual 3D materials and custom WGSL. It reads the GPU result into an egui texture
after edits or camera motion. Its initial resolution is 640×360; this transfer
path prioritizes compatibility and is not a low-latency GPU texture bridge.
Preview construction does not start or tick gameplay callbacks.

Choose 2D or 3D in the toolbar. Right drag orbits the 3D camera, middle drag pans,
and the wheel zooms. Click near an entity origin to select it; left drag moves
the selected entity in the camera plane with a transient visual preview. Each viewport drag commits one edit on
release, so undo restores the previous transform. A stale revision rejects the
gesture. Selection origins show colored translation axes. Frame selection moves
the view center to the selected entity. Picking is origin-proximity selection,
not a pixel/object-ID picking buffer.

Invalid preview data leaves the last successful image visible with diagnostics.
Closing a dirty document prompts Save, Discard or Cancel. Changes are validated
through the document model before they reach disk.
