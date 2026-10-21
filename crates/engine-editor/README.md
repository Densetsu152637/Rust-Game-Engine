# Native editor

`engine-editor` provides the Windows authoring window and a platform-independent
`EditorController`. It depends on the project model and core, not tooling or a
second rendering backend. Win32 controls use `windows-sys`; required FFI is
isolated in `native.rs`. The controller contains no unsafe code.

```rust,ignore
engine_editor::run(project_manifest, Box::new(compiler_backed_play_host))?;
```

The UI provides a manifest path/open action, save/reload, undo/redo, entity
hierarchy and selection, a position inspector, Rust-script/mesh/texture asset
pickers, validation/log output, and play/stop. Picker values are stable asset IDs;
the project model validates kinds and revisions. Unsaved replacement, reload and
close show Save/Discard/Cancel. Failed loading retains the current document.

Position values are committed with **Apply position**. A minimum window size
keeps the native controls usable. This editor has no embedded game viewport;
play opens the compiled runtime through the tooling host, matching the previous
editor's separate-runtime boundary. Windows is the only native UI platform;
other targets return an explicit unsupported-platform error.

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
