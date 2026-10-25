# World building

`engine_project::world` (crates/engine-project/src/world) groups the world-building
features. Each feature is one submodule that owns its authored `engine.*` component
descriptors, typed ECS adapters, installed services and per-tick hooks:

| Submodule | Feature |
| --------- | ------- |
| `coordinates` | Large-world coordinates |
| `terrain` | Heightmap terrain ([guide](terrain.md)) |
| `foliage` | Foliage and vegetation |
| `water` | Ocean, lake and river water |
| `streaming` | Level streaming and world partition |
| `pcg` | Procedural content generation ([guide](pcg.md)) |
| `tilemap` | 2D tilemaps |

## Hook contract

`world` aggregates the submodules in a fixed order:

- `component_descriptors()` is chained into `ComponentCatalog::new` after the
  simulation descriptors, so every world component is schema-validated and editable
  through MCP `component_set`.
- `register_components(app)` runs in `Runtime::new_with_simulation` after the
  simulation, UI and networking registrations.
- `install(world)` runs after `simulation::install`.
- `pre_simulation(world, dt)` runs inside each tick before `simulation::step`
  (forces, streaming requests, procedural generation).
- `post_transforms(world, dt)` runs after `refresh_world_transforms`
  (view-dependent level of detail and render geometry).

A hook error fails the tick with its diagnostic, like the simulation hooks.

## Rendering

Until the render-feature extension API lands, world geometry renders through the
existing mesh path: a `MeshRenderer` entity whose CPU mesh is supplied by a
`GeometryOverride`. Feature pages describe their adapter.
