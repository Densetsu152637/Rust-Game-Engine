# Physics rendering

`crates/engine-physics-render` draws the physics module's output through the render
extension API ([rendering architecture](rendering-architecture.md)): the physics debug
view, cloth and soft-body surfaces, particle fluids and destruction fragments. It is five
`PipelineFeature`s plus one extraction system; nothing in `engine-physics`,
`engine-project` or `engine-desktop` was changed for it.

| Feature | Stage | Draws | Data |
| --- | --- | --- | --- |
| `physics.debug_lines` | `Transparent`, after `engine.transparent` | collider outlines, contacts, joint anchors, cloth/soft-body wire edges, fluid particle ticks | `debug_view` + `deformable_debug_segments` |
| `physics.cloth` | `Opaque`, after `engine.opaque` | cloth triangles | `PhysicsRenderData.cloths` |
| `physics.soft_body` | `Opaque` | soft-body boundary triangles | `PhysicsRenderData.soft_bodies` |
| `physics.fragments` | `Opaque` | destruction fragment meshes | `fragments` / `fragment_mesh` |
| `physics.fluid` | `Opaque` | particles as sphere impostors | `PhysicsRenderData.fluids` |

## Setup

```rust
use engine_physics_render::*;

let config = PhysicsRenderConfig::default();
// 1. Copy physics data into the render world during the Extract stage.
install_physics_extraction(runtime.app_mut(), config.clone())?;
// 2. Register the features and keep the status handle.
let mut features = PipelineFeatures::default();
let status = register_physics_features_with(&mut features, &config)?;
runtime.app_mut().world_mut().insert_resource(features);
// Later: status.snapshot().get(names::CLOTH) -> activity, primitives, truncated, reason.
```

`register_physics_features(&mut features)` is the same with the default config. Both fail
on an invalid config or when a `physics.*` name is already registered.

### Why an extraction step

`FeatureContext.world` is the *render world* (`EngineApp::render_world`), filled by
`extract_component`. The physics data is not there: `PhysicsRenderData`, the destruction
state and the physics service (behind `debug_view`) are simulation-world resources.
`extract_physics_render` runs in the `Extract` stage (after `ExtractionSet::Cleanup`,
exclusive, ambiguity-free) and stores a `PhysicsRenderExtract` resource in the render
world:

- `data`: a copy of `PhysicsRenderData`. It is cloned only when `PhysicsRenderData.frame`
  changed (the data refreshes on frames where a physics step ran); otherwise the previous
  copy is moved forward.
- `debug`, `debug_state`, `debug_reason`, `debug_truncated`: debug segments, at most
  `max_debug_segments`. When `engine.PhysicsSettings.debug` is off (or there is no physics
  service) the state is `Off` with the engine's own message; nothing is drawn and the
  `physics.debug_lines` status carries the reason.
- `fragments`: for each `Fractured` entity, its live fragments with the world matrix of the
  fragment's `Transform` and the shared `Arc<FragmentRender>`, at most `max_fragments`.
  `fractured` lists the owners.
- `revision`: changes whenever content changed; features skip re-uploads for an unchanged
  revision (several views per frame upload once).

Fragment transforms are the simulation's current transforms (not the presentation
interpolation).

## Configuration

`PhysicsRenderConfig` (a world resource, validated; every range is checked and named in the
error):

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | all `true` | `PhysicsRenderToggles { debug_lines, cloth, soft_bodies, fluid, fragments }`: a feature with a `false` flag registers disabled by default; a camera can still enable it with `Camera3D.render_features` (`{"physics.fluid": true}`), and any feature can be switched off per camera the same way. Disabled features are also not extracted. |
| `debug_depth_test` | `true` | test lines against scene depth (`LessEqual`, no depth write); `false` draws over everything |
| `debug_line_width` | 2 px | constant screen-space width |
| `max_debug_segments` | 65 536 | cap on extracted and drawn segments (hard limit 1 048 576) |
| `max_surface_vertices` | 262 144 | vertex cap per surface feature (index budget 6 per vertex) |
| `max_fluid_particles` | 131 072 | particle cap |
| `max_fragments` | 512 | fragment cap (hard limit 4096) |
| `shading` | `Lit` | `Lit` or `NormalShaded` (see below) |
| `roughness` | 0.6 | perceptual roughness of lit surfaces |
| `cloth_color`, `soft_body_color`, `fragment_color`, `fragment_interior_color` | | linear RGBA vertex colours |
| `fluid` | | `FluidStyle`: `mode` (`Density`, `Speed`, `Flat`), `low`/`high` ramp colours, `max_speed`, `surface_emphasis` (0..=1 towards `surface_color`), `radius_scale` |

