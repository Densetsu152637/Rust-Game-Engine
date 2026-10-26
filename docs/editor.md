# Scene editor

`engine-editor` opens a portable egui/eframe authoring window around the
platform-independent `EditorController`. Launch it with `engine editor <manifest>`
or `engine_editor::run(project_manifest, play_host)`.

The window provides save, undo/redo, hierarchy selection, transform controls,
validated entity/component JSON editing, an asset list, diagnostics, play/stop and game reload,
and a real GPU scene preview with 2D/3D navigation, multi-object transform
gestures, geometry picking, snapping, grid and camera bookmarks.
See [Scene view editing](editor-scene.md) for transactional gestures and settings.
See [Portable editor](portable-editor.md) for controls and current limits.
See [Native hot reload](editor-hot-reload.md) for replacement and failure behavior.
The legacy Windows controls remain available through `native_smoke` for existing
structural regression tests; they are not the default authoring window.

All changes use the same revision-checked `SceneDocument` model as MCP. Preview
state never overwrites authored data. Play still runs an isolated Cargo-built
process with an unsaved project snapshot. Dirty close offers Save/Discard/Cancel.
## Play host

Tooling supplies the `PlayHost` implementation:

```rust,ignore
trait PlayHost {
    fn start(&mut self, project: engine_project::Project) -> engine_core::Result<()>;
    fn reload(&mut self, project: engine_project::Project) -> engine_core::Result<()>;
    fn cancel_reload(&mut self) -> engine_core::Result<()>;
    fn stop(&mut self) -> engine_core::Result<()>;
    fn poll(&mut self) -> engine_core::Result<engine_editor::PlayStatus>;
}
```

`PlayStatus` is `Compiling`, `Running`, `Reloading`,
`ReloadFailed { message }` or `Stopped { success, message }`. `start` and `reload`
queue compilation on a worker and return promptly. `poll` observes progress and
activates a prepared replacement only after successful previous-game teardown;
this activation can wait up to the host's graceful shutdown deadline. The host
owns candidate builds, cancellation and process cleanup. `stop` releases both
pending and active generations; errors remain visible so cleanup can be retried.

Play receives the isolated `Project` clone from `SceneDocument::begin_play`,
including unsaved edits and the original asset root. Reload Game receives a fresh
clone of the current authored scene without ending the document's play session.
The toolbar enables Reload Game once play is running. While replacement compiles
and reaches the startup barrier, the previous game continues. Cancel Reload
discards the candidate. Rejected compilation/readiness retains a still-running
previous game, reports diagnostics, and enables another reload attempt. Failed
teardown or candidate activation ends play with an error. Runtime state restarts
from the new authored snapshot; it is not transferred between processes.

Project-file Reload (`EditorController::reload`, also used by the legacy Windows
controls) still reloads disk content, protects unsaved edits, and stops play. It
is separate from Reload Game. Runtime mutations never replace the editable
document. The tooling host uses generation-local scene snapshots and private
user-data directories, keeping authored files and ordinary player saves separate.
The `engine-tooling` `editor_hot_reload` acceptance test observes real native
lifecycle events, unsaved replacement positions, failed compilation preserving the
old game, and separate private save directories. See
[Native hot reload](editor-hot-reload.md) for the exact replacement boundary.

## Verification

```text
cargo test -p engine-editor
cargo clippy -p engine-editor --all-targets -- -D warnings
cargo test -p engine-editor real_native_controls_smoke_closes_without_saving -- --ignored --nocapture
```

The default tests exercise the controller with a controlled `PlayHost` (typed edits,
history, stale revisions, unsaved guards, play ownership and failure paths). The
ignored Windows smoke test creates real native controls, selects an entity, applies a
position through the `WM_COMMAND` path, undoes it, selects typed assets, checks child
geometry and closes automatically, verifying the scene file is unchanged. It is
structural evidence, not visual review or physical input testing, and needs an
interactive desktop.

## Graph authoring

Project graph assets have an **Open graph <asset id>** action in Assets. The Graph
view uses the common `engine-graph` model and registered domain schemas for
behavior trees, state trees and animation. The native canvas provides node/port
editing, validated properties, history, diagnostics and revision-aware persistence.
Scene/Graph selectors retain independent documents; Save/Undo/Redo target the
active view. Dirty close protects both, and incomplete drafts export separately
without replacing the active asset. Play/reload reads saved graph asset data.
See [Native graph editor](editor-graph.md) for interaction and persistence contracts.
