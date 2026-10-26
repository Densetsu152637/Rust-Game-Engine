# Graph documents and graph kinds

Behavior trees, state trees, visual scripts and the animation, audio and material
graphs of other domains share one data model, the `engine-graph` crate. A graph is a
project asset of kind `graph`: one JSON document whose `kind` field selects the graph
kind (`behavior_tree` today) that gives its nodes meaning. One generic node-graph
editor widget, one validator and one set of MCP tools therefore serve every graph
domain.

`engine-graph` is a pure crate (serde and serde_json only). Domains own their node
types and runtimes: `engine-ai` defines the `behavior_tree` kind and compiles it
([Behavior trees](ai-behavior-trees.md)); `engine-project` keeps the registry of kinds
and binds graph assets to the project and runtime.

## Document format

```json
{
  "version": 1,
  "kind": "behavior_tree",
  "nodes": [
    {"id": "root", "node_type": "root", "position": [0, 0]},
    {"id": "wait", "node_type": "wait", "params": {"seconds": 2.5}}
  ],
  "edges": [
    {"from": {"node": "root", "port": "child"}, "to": {"node": "wait", "port": "parent"}}
  ],
  "variables": [{"name": "alarm", "type": "bool", "default": false}],
  "metadata": {"author": "level-design"}
}
```

Every object rejects unknown fields (`deny_unknown_fields`). A document is at most
4 MiB.

