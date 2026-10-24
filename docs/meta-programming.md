# Components, transforms, events and scene commands

The project document, native runtime, MCP tools and editor controller share the
`engine-project` model. Gameplay remains ordinary Cargo-compiled Rust. There is
no embedded expression evaluator or code downloaded from component schemas.

## Custom components

Set `componentSchemas` in `project.json` to a project-relative JSON file containing
an array of descriptors. The file is confined to the project and limited to 1 MiB.
For example:

```json
[
  {
    "name": "game.Health",
    "version": 1,
    "schema": {
      "type": "object",
      "properties": {"hp": {"type": "integer", "minimum": 0, "maximum": 100}},
      "required": ["hp"],
      "additionalProperties": false
    },
    "defaults": {"hp": 100},
    "requires": ["Transform"],
    "entityReferences": [],
    "dataOnly": false
  }
]
```

Entity component storage is explicit:

```json
{"custom": {"game.Health": {"version": 1, "data": {"hp": 75}}}}
```

Names must contain a namespace separator (`.`). Versions must match exactly.
`defaults` is a validated authoring example; missing required fields are rejected,
not silently filled. The supported schema subset is `type`, `properties`,
`required`, boolean `additionalProperties`, `items`, `minItems`, `maxItems`,
`minimum`, `maximum`, `minLength`, `maxLength`, `enum`, `description` and `default`.
Unsupported keywords, including `$ref`, are rejected. Types are object, array,
string, number, integer, boolean and null. Schemas have at most 16 nested levels;
component data is limited to 64 KiB. At most 256 custom types can be declared.

For native storage, derive Bevy `Component`, serde `Serialize` and `Deserialize`,
then call `register_component::<Health>(app, "game.Health", 1)` from the gameplay
crate's `configure`. Registration runs before startup. The runtime decodes authored
values into real typed ECS components before startup systems and script creation.
Systems can query and mutate them normally. Duplicate names and registering the
same Rust type under multiple names are rejected. Missing adapters fail startup.
`dataOnly: true` instead uses `catalog::DynamicComponents` and needs no adapter.

Snapshots encode current ECS values and validate them against their descriptor.
Hosts use `Runtime::try_snapshot()` and propagate serialization/validation errors.
The older infallible `snapshot()` includes explicit diagnostics when incomplete.
Scene transitions hydrate destination values; no schema is treated as executable
Rust. `entityReferences` lists JSON pointers to string entity IDs, checked during
scene validation and remapped during prefab cloning.

## Hierarchical transforms

`Transform.position` is local translation. Optional `rotation` is a quaternion in
XYZW order (default `[0,0,0,1]`), and `scale` defaults to `[1,1,1]`. A scene entity
may have a `parent` ID. Existing scenes have identity rotation/scale and no parent.
Parents must exist and have transforms. Cycles and more than 256 ancestors fail
validation. Quaternion values are normalized for evaluation; zero quaternions,
nonfinite values and singular scales are rejected.

`world_transform(world, entity)` resolves current local values to a column-major
`WorldTransform.matrix`; `refresh_world_transforms` updates derived ECS components.
Rendering transforms mesh vertices and sprite quads by the full matrix. Camera
follow, audio source/listener attachments and collision use world positions.
`move_entity` accepts world displacement and converts the result back into local
coordinates through the parent inverse. The existing 2D collision solver uses a
conservative world AABB around a rotated/scaled rectangle; it does not become an
oriented-box solver. Translation interpolation remains supported; rotation and
scale changes use the current simulation tick.

## Bounded events and callbacks

`EngineEvent` contains a name, optional source/target ECS entities and a JSON
payload. `emit_event(world, event)` queues it. `NativeScript::on_event` receives
targeted events (or broadcasts). Named Rust callbacks are registered through
`register_event_handler(app, "take_damage", callback)` and bound by an authored
component:

```json
{"EventBindings": {"bindings": [
  {"event": "collision.enter", "handler": "take_damage"}
]}}
```

Callbacks have signature
`fn(&mut World, Entity, &EngineEvent) -> engine_project::Result<()>`.
They run serially under the existing panic/owned-entity cleanup boundary.
Unregistered binding names fail startup. Collision results publish targeted
`collision.enter`, `collision.stay` and `collision.exit` events to both bodies,
with the other body as source and `{"other":"entity-id","phase":"enter"}`
payload. Native code can publish application-specific names through the same API.

At each tick the runtime drains a fixed FIFO snapshot before script updates.
Handler emissions wait until the following tick, preventing recursive dispatch.
Broadcast targets follow stable authored ID order. Dead targets are skipped,
including entities despawned by an earlier callback. `EventLimits` defaults to
1,024 queued events, 64 KiB per payload and 4,096 callback deliveries per tick;
exceeding limits returns an error. Scene changes clear events. Shutdown discards
pending events while retaining the last delivered records for reports.

`events::DeliveredEvents` is available to ECS systems. Runtime/headless reports
include the last tick's serializable `events` with authored source/target IDs and
payloads. Native test code may inject events using `emit_event` or atomic
`emit_events`. CLI `--events` and MCP `project_run_headless.events` schedule
stable-ID inputs and report bounded delivery traces across the run; see
[scheduled events](scheduled-events.md). This does not provide a live
running-process event channel.

## Transactional commands and prefabs

`SceneDocument::apply` handles component set/remove, reparent, prefab instantiation
and batches as well as the existing entity operations. Every operation checks the
revision, edits a candidate, validates the final scene and records one undo entry.
Failed batches leave the document and disk unchanged. Nested batches share a total
1,024-operation budget and a 16-level nesting bound. Reparenting preserves local
TRS; it intentionally changes world placement.

A `prefab` asset is a schema-1 scene fragment. `InstantiatePrefab` takes its asset
ID, a nonempty ID prefix and an optional destination parent. It copies the fragment,
prefixes IDs and remaps internal parents, camera-follow references and declared
custom entity references. Root entities attach to the requested destination parent.
Duplicate IDs, invalid references and exceeding 65,536 scene entities reject the
whole edit before publication. These are independent cloned instances; changing
the original fragment does not automatically update existing copies.

MCP `component_schema` combines built-in metadata and project descriptors.
`component_set`/`component_remove` accept namespaced custom names; custom set values
are `{version,data}`. `scene_edit` accepts `operations` using the serialized
`EditOperation` tagged union, including:

```json
{
  "scene": "scenes/main.json",
  "expected_revision": "revision-from-scene_inspect",
  "operations": [
    {"op":"instantiate_prefab","asset":"enemy-template","prefix":"enemy-1:","parent":null},
    {"op":"reparent","entity_id":"lamp","parent":"room"}
  ]
}
```

Other operation tags are `set_component`, `remove_component`, `set_position`,
`set_script`, `set_mesh_renderer`, `create_entity`, `replace_entity`, `delete_entity`
and `batch`. Fields follow the Rust enum's snake_case names. `dry_run` validates
without saving. Human controllers invoke these same document commands and share
their revision, validation and undo behavior.
