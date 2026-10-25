# Documentation

## Getting started

- [Repository README](../README.md): workspace layout, prerequisites, build and test.
- [Architecture](architecture.md): crate layering, one Rust type universe, the ECS
  graph scheduling model, render extraction and the desktop host.
- [Native game crates](scripting.md): how external Rust crates register components,
  systems and entity scripts, and how they are compiled into generations.
- [Project commands](commands.md): the `engine` command-line tool, JSON results and
  exit codes.
- [Project and scene format](project-format.md): version 1 manifest, scene, asset and
  validation contract.
- [Testing and CI](testing.md): test commands, hardware-gated tests and the GitHub
  Actions workflow.
- [Expansion validation](engine-expansion-validation.md): integrated 2D/3D,
  authoring, simulation and input acceptance results and remaining limitations.
- [Platforms](platforms.md): native host capabilities, package metadata and
  compile-only versus runtime support.

## Examples

- [First project](../examples/first-project/README.md): authored sample project with a
  game crate, lifecycle fixtures and an intentionally invalid crate.
- [Desktop 2D synthetic lab](../examples/desktop-2d/README.md): sprites, text,
  collision, rooms, audio, saves and a scripted walkthrough.
- [Native extension](../examples/native-extension/README.md): custom components,
  systems and a custom wgpu render pass from an external crate.
- [Programmable world](../examples/programmable-world/README.md): a 3D scene and
  retained HUD connected through typed callbacks, physics, animation and navigation;
  GPU-free default execution and an optional desktop window.
- [Acoustics showcase](../examples/acoustics-showcase/README.md): a 3D room and a
  2D side-view scene for spatial audio and ray-traced acoustics, as a data project.
- [PBR and shadows](../examples/pbr-shadows/README.md): three PBR spheres lit by a
  cascaded-shadow sun, a PCSS spot light and a point light, rendered to PNG or a
  window by an `engine-desktop` example.
- [Moving entity](../assets/scripts/moving_entity/src/lib.rs): the smallest script
  crate.

## Engine interfaces

- [Desktop rendering](desktop-2d-rendering.md): CPU content, the render contract,
  wgpu host and native render-feature extensions.
- [3D scene rendering](rendering3d.md): cameras, depth, metallic-roughness PBR
  materials, transparent draws and custom WGSL.
- [Mesh LOD, HLOD and culling](rendering-lod.md): the CPU engine-mesh-lod crate:
  quadric mesh simplification, LOD chains and selection, HLOD clusters, frustum culling.
- [Render architecture](rendering-architecture.md): the render extension API
  (features, stages, per-view data, HDR targets, light list, screen-lighting inputs,
  instancing), WGSL
  composition and the material shader contract, device feature composition,
  XR eye targets and texture streams.
- [Lights and shadows](rendering-lights.md): directional, point, spot and area
  light components in photometric units, clustered forward shading, cascaded,
  spot and cube shadows in a shadow atlas with PCF/PCSS, and lighting status.
- [HDR post-processing](rendering-post.md): tonemapping (ACES, AgX, Khronos Neutral),
  manual and histogram auto-exposure, bloom, HDR colour grading with a `.cube` 3D LUT,
  vignette and film grain; the `PostProcess` and `PostProcessVolume` components,
  volume blending and post status.
- [DLSS](dlss.md): the optional standalone `engine-dlss` NVIDIA Super Resolution
  crate, SDK setup and feature scope.
- [Hardware ray tracing core](rendering-raytracing.md): `engine-render-rt` tiers and
  status, the audited experimental token, BLAS/TLAS scenes, the WGSL ray-query library,
  RT shadow (hard/soft), ambient occlusion, reflection and diffuse-GI kernels, the
  spatio-temporal denoiser, effect settings/status, the reference path tracer, and the
  frame integration (`engine.rt.*` features, the `RayTracing` component, raster
  fallbacks and per-effect status).
- [Asset pipeline](asset-pipeline.md): bounded glTF/GLB import, dependency
  fingerprints, ownership and reload behavior.
- [Authored simulation](simulation.md): 2D/3D rigid bodies, animation and navigation
  adapters, transform authority and callback events.
- [Physics core](physics.md): shapes, material combine rules, named collision layers,
  `engine.PhysicsSettings`, the native physics API and extension seams.
- [Physics queries and determinism](physics-queries.md): ray, shape and overlap queries with filters,
  fixed-step stepping and interpolation, the `deterministic` feature, debug-view data, MCP `physics_query`/`physics_debug`.
- [Physics joints](physics-joints.md): fixed, hinge, slider, ball, spring, distance, rope and 6-DoF joints with limits, motors and breakable constraints.
- [Character controllers](physics-character.md): kinematic 2D/3D characters with slopes, steps, ground snapping, platform riding, pushing, gravity, jumps and ground/wall events.
- [Raycast vehicles](physics-vehicles.md): `engine.Vehicle3D` with raycast suspension, a slip-based Pacejka tyre model, engine/gearbox/differential drivetrain, input mapping, wheel visuals and events.
- [Skeleton ragdolls](physics-ragdoll.md): `engine.Ragdoll` bone bodies and joints, animated/ragdoll/powered modes, animation blend, get-up fade, hit reactions and settle events.
- [Physics destruction](physics-destruction.md): Voronoi pre-fracture of convex and concave meshes, `engine.Destructible3D` thresholds and damage, debris pooling, sleeping and expiry.
- [Physics rendering](physics-rendering.md): render features for the debug view, cloth, soft bodies,
  fluid particles and destruction fragments, with the render-world extraction and status reporting.
