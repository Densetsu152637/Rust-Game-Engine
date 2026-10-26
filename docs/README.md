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
- [PC VR (OpenXR)](xr.md): `engine-xr` runtime discovery and honest status,
  session lifecycle, controller actions and `XR:` bindings, tracking, haptics,
  Vulkan swapchains and the `engine.XrSettings` component.
- [Machine learning](ml.md): bounded ONNX model admission, CPU inference, asynchronous
  jobs and optional native ECS services.
- [Asset pipeline](asset-pipeline.md): bounded glTF/GLB import, dependency
  fingerprints, ownership and reload behavior.
- [Cinematic sequences](cinematics.md): authored transform/FOV tracks, camera cuts,
  deterministic playback, marker events and immediate native scrubbing.
- [Morph targets](animation-morph.md): imported defaults, live weight controls,
  cubic weight playback and morph-before-skin mesh deformation.
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
- [Crowds, carving and off-mesh links](ai-crowds.md): ORCA crowd steering, formations,
  dynamic navmesh obstacles, off-mesh links and their authored components.
- [Graph documents](ai-graphs.md): the shared node-graph model (`engine-graph`), the
  `graph` asset kind, graph kinds and node registries, stable validation codes, atomic
  edit operations, how a domain adds a kind, and the `graph_schema`, `graph_validate`
  and `graph_edit` MCP tools.
- [Behavior trees](ai-behavior-trees.md): the `behavior_tree` graph kind and `engine-ai`
  runtime: composites, decorators with observer aborts, services, the typed blackboard,
  `engine.BehaviorTreeAgent`, navigation-driven `move_to`, native tasks, events,
  telemetry and limits.
- [State trees](ai-state-trees.md): the `state_tree` graph kind: hierarchical states with
  enter conditions, ordered tasks, prioritised and delayed transitions, evaluators,
  `engine.StateTreeAgent`, navigation-driven `move_to`, native tasks and evaluators,
  events, telemetry and limits.
- [Physics library](../crates/engine-physics/README.md),
  [animation library](../crates/engine-animation/README.md), and
  [navigation library](../crates/engine-navigation/README.md): native APIs and limits.
- [Gameplay mechanics](gameplay2d.md): swept collision, triggers, placement, follow
  and flock helpers.
- [Input and UI](desktop-input-ui.md): action mapping, rebinding, focus handling and
  the modal UI model.
- [Custom and accessible input](custom-input.md): injectable peripheral adapters,
  remapping, hold/toggle actions and device diagnostics.
- [Retained game UI](game-ui.md): taffy flexbox/grid layout, widget set (text input,
  dropdown, scroll/virtual lists, tabs, modal dialogs, tooltips), spatial navigation,
  text backend and text-edit event contracts, limits.
- [Accessibility](accessibility.md): the neutral accessibility tree, assistive-technology
  action requests, the AccessKit screen-reader bridge (Windows UI Automation observed),
  UI/text scale, reduced motion, high-contrast theme, focus ring, the input-remapping
  screen, and captions and subtitles (`caption_track` assets with speakers and timing,
  `engine.CaptionPlayer`, WebVTT/SRT import, caption settings, polite live region).
- [UI styling](ui-styling.md): themes with tokens and selector rules (kind, class, id, state
  pseudo-classes), cascade and specificity, transitions and keyframe animation with
  reduced motion, data binding with a string-resolver seam, world-space UI panels.
- [UI authoring](ui-authoring.md): the editable `UiDocument` (one serializable operation
  vocabulary, atomic validated batches, bounded undo/redo, selection, change notifications,
  diff), layout inspection with hit testing and issues, the headless CPU preview rasterizer and
  PNG output, and the `ui_inspect`/`ui_preview`/`ui_edit` MCP tools.
- [UI render features](ui-render.md): the `engine-ui-render` crate (SDF screen text, depth-tested
  world panels and world text in `Camera3D` scenes, the colour-vision filter), its routing rule,
  status and GPU measurements.
- [Text engine](text.md): the `engine-text` crate (font fallback, shaping, bidi, line
  breaking, rich-text markup, caret geometry, coverage/SDF glyph atlases) and its game UI
  integration: `font_face`/`font_config` assets, the shaping `UiTextBackend`, rich labels
  with links and inline images, IME and typed text, world-space text and SDF status.
- [Localization](localization.md): the `engine-locale` crate (Fluent string tables, plurals,
  locale fallback, pseudo-localization, settings) and its project integration:
  `string_table`/`locale_config` assets, `@key` text references in the game UI, message
  bindings with plural arguments, the runtime language switch, per-locale fonts and asset
  overrides and the `locale_*` MCP tools.
- [Multiplayer](networking.md): authoritative sessions, mutual-TLS 1.3 transport,
  replication, ownership, runtime integration and plaintext trusted-LAN limits; delta mode
  over any transport (TCP or QUIC) with the `engine.NetworkReplication`, `NetworkHitboxes`,
  `NetworkVoice` and `NetworkSession` components, RPCs and events, lag-compensated queries,
  voice, link simulation, network telemetry and the `network_inspect` /
  `network_settings_set` MCP tools.
- [Network transports](networking-transport.md): the `MessageEndpoint` lane/delivery
  contract over TCP and QUIC, limits, congestion control, caller rustls configs and
  loss/latency simulation.
- [Scalable replication](networking-replication.md): the sans-IO `engine-net-replication`
  crate: generation-checked entity IDs, a typed/quantized component and RPC registry, delta
  compression against acknowledged baselines under loss and reordering, interest management,
  priority and bandwidth budgets, validated RPCs, the bounded wire format and telemetry.
- [Prediction, rollback and lag compensation](networking-prediction.md): the sans-IO
  `engine-net-predict` crate (client prediction and reconciliation, redundant input
  transport, prediction keys for abilities, GGPO-style rollback, rewound hitbox queries),
  its limits and trust model.
- [Voice chat transport](networking-voice.md): the sans-IO `engine-net-voice` crate:
  pluggable codec trait with a raw-PCM path, server routing (channel/proximity with server
  positions), adaptive jitter buffer and packet-loss concealment hooks.
- [Online services](online-services.md): the self-hostable `engine-online` crate and
  `engine-online-server` reference server: Ed25519 tokens bound to the client's TLS certificate,
  the admission gate, lobby / session browser, rule-based matchmaking with a widening skill
  window, deterministic host migration with epoch fencing, the bounded `Lane::SESSION` JSON
  protocol, server configuration, deployment and threat model.
- [NAT traversal and relay](online-nat-relay.md): the `engine-net-nat` crate (reflexive
  discovery, candidate sets, HMAC-authenticated hole punching, the QUIC/control
  `DemuxSocket`, the two-party relay with `RelaySocket` and the self-hosted
  `engine-relay-server` binary), wire formats, NAT types handled and the threat model.
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

- [Animation pose graphs](animation-graph.md): validated pose DAGs, state
  transitions, 1D/2D blend spaces, two-bone IK, project bindings and CPU skin deformation.

Per-crate details are in `crates/*/README.md`.
