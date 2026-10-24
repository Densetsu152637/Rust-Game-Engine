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
- [Platforms](platforms.md): native host capabilities, package metadata and
  compile-only versus runtime support.

## Examples

- [First project](../examples/first-project/README.md): authored sample project with a
  game crate, lifecycle fixtures and an intentionally invalid crate.
- [Desktop 2D synthetic lab](../examples/desktop-2d/README.md): sprites, text,
  collision, rooms, audio, saves and a scripted walkthrough.
- [Native extension](../examples/native-extension/README.md): custom components,
  systems and a custom wgpu render pass from an external crate.
- [Moving entity](../assets/scripts/moving_entity/src/lib.rs): the smallest script
  crate.

## Engine interfaces

- [Desktop rendering](desktop-2d-rendering.md): CPU content, the render contract,
  wgpu host and native render-pass extensions.
- [3D scene rendering](rendering3d.md): cameras, depth, directional lighting,
  transparent draws, materials and custom WGSL.
- [Asset pipeline](asset-pipeline.md): bounded glTF/GLB import, dependency
  fingerprints, ownership and reload behavior.
- [Authored simulation](simulation.md): 2D/3D rigid bodies, animation and navigation
  adapters, transform authority and callback events.
- [Physics library](../crates/engine-physics/README.md),
  [animation library](../crates/engine-animation/README.md), and
  [navigation library](../crates/engine-navigation/README.md): native APIs and limits.
- [Gameplay mechanics](gameplay2d.md): swept collision, triggers, placement, follow
  and flock helpers.
- [Input and UI](desktop-input-ui.md): action mapping, rebinding, focus handling and
  the modal UI model.
- [Retained game UI](game-ui.md): hierarchy/layout, controls, clipping, focus and
  native callback integration.
- [Multiplayer](networking.md): authoritative sessions, replication, ownership,
  runtime integration and trusted-network limits.
- [Meta-programming and authoring](meta-programming.md): component descriptors,
  typed adapters, hierarchy, events, batches and prefabs shared with MCP.
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
