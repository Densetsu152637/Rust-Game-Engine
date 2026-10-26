# Native graph editor

`engine-editor-graph` provides a reusable egui 0.37 canvas for the shared
`engine-graph::GraphDocument`. It does not define behavior tree, state tree or
animation document variants. A host supplies a `GraphKind`; palette entries,
ports, parameter declarations, defaults, limits and compiler diagnostics come
from that kind. The editor shell obtains shipped kinds from
`engine_project::graphs::kinds()`.

## Session and UI contract

Create `GraphSession::new(document, kind.clone())` and retain one
`GraphCanvas::default()` per open graph tab. Call `canvas.show(ui, &mut session)`
inside the host's egui panel. The host owns tab/file selection, project catalog
registration, save actions and unsaved-tab prompts. The canvas owns transient
selection, pan and gesture previews. Scene documents, play state and hot reload
remain separate host responsibilities.

The palette adds registry nodes with their declared ports and parameter defaults.
Drag a node header to move it. Drag an output circle to an input circle to wire
nodes; clicking an output then an input also connects. Port labels display the
schema's actual type. Select a header and use Delete or **Delete node** to remove
it and its attached edges. The properties section lists edges with **Disconnect**
actions. Middle-button dragging pans; **Reset view** resets pan.

Parameters use editable JSON values with an explicit Apply action. Strings need
quotes; null removes an override and restores the declared default. The schema's
type, bounds, enum choices and variable references remain enforced by the shared
validator. Variables use their shared JSON representation and can be added,
replaced or removed; metadata accepts a key and JSON value. Existing properties
are retained unless explicitly edited. This first version uses JSON fields rather
than specialized controls for every parameter type.

Every accepted UI edit passes through `engine_graph::apply_edits` at Structure
strictness. The session keeps up to 64 document snapshots for undo, with redo
cleared by a new edit. A header drag previews locally and submits one position edit
on release, so one Undo restores the entire gesture. Displacement is measured
from the original pointer press, including release-frame motion; stationary frames
retain the preview. Escape cancels a preview.
Rejected batches retain the exact prior document and history. Undo/Redo buttons
and command-Z / command-shift-Z operate on the graph tab; text fields retain their
own keyboard editing behavior. Read-only sessions disable authoring and history.

Complete diagnostics include generic rules and the kind's compiler callback. They
remain visible for incomplete intermediate documents. Invalid imported documents
can be inspected and repaired according to the shared edit validator's baseline
rules; the editor does not weaken those rules.

## Project files

`GraphFile::open(root, relative, kind)` returns a file handle and session.
`GraphFile::create` targets a new file in an existing project directory.
Paths must be project-relative `.json` files without parent traversal; canonical
parents and existing files must remain inside the canonical project root. Opening
reads at most the shared 4 MiB limit before parsing. Opening an invalid JSON file
returns an error; it does not discard or repair bytes automatically.

`file.save(&mut session)` requires Complete validation. Saving serializes the same
shared document, preserving positions, edge order, variables and metadata. It
compares the exact bounded disk bytes with the opened/saved revision both before
preparing the temporary file and immediately before replacement. External changes
cause `FileError::Conflict` and leave the session dirty. Existing file permissions
are retained; read-only files are rejected. The temporary file is created in the
same directory, written and synchronized before atomic publication. New targets
use no-clobber publication. A failed save does not advance the saved baseline.

As with the MCP graph tool, the final recheck narrows the race window; it is not a
cross-process compare-and-swap or a filesystem sandbox against a hostile process
replacing directories concurrently. Missing parent directories are not created.

`file.export_draft(separate_relative_path, &session)` exports incomplete work to a
new, separate file, refuses existing destinations, and leaves the active file and
session's saved baseline untouched. The host must not register this incomplete
draft as an active graph asset. Normal Save never writes an incomplete active
asset. The host may reopen a draft explicitly to continue editing.

## Editor shell

Launch `engine editor <manifest>`. In **Assets**, click **Open graph <asset id>**
for a project graph asset. The shell uses `GraphFile::open_registered` for bounded
parsing and builtin kind lookup, then opens the **Graph** view. The **Scene** and
**Graph** toolbar selectors retain both documents independently. Save, Undo and
Redo target the selected view. The graph canvas also exposes its own history
buttons and full diagnostics. Scene preview lighting and play/reload controls keep
their existing behavior; unsaved graph changes are not part of play snapshots.
Save the graph asset before playing or reloading to use its latest document.

Only one graph tab is retained. Opening another graph refuses to discard modified
work. **Close graph** offers save, explicit discard, or continued editing. Closing
the application includes dirty graph work in its existing save/discard/cancel
prompt; Save and close saves the graph first and then the scene. A failed graph
save keeps the application open. In the graph view, **Export incomplete draft**
accepts a separate new project-relative JSON path. Export leaves the active graph
dirty and does not register the draft in the project asset catalog.

The library's `GraphFile::create` API supports host-managed new graph assets;
this shell opens existing catalog assets and does not author project manifest
entries or expose a new-asset dialog. Scene/play/hot-reload transitions and file
selection belong to the shell. Runtime stepping, evaluation previews, clipboard
copy/paste, multi-selection, zoom, extensible-port schema editing and automatic
layout are not implemented. Future material and procedural domains can reuse the
canvas by registering their GraphKind.

## Validation

Focused tests cover shared atomic rollback and read-only history, save/reopen with
layout, external-write conflicts, separate draft policy and confinement, all
shipped kind registries and compiler diagnostics, and real egui pointer input for
palette creation, node drag, connection, deletion and single-gesture undo. The
editor shell adds a real asset-button input test that reaches the canvas, exports
a separate draft, saves/reopens layout and verifies the scene remains unchanged.
Run `cargo test -p engine-editor-graph -j1` and
`cargo clippy -p engine-editor-graph --all-targets -j1 -- -D warnings`.
