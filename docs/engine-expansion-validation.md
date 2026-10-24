# Engine expansion validation

Validated on Windows on 2026-10-24. The tested implementation revision is
`887848b8744eed220025e2dcad397e1de54beb9a`; subsequent validation-document edits
do not change executable code. Cargo remains the build and dependency tool.

## Acceptance results

The nine baseline criteria in [the implementation plan](engine-expansion-plan.md)
are met within the capability and validation boundaries below.

| Criterion | Evidence |
| --- | --- |
| Workspace and examples | 3,327 workspace tests and 102 standalone example tests passed; zero failures. Workspace and all five examples passed formatting and Clippy with warnings denied. The desktop-2D fixture validator passed. |
| Scene composition | Integrated hierarchy/TRS, cycle rejection, prefab reference remapping, transactional edits and undo tests passed. |
| Human/MCP authoring and components | Shared document/catalog tests passed; MCP discovers built-in and custom schemas, and native typed components round-trip through runtime snapshots. |
| Callbacks and events | Collision/animation/UI/navigation integration and bounded event tests passed. A real MCP stdio run compiled native gameplay, delivered a scheduled event, changed an entity's position, returned its delivery trace and preserved the authored scene. |
| 2D/3D rendering | Five real GPU pixel tests passed for 2D draws and 3D depth/lighting/custom WGSL; an additional external-crate custom-pass GPU test passed. CPU skin deformation is checked through both scene extraction paths. |
| Domain implementations | Asset import, physics/joints/sensors, skeletal animation, navigation, retained UI and authoritative loopback networking behavior tests passed. The programmable-world example exercises their runtime composition. |
| Profiling and platforms | Measured CPU profiles, bounded retention, platform capability errors and native package metadata tests passed. MCP reports bounded profiles, network status and input-device diagnostics. Platform execution limits remain explicit. |
| Documentation and example | Domain docs describe supported behavior and limits. The showcase's headless executable and optional desktop compilation passed; an actual native CLI/GPU screenshot was inspected. |
| Input/accessibility | Keyboard/mouse/controller mapping and custom-adapter tests passed, including focus, hotplug, remapping held controls, overlapping sources, hold/toggle behavior, bounded failures, snapshots and modal capture. |

The showcase's headless result was score 1, four 3D meshes, eight HUD draws, two
animation markers, one navigation arrival and one physics contact. Its native
CLI screenshot showed the 3D platform/cubes and retained 2D HUD. This demonstrates
composition, not production visual quality or physical editor interaction.

## Reproducible checks

```text
cargo test --workspace --locked --no-fail-fast
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check
```

For each manifest below, both `cargo test --manifest-path <manifest>` and
`cargo clippy --manifest-path <manifest> --all-targets -- -D warnings` passed:

- `examples/first-project/Cargo.toml`
- `examples/desktop-2d/Cargo.toml`
- `examples/native-extension/Cargo.toml`
- `examples/programmable-world/Cargo.toml`
- `assets/scripts/moving_entity/Cargo.toml`

The GPU checks ran the compiled `gpu_pixels`, `scene3d` and external `extension`
test executables with `--ignored --nocapture`; see [testing](testing.md) for their
equivalent Cargo commands. Device: NVIDIA GeForce RTX 5080, Vulkan, driver 617.14.
The showcase desktop feature passed `cargo check`; its default headless executable
also ran successfully. Full local logs and the inspected screenshot are retained
under the checkout's ignored `target/engine-expansion-*` and
`target/programmable-world.png` paths, not committed as source artifacts.

The final portable-library check also passed:

```text
cargo check --locked --target wasm32-unknown-unknown -p engine-core -p engine-ecs -p engine-render -p engine-platform
```

This establishes compilation of these libraries, not a runnable browser game.

Earlier integration runs exposed outdated discovery expectations and an ambient
`RUST_LOG=warn` setting in a lifecycle-log test. The tests now cover the expanded
tool/component catalog and explicitly request the log level they assert. Native
compilation correctly rejected source edits during one intermediate run; the
final frozen revision passed the complete suite.

## Remaining boundaries

- This is a functional engine baseline, not Unity parity or an AAA production
  engine. A full animation authoring/state-machine editor, crowd avoidance,
  image-based lighting and cascaded shadows, large-world streaming and
  production-scale performance need further work. Later work added
  metallic-roughness PBR with directional shadows ([rendering](rendering3d.md)) and
  layered NavMesh baking ([navigation](navigation.md)).
- Networking is a bounded authoritative implementation. It offers mutual-certificate
  TLS 1.3 ([multiplayer](networking.md)) alongside explicit plaintext trusted-LAN
  APIs, without matchmaking, NAT traversal, prediction or rollback services.
- The optional `engine-dlss` crate provides Super Resolution only; Frame Generation,
  Ray Reconstruction and DLSS 5 neural rendering are not implemented ([DLSS](dlss.md)).
- Profiles measure CPU stages, not GPU timestamps, allocation costs or individual
  systems. The editor viewport uses GPU readback and basic origin-based picking.
- Native Windows/Linux/macOS host paths exist. Hosted Linux/macOS results and
  interactive execution must be verified separately. Mobile, browser and console
  hosts are out of scope; SDK packaging and certification are not implemented.
- WebAssembly coverage is portable-library compilation only; see [platforms](platforms.md).
- Physical keyboard/controller/assistive-device behavior and portable-editor
  window interaction were not manually verified. OS-emulating devices use the
  standard paths; proprietary protocols require native adapters and device testing.
- Ordinary suites ignored 40 workspace and two example hardware-gated tests.
  Six selected GPU tests were run separately; other ignored hardware tests are
  unverified. CI configuration/static validation is not a hosted CI pass.
