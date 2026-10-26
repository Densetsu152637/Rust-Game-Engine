# Persisted editor settings

The portable editor stores snapping, grid, gizmo mode/space/pivot and named camera
bookmarks in project-root `editor.json`. `engine-editor-settings` owns its versioned
schema, defaults, partial merge rules, numeric limits and bookmark validation.
Documents are at most 256 KiB, with up to 64 bookmarks. Unknown fields, unsupported
schema versions and invalid ranges are errors.

The MCP tools `editor_settings_get` and `editor_settings_update` use this same
model. Get is read-only: a missing file returns defaults, `exists:false` and
`revision:null`, without creating a file. Existing data is bounded before parsing;
invalid files are reported and preserved.

Update requires write access and a `patch` object. `snapping`, `grid` and `gizmo`
merge field by field; supplying `bookmarks` replaces the complete list. To edit an
existing file, pass its exact byte `revision` from Get as `expected_revision`.
A stale revision returns `revision_conflict`; omitting an existing file's token
returns `revision_required`. To create missing settings, omit the token. Every
patch is merged, validated and serialized before publication. Invalid patches
leave the original file unchanged.

For example, read settings, then update with:

```json
{"patch":{"snapping":{"enabled":true,"translate_step":0.25},"grid":{"spacing":2}},"expected_revision":"revision returned by editor_settings_get"}
```

The target is always root `editor.json`; neither tool accepts a caller path.
Both tools use MCP's no-link confinement resolver and bounded reads. Existing
files receive an exact-byte and confinement recheck immediately before atomic
replacement. Missing-file creation rechecks absence and uses no-clobber publication.
The recheck narrows the race window; it is not a cross-process compare-and-swap.
No scene, project manifest, game code or runtime state is changed.

These tools update persisted settings, not a running editor's cached view state.
Reopen the editor to load the new settings. The existing UI's independent Save
view settings action retains its documented last-writer-wins behavior and can
replace persisted settings with its cached state. Coordinate writers explicitly.

Native MCP transcript tests in `crates/engine-mcp/tests/editor_settings.rs` cover
access/discovery, defaults, round-trips, partial merge/bookmark preservation,
invalid input rollback, revision conflicts, file/patch bounds, rejected caller
paths and file-link confinement. File-link coverage reports a skip when Windows
does not grant symlink privilege. Run the focused test under the assigned build
slot with `cargo test -p engine-mcp --test editor_settings -j1`.
