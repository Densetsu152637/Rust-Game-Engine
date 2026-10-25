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
- [Desktop packages](desktop-packaging.md): relocatable packages and dependency
  notices.
- [Editor](editor.md) and [portable viewport](portable-editor.md): the native
  desktop editor, shared document model, camera controls and isolated play host.
- [CPU profiling](profiling.md): measured stages, bounded frame history, counters
  and snapshot semantics.
- [MCP server](mcp.md): the stdio protocol, read/write/exec tools, resources,
  access levels and limits.

Per-crate details are in `crates/*/README.md`.