- [Cloth (XPBD)](physics-cloth.md): engine-xpbd world, collider proxies with two-way impulses, cloth stretch/bending/tethers, wind and pins.
- [Soft bodies (XPBD)](physics-softbody.md): tetrahedral edge/volume soft bodies, box tet generator and surface triangles.
- [Particle fluids](physics-fluids.md): CPU Position Based Fluids, emitters, sinks,
  rigid-body proxy coupling and renderer outputs.
- [Navigation meshes](navigation.md): layered polygon NavMesh baking, bounded path
  queries, authored `NavigationAgent` data and MCP navigation tools.
- [Physics library](../crates/engine-physics/README.md),
  [animation library](../crates/engine-animation/README.md), and
  [navigation library](../crates/engine-navigation/README.md): native APIs and limits.
- [Gameplay mechanics](gameplay2d.md): swept collision, triggers, placement, follow
  and flock helpers.
- [Input and UI](desktop-input-ui.md): action mapping, rebinding, focus handling and
  the modal UI model.
- [Custom and accessible input](custom-input.md): injectable peripheral adapters,
  remapping, hold/toggle actions and device diagnostics.
- [Retained game UI](game-ui.md): hierarchy/layout, controls, clipping, focus and
  native callback integration.
- [Multiplayer](networking.md): authoritative sessions, mutual-TLS 1.3 transport,
  replication, ownership, runtime integration and plaintext trusted-LAN limits.
- [Meta-programming and authoring](meta-programming.md): component descriptors,
  typed adapters, hierarchy, events, batches and prefabs shared with MCP.
- [Scheduled events](scheduled-events.md): CLI/MCP event injection, callback
  delivery and bounded execution traces.
- [Audio and persistence](desktop-services.md): the mixer, spatial sources, CPAL
  playback, durable saves and separate settings.
- [Spatial audio and acoustics](acoustics.md): authored acoustic components,
  ray-traced propagation with GPU tiers and CPU fallback, the 2D proxy world, MCP
  tools and the `--acoustics-backend` option; the decision record and verification
  plan are in [spatial-audio-design.md](spatial-audio-design.md), and the crates in
  [engine-acoustics](../crates/engine-acoustics/README.md) and
  [engine-acoustics-gpu](../crates/engine-acoustics-gpu/README.md).
- [Desktop packages](desktop-packaging.md): relocatable packages and dependency
  notices.
- [Editor](editor.md) and [portable viewport](portable-editor.md): the native
  desktop editor, shared document model, camera controls and isolated play host.
- [CPU profiling](profiling.md): measured stages, bounded frame history, counters
  and snapshot semantics.
- [MCP server](mcp.md): the stdio protocol, read/write/exec tools, resources,
  access levels and limits.

## World building

- [World building overview](world.md): the `engine_project::world` module, its
  per-feature component, install and tick-hook contract, and the render adapter.
- [2D tilemaps](world-tilemap.md): tilesets, chunked layers, rule tiles, merged
  collision, the `engine.Tilemap` component, the tile render data handed to the sprite
  path, native painting API and MCP tilemap tools.
- [Procedural content generation](pcg.md): seeded noise, the node graph and executor
  (`engine-pcg`), `engine.PcgVolume` runtime generation and the `pcg_*` MCP tools.
- [Level streaming](streaming.md): world partition cells, streaming sources, hysteresis
  and budgets, data layers, HLOD proxies, `engine.WorldPartition`, the native API and
  the `streaming_*` MCP tools (`engine-streaming`).
- [Heightmap terrain](terrain.md): the `.terrain` heightfield asset (`engine-terrain`),
  crack-free CDLOD geomorphing, sculpt/paint/hole brushes, `engine.Terrain` collider
  tiles, PCG surfaces and navigation input, the CPU chunk fallback, the `world.terrain`
  GPU feature (`engine-world-render`) and the `terrain_*` MCP tools.
- [Foliage and vegetation](world-foliage.md): foliage types, deterministic scatter and
  brush painting over terrain or PCG surfaces (`engine-foliage`), LOD bands with
  hysteresis and impostors, wind, `engine.Foliage`, the CPU cluster fallback, the
  `world.foliage` GPU feature and the `foliage_*` MCP tools.
- [Large-world coordinates](world-coordinates.md): floating-origin rebasing with f64
  absolute positions, `engine.LargeWorld` and `engine.AbsolutePosition`, how physics,
  navigation, streaming, terrain, foliage and PCG follow a shift, the `world.origin_shifted`
  event, telemetry and the camera-relative rendering property.

Per-crate details are in `crates/*/README.md`.
