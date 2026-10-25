# UI authoring

Tooling for building the [retained game UI](game-ui.md) without running a game: an editable
document model with one shared operation vocabulary, layout inspection, a headless CPU preview,
and three MCP tools on top of them. The editor and the MCP server use the same code, so what an
agent edits is what the editor shows and undoes.

| Piece | Module (`engine_project::ui_tree`) | Purpose |
| --- | --- | --- |
| `UiDocument`, `UiOp` | `document` | A `UiRoot` (plus an optional theme) edited by validated, atomic, undoable operations. |
| `UiDiff`, `diff_ops` | `diff` | What differs between two roots, as changed IDs or as the operations that turn one into the other. |
| `inspect_layout`, `UiInspection` | `inspect` | Computed boxes per node, hit testing and layout issues. |
| `rasterize`, `RasterImage` | `raster` | Deterministic CPU rasterizer for a `runtime::Draw` list, PNG output. |
| `render_preview`, `UiPreviewAssets` | `preview` | Project assets, theme/locale/state resolution and the rasterized image. |
| `ui_inspect`, `ui_preview`, `ui_edit` | `engine-mcp` `tools/ui.rs` | The [MCP tools](#mcp-tools). |

## The document model

```rust
use engine_project::ui_tree::{UiDocument, UiOp};

let mut doc = UiDocument::from_json(&data)?;          // data of an engine.UiRoot component
let ops = UiOp::parse_all(&serde_json::json!([
    {"op": "add_node", "id": "quit", "widget": {"kind": "button", "text": "Quit"}, "parent": "menu"},
    {"op": "set_style", "id": "quit", "patch": {"width": {"unit": "pixels", "value": 120}}},
]))?;
doc.apply_batch(ops)?;                                 // one undo step
doc.undo();                                            // back to the previous JSON, byte for byte
```

Every operation works on a copy of the root. A batch is applied operation by operation (each with
structural checks such as unknown IDs and cycles), the final tree is validated once with
`UiRoot::validate`, and only then does it replace the document. A failing operation or an invalid
result returns the error of the first failure, prefixed with the operation index
(`op 2 (move_node): ...`), and leaves the document, its history and its generation untouched.
Intermediate states of a batch may be invalid, which is what makes multi-step edits possible (a
virtual list and its template child can be added in one batch). A batch that changes nothing
succeeds without creating a history step.

### Operation vocabulary

Operations are serde values tagged by `op` (snake_case). Node IDs are the stable authored IDs of
`UiNode.id`. "Sibling index" counts the children of one parent (or the top-level nodes) in vector
order, which is the paint and layout order.

| `op` | Fields | Effect |
| --- | --- | --- |
| `add_node` | `id`, `widget`, `parent?`, `index?`, `node?` | Adds a node. `widget` is a kind name (`"button"`) or an object with a `kind` plus fields laid over that kind's defaults. `node` may hold `style`, `visible`, `enabled`, `classes`, `tooltip`, `nav`, `accessibility`. `index` absent appends. |
| `remove_node` | `id` | Removes the node and its subtree, the bindings that targeted removed nodes and navigation overrides that pointed at them. |
| `move_node` | `id`, `parent?`, `index?` | Reparents and/or reorders the node with its subtree. `parent` absent keeps the parent, `null` makes the node top level; `index` is its final sibling position. At least one of the two is required. Moving a node into itself or its subtree is `ui.op.cycle`. |
| `duplicate_node` | `id`, `new_id?`, `parent?`, `index?` | Copies the subtree with fresh IDs (`<id>_copy`, `<id>_copy2`, ...; `new_id` names the copy of `id`). Navigation targets and the virtual-list `template` inside the copy point at the copy, bindings of copied nodes are copied. The copy follows the original by default. The report's `id_map` lists old to new IDs. |
| `rename_node` | `id`, `new_id` | Changes the ID and every reference: children's `parent`, navigation overrides, a virtual list's `template` and bindings. The report's `id_map` carries the rename. Theme selectors (`#id`) live in themes and are not rewritten. |
| `set_widget` | `id`, `patch` | Merge patch on the widget. A `kind` that differs from the current one first resets the widget to the new kind's defaults. |
| `set_style` | `id`, `patch` | Merge patch on `UiStyle` (camelCase fields). |
| `set_node` | `id`, `patch` | Merge patch limited to `visible`, `enabled`, `tooltip` and `nav`. |
| `set_classes` | `id`, `classes` | Replaces the class list. |
| `set_accessibility` | `id`, `accessibility` | Replaces the [accessibility](accessibility.md) overrides; `null` clears them. |
| `set_bindings` | `bindings` | Replaces all data bindings of the root. |
| `set_binding` | `node`, `property`, `binding` | Upserts (object with `path`, `mode`, `format`, `message`, `args`, `itemPath`) or removes (`null`) one binding. |
| `set_root` | `patch` | Patch limited to `layer`, `visible`, `enabled`, `theme`, `reducedMotion`. |

**Patch semantics.** Patches are JSON merge patches over the serialized value, whose default
fields are omitted: an object merges recursively, `null` resets a field to its default, arrays and
scalars replace. One rule keeps tagged values well formed: an object carrying a `kind` or `unit`
discriminator replaces instead of merging, so `{"width": {"unit": "fill"}}` never keeps the old
`value`. `{"margin": {"left": 4}}` changes only the left margin; `{"inset": {"top": null}}` makes
the top inset `auto` again.

**Widget defaults.** `add_node` and a kind-changing `set_widget` start from the serde defaults of
the kind plus the placeholders its required fields need (`label` text `Label`, `button` text
`Button`, `slider` 0 to 1 step 0.01, `tabs` one label, `virtual_list` item extent 32). `image`
and `nine_slice` need a `texture` and `virtual_list` a `template` child, otherwise the final
validation reports the problem. `UI_WIDGET_KINDS` lists the kinds.

**Limits.** At most `MAX_UI_OPS` (1024) operations per batch, `MAX_UI_PATCH_BYTES` (64 KiB) per
patch, widget spec or binding, and the node count is bounded while a batch applies
(`MAX_UI_NODES`, 1024), so a hostile batch cannot grow the working copy beyond one root.

### Errors

Stable codes, in the first diagnostic of the returned `engine_project::Error`:
`ui.op.invalid` (malformed operation list), `ui.op.limit`, `ui.op.unknown_node`,
`ui.op.invalid_id`, `ui.op.duplicate_id`, `ui.op.cycle`, `ui.op.index`, `ui.op.widget`,
`ui.op.style`, `ui.op.node`, `ui.op.field` (a patch key that is not allowed), `ui.op.accessibility`,
`ui.op.binding`, and the codes of `UiRoot::validate` (`ui.tree.invalid`, `ui.bind.invalid`, ...) for a
batch whose result is invalid.

### History

`apply`, `apply_batch` and `apply_labeled` each create exactly one undo step holding a snapshot
of the previous root, so `undo` restores a root whose serialization is identical to the one before
the edit and `redo` the one after it. A new edit clears the redo stack. The depth is bounded:
`DEFAULT_UI_HISTORY` (128) steps, configurable from 1 to `MAX_UI_HISTORY` (1024) with
`set_history_limit`; the oldest steps drop first. `undo_label` and `redo_label` name the next step
(`add_node`, `batch of 3`, or the label given to `apply_labeled`). The theme (`set_theme`) is not
part of the history.

### Selection and notification

`select(ids)` replaces the selection (an ordered list of node IDs; unknown IDs are an error and
change nothing, duplicates collapse). The selection follows renames, drops removed nodes (also
after undo and redo) and is not part of the history. `generation()` increments on every edit, undo,
redo and theme change, not on selection changes.

Each change produces a `UiChange { generation, kind, label, diff, selection }` with `kind` one of
`Edit`, `Undo`, `Redo`, `Theme`, `Selection`. Register callbacks with `add_listener` (called
synchronously after the change; `remove_listener` takes the returned ID) or poll `take_changes`
(the newest 256 changes are kept). `UiChange::diff` names exactly what changed, so a panel can
refresh only those nodes.

### Diff

`diff_roots(before, after)` returns a `UiDiff`: root fields that differ, whether the bindings
differ, `added` and `removed` IDs, `changed` nodes with the field names that differ (`parent`,
`widget`, `style`, `visible`, `enabled`, `classes`, `tooltip`, `nav`, `accessibility`) and the
parents whose child order differs (`reordered`, counting only children that kept that parent).
`changed_ids()` is the sorted union of added, removed and changed IDs.

`diff_ops(before, after)` returns the `UiOp` list that turns `before` into `after`: survivors below
removed nodes move out first, removals, additions (parents first), two-pass reparenting (so swapped
hierarchies never cycle), field patches computed as inverse merge patches, bindings, and finally
`move_node` with an index per parent to restore sibling order. Applying the list with `apply_ops`
yields a root whose `UiTree::canonical()` tree (depth-first order, sibling order kept) equals that of
`after`; the position of unrelated subtrees inside the node vector is not reproduced.

## Layout inspection

`inspect_layout(root, viewport, text, states)` (or `UiDocument::inspect`) lays the root out with
the same code as rendering (`UiTree::laid`, so the boxes are exactly those of `layout_with`) and
returns a `UiInspection`:

- `nodes` in paint order, each a `UiNodeLayout`: `id`, `parent`, widget `kind`, `depth`, `rect`
  (border box), `content` (minus padding and widget chrome), `clip` (the ancestors' clip),
  `visible_rect` (`rect` clipped by `clip`), `enabled`, `overlay` (part of an open dialog), the
  virtual-list `item` of a replica, `scroll`/`scroll_max` and the interaction `states` you passed.
  All values are logical pixels.
- `unplaced`: nodes without a box (hidden, in an unselected tab, a closed dialog).
- `hit_test(point, mode, root)` and `hit_stack(...)`: the topmost node under a point and the stack
  below it. `UiHitMode::Any` sees every visible box (containers and disabled nodes included),
  `Interactive` only enabled nodes that receive pointer presses.
- `issues`: layout findings (at most `MAX_UI_ISSUES`, 200; `issues_truncated`), each with a stable
  `code`, a `severity` (`warning` or `info`), the `node` and a message.

| Code | Meaning |
| --- | --- |
| `ui.issue.overflow` | A child's border box extends more than half a pixel past its parent's (not reported inside scroll views and virtual lists). A warning when the parent clips, otherwise info. |
| `ui.issue.zero_size` | A node's box has no width or no height. |
| `ui.issue.clipped_away` | A node has area but none of it is visible: clipped by an ancestor or outside the viewport (not reported inside scroll views). |
| `ui.issue.unreachable_focus` | A focusable, enabled node cannot take keyboard focus: an ancestor or the root is disabled, an open modal dialog traps focus, or it is completely clipped. |
| `ui.issue.nav_target_not_focusable` | A `nav` override names a node that is not focusable. |

Inspect the *effective* root to see a themed, localized look: `effective_root(root, id, &view)`
applies the theme cascade (with the interaction `states`), message references and a default font,
exactly like the runtime does before layout.

## Headless preview

`render_preview(root, id, &assets, &options)` (or `UiDocument::preview`) resolves the effective
root, lays it out at `options.viewport`, extracts the same `Draw` list the game renders
(`UiRoot::extract_draws_with`) and rasterizes it into a `RasterImage` (RGBA8, top row first;
`to_png()` encodes it with the `image` crate, `digest()` hashes the pixels).

`UiPreviewAssets::from_project(project, wanted_textures)` loads what a preview needs: every bitmap
`font` with its atlas, every `font_face` and the `font_config` (as the shaping `EngineTextBackend`),
the textures named by the tree (`referenced_textures`) and the project's localization. Other
assets stay on disk. `sole_font()` returns the project's only font, used for nodes that name none.

### Rasterizer fidelity

The rasterizer reproduces the engine's 2D pipeline on the CPU:

- Draws sort by `(layer, id)` like the sprite renderer; only `DrawSpace::Ui` draws are drawn.
- Triangles fill by pixel centre with the top-left rule (a shared edge draws exactly once), either
  winding. Vertex colour and UV interpolate linearly.
- A texture is sampled `Nearest` (the engine's GPU sampler, the default) or `Bilinear`, clamped at
  the edges; the sample multiplies the vertex colour (white without a texture).
- Straight-alpha blending with the GPU `ALPHA_BLENDING` equations on 8-bit channels
  (`rgb = src.rgb * src.a + dst.rgb * (1 - src.a)`, `a = src.a + dst.a * (1 - src.a)`), rounding to
  nearest after every draw. Colours are used as stored; there is no sRGB conversion. With an opaque
  clear colour (the preview default) this is the usual "over" operator.
- Clip rectangles cut each draw; `scale` multiplies coordinates (logical pixels to image pixels).

Differences to the GPU path, all deliberate: text is rendered from the **coverage** atlases the
text backends produce (bitmap fonts and `engine-text` coverage pages) — SDF text, the colour-vision
filter, world-space panels, depth testing, MSAA, post effects and the other GPU render features are
out of scope and absent from previews; edge pixels and rounding may differ by a few 8-bit steps
from a GPU frame. Output is deterministic for a given draw list and options (the unit tests render
twice and compare digests).

| Limit | Value |
| --- | --- |
| Image side | `MAX_RASTER_DIMENSION` 4096 pixels (MCP previews: at most 2048 x 2048 pixels) |
| Draws per frame | `MAX_RASTER_DRAWS` 65,536 |
| Triangles per frame | `MAX_RASTER_TRIANGLES` 1,000,000 |
| Pixel visits (summed triangle bounding boxes) | `MAX_RASTER_FILL` 536,870,912; `RasterOptions::max_fill` can lower it |

Errors: `ui.preview.size`, `ui.preview.too_complex`, `ui.preview.invalid_mesh` (indices outside the
vertices, a partial triangle, non-finite positions), `ui.preview.encode`. Layout errors keep their
own codes (`ui.tree.invalid`, `render.font.unknown`, `render.texture.unknown`, ...).

## MCP tools

All three take `scene` (path) and `entity` (the entity holding `engine.UiRoot`). Failures use the
usual `{ok:false, error:{code,message,hint}}` shape; engine codes pass through (`ui.op.*`,
`ui.tree.invalid`, `ui.preview.*`, `render.font.unknown`) next to `ui_root_missing` (the entity has
no `engine.UiRoot`), `ui_root_invalid`, `ui_conflict` (stale `expected_hash`), `ui_theme_invalid`,
`ui.theme.unknown`, `ui_locale_unavailable` and the generic `invalid_argument`,
`revision_conflict`, `entity_not_found`.

Read tools share these view arguments: `viewport` `[w, h]` (default `[1280, 720]`), `theme` (name of
an `engine.UiTheme` entity of the scene; default the root's own `theme`; `""` for none), `locale`,
`states` (node ID to flag names `hovered`, `pressed`, `focused`, `disabled`, `checked`, `open`,
`selected`; they pick `:hover`/`:pressed`/`:focus`... rules and the focus ring) and `font` (default:
the project's only font). Bindings are not evaluated and themes registered natively in
`UiStyleSettings` are not visible: previews show the authored tree plus scene themes.

### `ui_inspect` (read)

Returns `revision`, `ui_hash` (pass it as `expected_hash` to `ui_edit`), `node_count`, `viewport`,
`theme`, `text_backend` (`shaped`, `bitmap` or `none`: without any font text measures empty and a
warning says so), `font`, `nodes` (paged with `offset`/`limit`, default 300; the layout fields above
plus `children`), `unplaced`, `issues` and, with `point`, `hit` (`id`, `interactive`, `stack`).

### `ui_preview` (read)

Returns the PNG as MCP image content (`image/png`, base64) after the structured result: `width`,
`height`, `png_bytes`, `digest`, `draws`, `triangles`, `theme`, `font`. Extra arguments: `scale`
(0.1 to 8, default 1), `background` (straight RGBA, default a dark grey) and `sampling` (`nearest`
or `bilinear`). The image may have at most 2048 x 2048 pixels (viewport times scale), and its base64
PNG must fit the server's output message limit (1 MiB by default, which allows about 760 KB of PNG)
next to the result; otherwise `ui.preview.size` says what to lower. Text needs a font: a project without fonts and a tree with text
fails with `render.font.unknown` and a hint.

### `ui_edit` (write)

Arguments: `expected_revision` (scene revision, from `scene_inspect`, `entity_get`, `ui_inspect` or
the previous edit), `operations` (the vocabulary above, at most 1024), optional `expected_hash`
(`ui_hash` of the root you based the edit on) and `dry_run`. The batch is applied to the entity's
`engine.UiRoot` with `apply_ops`, and the validated root is saved through the same revisioned
scene-edit path as `component_set` (`SceneDocument`), so the scene is validated against the whole
project and written atomically. Any failure — stale revision (`revision_conflict`), stale hash
(`ui_conflict`), a failing operation, an invalid result, a scene that no longer validates — leaves
the file untouched.

The result carries `saved`, `dry_run`, `previous_revision`, `revision`, `previous_ui_hash`,
`ui_hash`, `node_count`, `reports` (per operation: `op`, `ids`, `id_map`), `diff` (`UiDiff`) and
`changed_ids`. Without write access the tool is neither listed nor callable.

Example: add a button and rename another in one atomic step.

```json
{"scene": "scenes/start.json", "expected_revision": "<revision>", "entity": "hud",
 "operations": [
   {"op": "add_node", "id": "quit", "widget": {"kind": "button", "text": "Quit", "action": "quit"},
    "parent": "panel", "node": {"style": {"width": {"unit": "pixels", "value": 80}}}},
   {"op": "rename_node", "id": "go", "new_id": "play"}
 ]}
```

Themes (`engine.UiTheme`) and everything else about a UI entity are edited with the generic
`component_set`; `ui_edit` covers only the tree.

## How the editor consumes this

The editor domain owns the panels; it builds on `UiDocument` and does not duplicate its logic.

1. Load the `engine.UiRoot` component data with `UiDocument::from_json` and keep one document per
   open UI entity. Map editor gestures to `UiOp` values (drag a node: `move_node`; inspector field:
   `set_style` / `set_widget` / `set_node`; palette drop: `add_node`; copy-paste: `duplicate_node`;
   F2: `rename_node`) and group multi-step gestures into one `apply_batch` so they undo together.
2. Drive Edit > Undo/Redo from `can_undo`/`can_redo`/`undo_label`/`redo_label` and the shortcuts
   from `undo`/`redo`. Use `select`/`selection` for the hierarchy and viewport selection.
3. Subscribe with `add_listener` (or poll `take_changes`) and refresh only `UiChange::diff`'s
   `changed_ids`; compare `generation()` to skip redundant work.
4. Draw selection overlays and guides from `inspect()` boxes, pick nodes with `hit_test`, and list
   `issues` in a diagnostics panel. Inspect the effective root (`effective_root`) when a theme or
   locale is active.
5. For a viewport image without a GPU (thumbnails, golden tests, remote sessions) call
   `preview(&assets, &options)`; interactive viewports should keep using the real renderer, which
   shows SDF text and the GPU features that previews omit.
6. Save by writing `doc.to_json()` back as the component value through the scene document
   (`SceneDocument::apply` with `EditOperation::SetComponent`) or `doc.into_root()` natively. The
   same operations arrive from MCP through `ui_edit`; an editor that holds a document open should
   reload (or `diff_from`) when the scene revision changes under it.

## Tests

`cargo test -p engine-project --test ui_authoring` covers every operation with undo/redo to
identical JSON, batches as one step, rename references, cycle rejection, unique duplicates, invalid
operations leaving the document unchanged, bounded history, selection and notifications, diff
round trips, inspection against `layout_with`, hit testing and issue detection, rasterizer pixel
tests (solid colour, alpha, clipping, layers, texture sampling, scale, hostile input) and previews
of bitmap and TrueType labels. `cargo test -p engine-mcp --test ui_authoring` covers the MCP tools
(inspect output, PNG image content, edit/stale/invalid/dry-run, gating); the tool-count and order
contract tests in `engine-mcp` and `engine-tooling` include the three tools.