| Field | Meaning |
| --- | --- |
| `version` | Format version; `1`. |
| `kind` | Graph kind name; must be registered. |
| `nodes[]` | `id` (unique, 1-64 characters of letters, digits, `_`, `-`, `.`), `node_type` (from the kind's registry), optional `ports`, `params` and `position` (`[x, y]`, editor canvas only). |
| `edges[]` | `from` and `to`, each `{node, port}`. An edge runs from an `out` port to an `in` port. |
| `variables[]` | `name`, `type`, optional `default` and `description`. For behavior trees these are the blackboard keys. |
| `metadata` | Free-form editor or tooling data, at most 64 entries. |

**Ports.** A port is `{name, direction: "in"|"out", type, multiple}`. Types are
`flow`, `bool`, `int`, `float`, `string`, `vec3`, `entity`, `any` and
`{"custom": "name"}`. `flow` carries control flow and connects only to `flow`;
`any` connects to every data type; `int` widens to `float`; a `custom` type matches
only itself. `multiple` says whether more than one edge may attach to the port.
`ports` may be omitted on a node: the registry's declaration of the node type is then
its port list. When present it must equal the declaration (extra ports are allowed
only for node types marked `extensible`). `graph_edit`'s `add_node` writes the ports
out so a document is self-describing for editors.

**Edge order.** The edges leaving one port are ordered by their position in `edges`.
Domains use that order as child order (priority order for a behavior tree selector),
and `connect`'s `index` inserts into it.

**Variables.** `type` is any non-`flow` type the kind allows; `default`, when present,
must fit the type (`vec3` is an array of three numbers; `entity` is an authored
entity id string).

## Graph kinds and node registries

A `GraphKind` declares:

- `name` and `description`;
- `allow_cycles` (false: any directed cycle is `graph.cycle`);
- `single_root` (true: exactly one node of a `root` node type must exist and every
  node must be reachable from it along edges);
- `limits` (nodes, edges, variables; each can only lower the hard ceilings of 4096
  nodes, 16384 edges and 256 variables);
- the allowed `variable_types`;
- a `NodeTypeRegistry`: for every node type its `category`, `description`, ports
  (`PortDef`: name, direction, type, `multiple`, `required`) and parameters
  (`ParamDef`);
- an optional semantic `check`, run after every generic rule passes (a behavior tree
  compiles the document here, so compiler problems surface as diagnostics).

A `ParamDef` has a `name`, a type, `required`, a `default`, bounds and a
description. Parameter types: `bool`, `int`, `float` (with `min`/`max`), `string`
(`max_len`, default 256), `vec3`, `enum` (`choices`), `variable` (the name of a
document variable, optionally restricted to `variable_types`) and `json` (any value
within the size limits; the domain interprets it).

Embedded JSON values (parameters, variable defaults, metadata) are bounded: at most
512 entries, nesting depth 8 and 4096 bytes per string.

## Validation

`validate(document, kind, level)` returns coded diagnostics `{code, path, message}`.
Output order is deterministic and the list is capped at 100 (the last entry is
`graph.diagnostics.truncated`). Paths name things by id so they stay valid while edits
reorder arrays: `/nodes/<id>`, `/nodes/<id>/params/<name>`,
`/nodes/<id>/ports/<name>`, `/edges/<from.node>.<from.port>-><to.node>.<to.port>`,
`/variables/<name>`, `/metadata/<key>`; the empty path is the document.

There are two levels, because edits pass through incomplete states:

- `structure`: identity, types, bounds, port rules and cycles;
- `complete` (the default): `structure` plus required ports and parameters, the root
  rule, reachability and the kind's own check.

| Group | Codes |
| --- | --- |
| Document | `graph.document.malformed`, `graph.document.too_large`, `graph.diagnostics.truncated`, `graph.version.unsupported`, `graph.kind.mismatch` |
| Limits | `graph.limit.nodes`, `.edges`, `.variables`, `.ports`, `.params`, `.value`, `.metadata`, `.edits` |
| Nodes | `graph.node.id_invalid`, `.duplicate`, `.unknown_type`, `.ports_mismatch`, `.position_invalid`, `.unreachable` |
| Ports | `graph.port.duplicate`, `graph.port.unconnected` (complete) |
| Parameters | `graph.param.unknown`, `.type`, `.range`, `.missing` (complete), `.variable` |
| Edges | `graph.edge.unknown_node`, `.unknown_port`, `.direction`, `.type_mismatch`, `.duplicate`, `.multiple` |
| Structure | `graph.cycle`, `graph.root.missing`, `graph.root.multiple` (complete) |
| Variables | `graph.variable.name_invalid`, `.duplicate`, `.type`, `.default` |
| Metadata | `graph.metadata.key_invalid` |
| Edits | `graph.edit.node_missing`, `.edge_missing`, `.variable_missing`, `.index` |
| Project | `graph.kind.unknown` (a `kind` no domain registered) |

Kinds add their own codes (behavior trees use `bt.*`, listed in
[Behavior trees](ai-behavior-trees.md#compiler-diagnostics)).

## Edit operations

Edits are pure functions over a document (`apply_edit`, `apply_edits`). Each is
applied to a copy; the copy is validated at the `structure` level and the edit is
rejected when it **introduces** a diagnostic the document did not already have, so an
edit can repair an invalid document but never makes it worse. A rejected edit leaves
the document unchanged. `apply_edits` runs a batch of at most 1024 operations
atomically and, with `Strictness::Complete`, also requires the final document to pass
full validation.

| `op` | Fields | Effect |
| --- | --- | --- |
| `add_node` | `id`, `node_type`, `params`, `position` | Adds a node with the type's declared ports. |
| `remove_node` | `id` | Removes the node and every edge attached to it. |
| `connect` | `from`, `to`, `index` | Adds an edge; `index` is the position among the edges already leaving `from` (child order); omitted means last. |
| `disconnect` | `from`, `to` | Removes one edge. |
| `set_param` | `node`, `name`, `value` | Sets one parameter; `null` removes it so the default applies. |
| `set_position` | `node`, `position` | Moves the node on the canvas (`null` clears). |
| `add_variable` / `set_variable` | `variable` | Declares / replaces a variable. |
| `remove_variable` | `name` | Removes a variable (rejected while a parameter references it). |
| `set_metadata` | `key`, `value` | Sets an entry; `null` removes it. |

Operations are JSON objects: `{"op":"connect","from":{"node":"seq","port":"children"},"to":{"node":"w","port":"parent"},"index":0}`.

## Project integration

- Assets of kind `graph` are validated at `Project::load` at the `complete` level with
  the kind named by the document. Code: `engine_project::graphs`
  (`load_graph`, `validate_document`, `kinds`).
- A project whose graph asset is invalid does not load, like any other asset kind.
  That is why `graph_edit` writes only complete graphs.
- Components reference graph assets by id (`engine.BehaviorTreeAgent.graph`); scene
  validation checks the reference and compiles the graph.

### Adding a graph kind to another domain

1. In your crate, depend on `engine-graph` only (no engine or ECS dependency is
   needed for the model) and define `pub fn kind() -> GraphKind`: build a
   `NodeTypeRegistry` of `NodeTypeDef`s (use `PortDef` and `ParamDef` builders), then
   `GraphKind::new(name, description, registry)` with `.single_root()`,
   `.allow_cycles()`, `.variable_types(..)`, `.limits(..)` and `.check(fn)` as needed.
   Put semantic validation in the `check` function (it only runs on documents that
   passed every generic rule).
2. Compile a document that passed `validate(.., Strictness::Complete)` into your
   executable form. Use `GraphIndex` for ordered edge lookups and
   `NodeTypeDef::effective_params` to read parameters with defaults applied.
3. Register the kind by adding it to the array in `engine_project::graphs::build`.
   `graph_schema`, `graph_validate`, `graph_edit` and project validation then handle it
   with no further change. The editor widget lists palettes from the same registry.
4. Test with the negative-case pattern in `crates/engine-graph/tests/graph.rs`: one
   focused document per diagnostic code.

## MCP tools

`graph_schema` and `graph_validate` are read tools; `graph_edit` needs `--allow-write`.
None executes game code. See also the [tool reference](mcp.md#read-tools).

| Tool | Arguments | Result |
| --- | --- | --- |
| `graph_schema` | `kind`, `node_type` (optional) | Without arguments: every kind briefly (flags, limits, node type names and categories). With `kind`: that kind's node types in full (ports with direction, type, `multiple`, `required`; parameters with type, default, `min`/`max`, `choices`; category; description), optionally one `node_type`. Always includes the `edit_operations` and the `diagnostic_codes`. |
| `graph_validate` | exactly one of `asset`, `path`, `document`; `level` (`complete`/`structure`) | `valid`, `kind`, counts and `diagnostics` (`code`, `severity`, `file`, `path`, `message`). Validation problems are data (`ok:true`); a file read through `asset` or `path` also returns its `revision`. Works on a graph whose asset is currently invalid (the asset is resolved through `project.json` without loading the project). |
| `graph_edit` | `asset` or `path`; `kind` (new files); `expected_revision`; `edits` (1-1024 operations) | Applies the batch atomically and writes the document. Returns `path`, `created`, `applied`, the new `revision`, `previous_revision`, counts and, for a new file, the `asset` entry to add to `project.json`. |

`graph_edit` rules:

- The final document must pass **complete** validation, otherwise nothing is written;
  the failure code is the first diagnostic's code, `details.op_index` names the failing
  operation (null when the batch is fine operation by operation but the result is
  incomplete) and `diagnostics` lists them all.
- An existing graph needs `expected_revision` (`hash_required` if missing,
  `revision_conflict` if stale). The revision is the hash of the file bytes, as returned
  by `graph_validate` and the previous `graph_edit`.
- A new graph is a project-relative `.json` path in an existing directory plus its
  `kind`; the first batch must build a complete graph (for a behavior tree: the root,
  its child and the connection). Existing files are never replaced without a revision.
- Writes are atomic (temporary file, flush, rename) and confined to the project like
  every write tool.
- Registering a new file is separate: add the returned `asset` entry to `project.json`.

Focused validation: `cargo test -p engine-graph`, `cargo test -p engine-mcp --test graph`.
