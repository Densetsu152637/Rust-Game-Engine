# Programmable 2D/3D engine expansion

This implementation plan tracks the requested expansion from the existing 2D
engine. Cargo remains the build and dependency tool. The editor is a client of
the authoring APIs; compiling and running a game does not require opening it.

## Acceptance criteria

1. Existing workspace and standalone examples build and pass their documented
   tests, formatting and lint checks.
2. Authored local translation, quaternion rotation, scale and parent relationships
   produce correct world transforms, reject cycles, and support reusable scene
   fragments with remapped internal references and transactional undo.
3. MCP and human authoring use the same validation/revision/document commands.
   Declared custom components can be discovered, edited, instantiated as native
   ECS components and observed in runtime snapshots.
4. Registered callbacks receive bounded engine events, including actual collision
   triggers. Callback-generated events cannot recurse without a tick boundary.
5. Both 2D and 3D scenes use a working render path, including camera projection,
   depth, authored materials and user WGSL. Hardware tests distinguish rendering
   evidence from CPU-only validation.
6. Physics, animation, assets, UI, navigation and multiplayer have executable
   implementations and behavior tests; registering an interface alone does not
   satisfy a subsystem requirement.
7. Profiling exposes measured timings and bounded history. Platform reports
   distinguish implementation, compilation and execution validation.
8. Documentation and examples describe the integrated behavior and its remaining
   limitations. Unsupported capabilities fail explicitly.

These criteria establish useful engine capabilities, not Unity parity or AAA
production readiness. Production scale, visual quality, platform certification,
security and performance require separate evidence on representative games and
hardware. A grid pathfinder is not a NavMesh implementation; basic lighting is not
a full physically based renderer; plaintext local networking is not a secured
Internet service.

## Shared contracts

- `engine-core` remains independent of windows, project files and device backends.
- Bevy ECS remains the native component/resource/scheduling model. Rust game code
  registers systems and adapters during `configure(&mut EngineApp)`.
- Authored IDs remain stable strings; runtime entity handles remain generational.
- `Transform` stores local position, quaternion rotation and scale. Derived
  `WorldTransform` matrices are column-major. Parent cycles and invalid numeric
  values are errors.
- Project component descriptors are available without executing game code.
  Runtime adapters hydrate typed native components after game configuration.
- `SceneDocument` owns transactional edits, revisions, undo/redo and save behavior.
  MCP and the editor must not maintain separate mutation semantics.
- Engine events use bounded queues. Events emitted during delivery wait for a
  subsequent tick. Script/callback failures use the runtime's existing error path.
- Rendering consumes immutable snapshots and resolved world matrices. Custom
  shaders use validated WGSL interfaces. Unsupported bindings are errors.
- Optional domain backends are supplied through explicit constructors/resources.
  Cargo features select compiled dependencies; they do not replace runtime
  dependency injection.

## Implementation order and ownership

| Workstream | Scope | Dependencies | Validation |
| --- | --- | --- | --- |
| Scene and metaprogramming | Catalog, custom native components, events, hierarchy, prefab instantiation, shared commands, MCP | Existing project/runtime | Round trips, invalid input, callbacks, transform consumers, revision conflicts |
| Simulation and content | Rapier 2D/3D, skeletal animation, glTF assets, bounded graph/grid navigation, authoritative replication | Standalone APIs first; scene adapters after shared model | Physical behavior, animation samples/skinning, import confinement, paths, real loopback clients |
| Rendering and authoring | 3D extraction, cameras/materials/custom WGSL, portable visual editor, UI improvements | Shared model and transforms | Recorded commands, GPU pixels, editor manipulation/undo, 2D regression checks |
| Profiling and platforms | Measured core timings, bounded snapshots, target capability checks and build coverage | Core lifecycle; integration into tooling | History/error tests, actual build results, truthful unsupported-target handling |
| Integration and review | Runtime adapters, examples, docs, cross-subsystem tests and review | All accepted streams | Workspace/examples tests, fmt, clippy, targeted hardware checks |

## Validation and remaining work

Implementation status and limitations must be updated from the final code and
recorded command results. Features are not considered complete merely because
they appear in this plan. Desktop, browser, mobile and console support each need
their own host, packaging, input/device and execution evidence. SDK or hardware
availability must be reported rather than simulated by capability flags.
