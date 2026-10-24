# Scene editor

`engine_editor::run(project_manifest, play_host)` opens the portable egui/eframe
shell backed by the existing revision-aware `EditorController`. The hierarchy,
transform inspector, validated component JSON editor, asset list and console share
the authored scene model used by MCP. The embedded viewport uses the engine GPU
renderer for 2D and 3D previews; camera navigation and translation gestures do not
run gameplay callbacks. Play remains an isolated Cargo-built process.

See [Portable editor](../../docs/portable-editor.md) for controls, supported
contracts and performance limits. The legacy Windows-only controls remain available
through `native_smoke` for structural regression coverage.
## Compiler and play ownership

Tooling supplies the implementation of:

```rust,ignore
trait PlayHost {
    fn start(&mut self, project: engine_project::Project) -> engine_core::Result<()>;
    fn stop(&mut self) -> engine_core::Result<()>;
    fn poll(&mut self) -> engine_core::Result<engine_editor::PlayStatus>;
}
```

`PlayStatus` is `Compiling`, `Running`, or
`Stopped { success: bool, message: String }`. Start should queue compilation in
a worker and return promptly; poll must be nonblocking so the native message loop
remains responsive. The host owns candidate builds, cancellation and process
cleanup. Stop must release that generation before returning success. An error
keeps play active so the user can retry stopping. The controller attempts best
effort stop on Drop, but explicit close/stop is needed to report errors.

Start receives `SceneDocument::begin_play`'s isolated Project clone, including
unsaved scene edits and the original asset root. The host must launch that clone,
not reload the saved scene or temporarily overwrite authored files. Runtime
mutations never replace the editable document. Failed compile resets the document
play state, and terminal runtime failures appear in the log pane.

`EditorController::apply(operation, expected_revision)` exposes the same stale
revision protection as `SceneDocument`. `open`/`reload` require an explicit
discard flag when dirty. `select`, `save`, `undo`, `redo`, `play`, `stop`, `poll`
and `close` are independently testable without a native window.

## Verification

```text
cargo test -p engine-editor
cargo clippy -p engine-editor --all-targets -- -D warnings
cargo test -p engine-editor real_native_controls_smoke_closes_without_saving -- --ignored --nocapture
```

The explicit Windows smoke creates real native controls, selects an entity,
applies a position through the WM_COMMAND path, undoes it, selects typed assets,
checks nonempty child geometry, and closes automatically. It verifies the scene
file did not change. This is structural/control-path evidence, not subjective
visual review or physical input testing. The default test suite skips the native
smoke because headless workers may not have an interactive desktop.

Compilation and launch of a real project generation is tooling integration
acceptance; the editor model tests use a controlled PlayHost to verify ownership
and failure paths without claiming compiler/backend coverage.