## Behaviour

- **Buffers** are rewritten per revision and grow by powers of two (4 KiB minimum) up to a
  hard cap derived from the config. Objects that would exceed a cap are dropped whole,
  never split; segments and particles are cut at the cap. The feature status reports
  `truncated` with the counts.
- **Debug lines** are instanced quads (6 vertices per segment) with constant pixel width.
  Segments are clipped to the near plane in clip space, so lines through the camera plane
  stay finite. They blend with alpha over the HDR colour.
- **Cloth, soft bodies, fragments** share one pipeline: world-space `EngineVertex` data
  (position, vertex colour, normal), two-sided (no culling; the shader flips the normal on
  back faces), depth write on. Fragments are flat-shaded, non-indexed triangles; triangles
  flagged `interior` (fracture surface) use `fragment_interior_color`.
- **Shading `Lit`** imports `engine::lighting` and uses the view bind group's light list,
  clusters, shadows, ambient and screen-lighting inputs (`engine_direct_lighting` +
  `engine_indirect_lighting`, dielectric, roughness from the config). The view's light data
  is accessible to features, so lit is the default and is reported as `ShadingReport::Lit`.
  `NormalShaded` is the explicit unlit fallback: vertex colour times
  `0.3 + 0.7 * (0.5 + 0.5 * dot(n, normalize(0.3, 1, 0.2)))`. It is only used when
  configured, and reported as `ShadingReport::NormalShaded`.
- **Fluid** particles are camera-facing quads. The fragment shader reconstructs the sphere
  (normal and `frag_depth`), so spheres intersect scene geometry and each other correctly
  and are lit like the surfaces. Colour: `Density` maps `density / rest_density` from 0.5
  (low) to 1.5 (high), `Speed` maps `|v| / max_speed`; surface particles blend towards
  `surface_color` by `surface_emphasis`.
- **Status**: `PhysicsRenderStatusHandle::snapshot()` returns per feature the last activity
  (`NotRun`, `Idle`, `Drew`, `Skipped`), primitives, cap, `truncated`, the shading mode and a
  `reason` (missing extraction, debug view unavailable, dropped objects, invalid data).

## Intact mesh of a fractured entity

`PhysicsRenderExtract.fractured` lists the `Fractured` owners so a host can skip their
intact mesh, but the engine's own mesh extraction does not read it yet: the intact mesh of a
fractured entity keeps drawing alongside its fragments until the project's 3D scene
extraction skips `Fractured` entities (a small `engine-project` / `engine-desktop` change,
outside this crate's scope). Fragments use configured colours; the owner's `MeshMaterial`
(texture, tint) is not applied.

## Verification

- `cargo test -p engine-physics-render --locked`: buffer growth and caps, line / surface /
  fragment / particle building (counts, bounds, colour mapping, rejection of inconsistent
  data), shader validation with naga, config validation, registration, and the extraction on
  a real `Runtime` (cloth, soft body, fluid, debug segments, fractured box).
- `cargo test -p engine-physics-render --locked -- --ignored --test-threads=1` (GPU, run on
  the RTX 5080 / Vulkan): a line's pixels at the projected coordinates with the requested
  width and its depth test against a scene mesh (on and off); a cloth quad's centre pixel
  equals the normal-shaded colour and is hidden by a nearer wall; lit cloth brightens under
  a directional light, both sides; a fragment triangle in the fracture colour; fluid
  impostors at the projected particle pixels and depth-correct against a wall.

## Deferred

- **Screen-space fluid surface** (depth smoothing, thickness, refraction) and marching-cubes
  meshing: particles draw as lit opaque sphere impostors. `FluidRender` already carries the
  inputs (positions, densities, surface flags, radius, domain).
- **Fragment materials** from the owner's `MeshMaterial`, and hiding the intact mesh (above).
- **Interpolated** deformable and fragment transforms: data is drawn at the latest
  simulation state.
- **Particles beyond the cap** are cut, not streamed; GPU-side culling is not done.
- Impostors are camera-facing billboards under perspective, so wide-angle edges are
  slightly elliptical.
