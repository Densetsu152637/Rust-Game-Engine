# Native editor

`engine-editor` provides a native Windows authoring window and a platform-independent
`EditorController`. It depends on the project model and `engine-core`, not on
tooling or a second rendering backend. Win32 controls use `windows-sys`, and the
required FFI is isolated in `native.rs`; the controller contains no unsafe code. On
other targets the window entry point returns an explicit unsupported-platform error,
so the crate still builds and its controller tests run on any platform.

Launch it with `engine editor <manifest>` (see [Project commands](commands.md)) or
from Rust with `engine_editor::run(project_manifest, play_host)`.

## What it edits

The window offers a manifest path and open action, save and reload, undo and redo,
the entity hierarchy with selection, a position inspector, Rust-script, mesh and
texture asset pickers, a validation and log pane, and play/stop. Picker values are
stable asset IDs and the project model validates kinds and revisions. Position
values are committed with **Apply position**. Replacing, reloading or closing with
unsaved edits shows Save/Discard/Cancel, and a failed load keeps the current
document. There is no embedded game viewport: play launches the compiled runtime
through the tooling host, matching the earlier editor's separate-runtime boundary.

Edits go through `SceneDocument` in `engine-project`: every operation carries the
expected revision and stale edits are rejected, history supports undo and redo, saves
are atomic, and no arbitrary native ECS state is written into the authored scene.

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
