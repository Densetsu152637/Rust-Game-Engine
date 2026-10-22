# Engine ECS

The ECS layer of the engine. It re-exports `bevy_ecs` and `glam` (one dependency
identity for engine and game code) and adds only what Bevy does not provide.
Entities, components, queries, resources, deferred commands, change detection and
scheduling are plain Bevy; nothing here wraps or replaces them. The crate has no
app lifecycle or stages: `engine-core` builds those on top and re-exports this
crate, so `engine_core::Current`, `engine_core::bevy_ecs` and
`engine_core::prelude::*` keep working.

## Contents

- `Current<T>` / `Next<T>` / `Buffered<T>` / `publish_buffered::<T>`: published
  double-buffered values as ordinary components. `Buffered::new(v)` spawns both
  halves equal. Order `publish_buffered` with normal schedule edges; it copies only
  when values differ, so `Changed<Current<T>>` stays accurate.
- `RenderWorld`, `ExtractedFrom`, `ExtractionSet`, `extract_component::<T>`,
  `cleanup_extracted_entities`: change-driven simulation to render-world
  extraction with removal propagation and a source to render entity mapping.
  `EngineApp` wires these into its Extract stage; they also run on a bare
  `World` + `Schedule` (see `tests/buffered_extraction.rs`).
- `prelude`: Bevy's prelude, the glam types and the items above.

## Relation to the legacy C++ ECS

| C++ capability | Rust |
| --- | --- |
| `Alias<T, Tag>` wrapper components | Newtype components with `#[derive(Component)]` |
| `BufferedAlias` | `Current<T>` / `Next<T>` / `Buffered<T>` |
| Dirty tracking for render transfer | Bevy `Changed<T>`; `extract_component` |
| Views / filtered queries | Bevy `Query`, `With`/`Without`/`Added`/`Changed` |
| Structural deferral | Bevy `Commands` and apply points |
| Job scheduler / walls | Bevy schedules (`engine-core` stages) |
| `SharedAlias` value interning | Not ported: no consumer yet (add when batching needs it) |

## Checks

```text
cargo test -p engine-ecs
cargo clippy -p engine-ecs --all-targets -- -D warnings
```
