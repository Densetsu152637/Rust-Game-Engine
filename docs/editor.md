# Scene editor

`engine-editor` opens a portable egui/eframe authoring window around the
platform-independent `EditorController`. Launch it with `engine editor <manifest>`
or `engine_editor::run(project_manifest, play_host)`.

The window provides save, undo/redo, hierarchy selection, transform controls,
validated entity/component JSON editing, an asset list, diagnostics, play/stop,
and a real GPU scene preview with 2D/3D navigation and translation gestures.
See [Portable editor](portable-editor.md) for controls and current limits.
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
    fn stop(&mut self) -> engine_core::Result<()>;
    fn poll(&mut self) -> engine_core::Result<engine_editor::PlayStatus>;
}
```

`PlayStatus` is `Compiling`, `Running` or `Stopped { success, message }`. `start`
queues compilation on a worker and returns promptly, and `poll` must not block so the
native message loop stays responsive. The host owns candidate builds, cancellation
and process cleanup, and `stop` must release the generation before reporting success;
an error keeps play active so the user can retry. `start` receives the isolated
`Project` clone from `SceneDocument::begin_play`, including unsaved scene edits and
the original asset root; the host must launch that clone, not the saved files.
Runtime mutations never replace the editable document, and failed compilation resets
the document's play state with diagnostics in the log pane. The tooling
implementation (`NativePlayHost`) runs the game with a scene snapshot override and a
private user-data directory so editor play never touches player saves. The
`engine-tooling` `runtime_pipeline` test plays an unsaved edit headlessly through
`NativePlayHost` and checks that the authored scene file is unchanged; the private
user-data location is read from the play host source.

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
