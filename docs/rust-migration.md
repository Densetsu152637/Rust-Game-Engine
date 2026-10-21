# Rust migration

## Scope and sequence

This is an active migration, not a completion report. Rewrite the standalone
engine in this repository first; only after engine acceptance migrate Unsealed's
game scripts and update its pinned engine revision. Mobile support is removed.
Existing desktop capabilities remain acceptance requirements. Narrative/game
features that Unsealed only documents as future work are not new requirements.

Starting engine revision: `791f28fe818057b971adae1e4326e44b75069efd`.
Starting Unsealed revision: `06fcbe857077363a34c6a7f4afbcfe793057b051`, consuming
engine `e110e846600133db058a32048df1adcada8126d6`.
At intake, engine `mobile/` files and `src/components/alias.h` were already
deleted locally; preserve these changes and keep their ownership explicit.

## Decisions

- Replace EnTT with library-owned Rust ECS storage, queries, change tracking,
  deferred commands, and parallel dependency scheduling. Use native component
  types/newtypes and bundles instead of duplicating generic ECS storage.
- Replace walltime batching with explicit system ordering and access-aware
  scheduling. Specify command publication and simulation/render visibility;
  reject invalid graphs rather than silently misordering work.
- Replace GLFW with a desktop Rust window/event library and use a Rust GPU
  backend. Preserve real rendering, device errors, resize and shutdown behavior.
- Compile external Rust game modules on demand with Cargo/rustc into a native
  runtime generation containing both engine and game code. All public native
  APIs, component types, resources, system labels and rendering extension points
  share one Rust type universe. Do not pass Rust objects across independently
  compiled DLL boundaries. This is runtime native compilation, not an interpreter
  or a claim of seamless in-process compiler JIT patching.
- The compiler/launcher owns compilation diagnostics and process lifecycle;
  gameplay calls are ordinary native Rust calls, not a restricted RPC surface.
  A failed candidate build must not destroy a working generation.
- Retain authored asset/entity IDs separately from generation-aware ECS handles.
- Remove the superseded C++/Lua/mobile build paths once their desktop replacements
  have passed acceptance. Do not retain a second implementation as the final design.

The selected libraries are standalone Bevy ECS (rather than the full Bevy app),
winit and wgpu. Versions are pinned only after successful registry resolution and
compilation. Bevy owns archetypes, access-aware schedules and queries; wgpu owns
GPU resources and backend synchronization. Relevant primary references:
[Bevy ECS](https://docs.rs/bevy_ecs/latest/bevy_ecs/),
[wgpu](https://docs.rs/wgpu/30.0.1/wgpu/),
[winit event-loop ownership](https://docs.rs/winit/0.30.13/winit/event_loop/struct.EventLoopBuilder.html),
[Rust ABI](https://doc.rust-lang.org/reference/items/external-blocks.html#abi).

### Implementation ownership

Core is the first implementation writer on the primary checkout. Other streams
start from plan commit `72ca433` and receive the committed core before integrated
compilation. Root owns shared integration, the final lockfile and retirement of
these task-created worktrees. Each worktree is inside the standalone engine's
ignored `build/rust-worktrees/` directory, never inside Unsealed's submodule.

| Stream | Owned paths | Branch / worktree suffix |
| --- | --- | --- |
| E3 core | Root Cargo files, toolchain, `crates/engine-core/` | `main`, primary |
| E4 authored runtime | `crates/engine-project/` | `codex/rust-project`, `project` |
| E5 compiler and tooling | `crates/engine-tooling/` | `codex/rust-tooling`, `tooling` |
| E6 desktop graphics | `crates/engine-desktop/` | `codex/rust-desktop`, `desktop` |
| E7 audio | `crates/engine-audio/` | `codex/rust-audio`, `audio` |
| E8 native editor | `crates/engine-editor/` | `codex/rust-editor`, `editor` |
| E9 native examples (after E7) | `examples/`, applicable `assets/scripts/` | `codex/rust-audio`, `audio` reused |
| E10 MCP | `crates/engine-mcp/` | `codex/rust-mcp`, `mcp` |

Documentation/CI migration and independent acceptance are queued until their
public interfaces exist. No stream may remove an existing
capability merely because its replacement has not yet been implemented.

## Implementation and acceptance gates

1. **Core foundation:** Cargo workspace, pinned dependencies/toolchain, public ECS
   and app API, lifecycle, deterministic fixed stepping, graph scheduling,
   structural-command visibility, typed custom component/resource/bundle/system
   examples, change-driven render extraction, and behavioral tests.
2. **Authored runtime:** project/scene validation and root-confined assets, stable
   IDs, input snapshots/rebinding/focus, collision and triggers, scene transitions,
   sprites/cameras/animation, modal UI, audio ownership, save/settings migrations,
   atomic persistence and failure-path tests.
3. **Desktop:** real textured geometry and sprite/font rendering, shared resources,
   public render jobs, logical viewport/pixel snapping, window/input lifecycle,
   native audio playback with offline mixer checks, device error propagation.
4. **Tooling:** runtime Rust compilation and invalid-build diagnostics, reproducible
   generations, CLI machine results, project initialization/validation/assets/
   shaders/package, scene inspection, MCP discovery and read-only tools, native
   authoring editor with undo/redo/save/reload and isolated play/stop.
5. **Engine integration:** port engine examples and walkthroughs, test the complete
   revision, exercise external custom Rust components and rendering jobs, verify
   packages from a different working directory, align docs and desktop CI, then
   remove obsolete implementations/dependencies. Report GPU/native UI/audio and
   hosted CI evidence separately from headless tests.
6. **Unsealed (blocked on gate 5):** convert its sole Player Lua module to external
   Rust; preserve the one authored entity, source mesh/texture and action names;
   verify no-input, cardinal, opposite and normalized diagonal movement, preserved
   Z, deterministic fixed stepping, visible launch and package launch; update docs
   and pin the accepted standalone engine commit.

## Parity inventory

The source inventory at the starting revision found approximately 39,254 lines
in 183 C++ files, including 7,388 test lines. This bounds migration coverage; it
is not a Rust size target.

| Capability | Existing source owners | Acceptance focus |
| --- | --- | --- |
| ECS and scheduling | `src/ecs/` | Identity, arbitrary components/aliases, include/exclude/dirty queries, bundles/archetypes, shared/buffered data, schemas, deferred mutation, graph visibility |
| App lifecycle | `src/core/`, `src/runtime/` | Startup/shutdown, bounded ticks, cancellation, host-thread presentation, errors |
| Authored projects | `src/project/project.*` | Manifest/scene diagnostics, confinement, stable IDs, revisioned edits, atomic save, undo/redo |
| Gameplay | `src/project/runtime.*`, `gameplay2d.*` | Movement, collision, masks, deterministic contacts/triggers, safe positions, follow/flock, transactional rooms/travellers |
| Input and UI | `src/interaction/`, `src/project/gameplay_ui.*` | Snapshots, rebinding, pointer/wheel/gamepad, focus loss, modal capture, scrolling/clipping, UTF-8 |
| Rendering | `src/rendering/`, `src/vulcan/` | Shader/mesh/texture/uniform contracts, batching, resource lifetime, bounded queues, actual GPU drawing, sprite/font/camera behavior |
| Audio | `src/audio/` | WAV, offline mix and playback, buses/voices/pitch/loop/pause, spatial listeners/sources and event ownership |
| Persistence | `src/project/persistence.*` | Separate saves/settings, versioned migration, backup recovery, bounded JSON, durable atomic writes |
| External scripting | `src/scripting/` | Full native public API, lifecycle, candidate validation, custom schemas/systems and ownership cleanup |
| Iteration/packaging | `src/tooling/` | Stable asset index/cache, compile-only validation, shaders, confined export, source/assets/runtime/licenses, relocatable launch |
| Editor | `src/editor/` | Native open/save/reload/undo/redo, selection/inspector/asset pickers, validation/logs, unsaved protection, play isolation |
| CLI and MCP | `src/commands.cpp`, `src/automation/` | JSON results/errors, deterministic injected input, bounded stdio JSON-RPC, negotiation/discovery/path confinement |

## Evidence and outstanding work

Initial source inventories are static evidence only. Rust 1.99.1 and Cargo are installed at
`C:\Users\Nicholas\.cargo\bin` but are not initially on this shell's PATH;
Visual Studio C++ build tools are installed. Use explicit tool paths/environment.

Root owns integration, acceptance, publication and cleanup. Independent writers
use separate worktrees; shared manifests/lockfile/API changes have one owner.
Record actual test commands/results and remaining gaps here as gates progress.

### Core milestone: accepted

Commit `a6a006784d18ea12b5309d2e4473b1e5e996b00a` adds `engine-core` and
the workspace. The core worker ran `cargo check -p engine-core`,
`cargo test -p engine-core` (11 passed), `cargo fmt --all --check`, and
`cargo clippy -p engine-core --all-targets -- -D warnings` successfully. Root
reviewed the resulting implementation and test coverage before accepting this
bounded milestone. All subsystem worktrees received that commit.

Tests cover native types/bundles/labels, deferred command visibility, conflicting
or cyclic schedules, fixed ticks/lifecycle, old/new buffered visibility, dirty
render extraction, component removal, stale entity generations and phase-boundary
stop requests. Empty or cross-schedule set references retain Bevy's documented
limitations; this wrapper does not invent a parallel graph validator. Native
system panics remain process-level failures for the supervisor to report.

This is not complete engine acceptance: authored runtime, devices, tooling,
external compilation, examples, editor, removal and Unsealed gates remain open.
Builds currently use `RUSTUP_TOOLCHAIN=stable` selecting the installed, verified
1.99.1 compiler, and the ignored `build/cargo-home` registry cache because the
default Cargo home is not writable in the sandbox. Targets remain per checkout.

### Audio milestone: accepted, runtime integration pending

Worker commit `d1085b86d3f6333134c1768743f6f354071c8839` was integrated on
main as `ea1f168`. `engine-audio` uses CPAL for native output and Hound for WAV
sample decoding. Engine-owned deterministic mixing, source ownership and event
policy remain explicit; it does not introduce a second library mixer alongside
the existing policy. The crate README records the dependency decision.

The audio worker passed formatting, Clippy with warnings denied, ten offline
behavior/fault tests and the explicit native device test on the final source.
That test submitted 7,842 stereo frames, consumed clip PCM and shut down without
reported device errors. This is device activity evidence, not audible-quality or
physical-disconnect verification. Root reviewed the crate documentation and test
coverage before integration. Scene attachments, listener ownership and desktop
shutdown ordering still require integrated runtime acceptance.

### Current integration checkpoint

This checkpoint is for continuing the active rewrite, not a completion claim.
The task is the user-created goal in chat `01a15cff-55de-7ad2-ba80-39a49eb7a02d`.
The user explicitly selected **heavy** and then authorized removing mobile
features. Both assigned repositories began on `main`; the custom workflow's
starting-main publication rule applies, subject to checks and host permissions.
No task commits have been pushed at this checkpoint. Unsealed remains unchanged.

Primary engine is `D:\Git Repositories\CPP-Game-Engine`, currently `main` at
`443c87d` (before this checkpoint's commit). Integrated milestones:

- Core `a6a0067`: accepted as described above.
- Audio `ea1f168`: accepted component; scene/device integration still covered later.
- Authored runtime `98047f2`, then helpers/audio/lifecycle `aa000d2`: worker passed
  all 25 project tests and Clippy. Followup audit is correcting old persistence
  profile/path/version/backup behavior before final parity acceptance.
- Native compiler `a243827`: eight actual external-build/process tests passed,
  plus the native custom-component/system proof reran on the imported project
  dependency graph. Compilation, diagnostics, startup barrier, graceful shutdown,
  owned process-tree cancellation and last-good retention work. CLI/runtime entry,
  packaging and compiler-backed editor play are still being implemented. An
  external package's ancestor Cargo workspace/config fingerprint edge remains
  assigned for correction.
- Native editor `c8847a3`: five controller tests, format, Clippy and actual bounded
  Win32 control smoke passed. Real compiler-backed play remains an integration gate.
- Desktop `443c87d`: all-target compilation passes. Real GPU pixel/window tests
  and final lint/behavior acceptance are running; this is a compile checkpoint.

Active child assignments (canonical names under `/root`):

- `engine_inventory`: E4, project runtime fixes in `project` worktree. Owns only
  `crates/engine-project/`; persistence compatibility/failure-path patch next.
- `consumer_inventory`: E5, compiler/CLI/runtime entry/assets/package/editor
  PlayHost in `tooling`; owns only `crates/engine-tooling/`.
- `desktop_render`: E6, real GPU/window/input/audio-runner acceptance in `desktop`;
  owns `crates/engine-desktop/` and keeps PNG/log evidence under its ignored build.
- `audio_runtime`: E9, engine native examples and full walkthrough conversion in
  reused `audio` worktree. Audio implementation is complete; now owns examples.
- `rust_architecture`: E11, read-only independent critique of project/compiler
  contracts; core/editor authoring is complete. Its `editor` worktree is retained
  until play integration is accepted, then root retires it.
- `mcp_port`: E10, independent read-only MCP protocol implementation in `mcp`;
  owns `crates/engine-mcp/`, transcript tests and crate docs.

Six active child slots are a monitored allowance, justified by independent
subsystems/protocol verification. Astra high is selected for architecture,
graphics and consequential critique; Sol high for bounded subsystem execution.
Heavy's general and applicable research ceiling is Frontier; no numeric budget
was requested. Root owns all integration, shared lockfile, publication, legacy
removal and task-worktree retirement. Do not cherry-pick a worker's prerequisite
commits twice: root propagates only the owned feature commits to dependent trees.
Before changing a worker index, coordinate a pause of its staging/commit and
verify the index is empty. Local generated lockfiles in worktrees are deliberately
uncommitted; preserve them until checks finish, then retire with the task worktree.

The authoritative instruction entry is
`D:\Git Repositories\Agent-Custom-Workflow\AGENTS.md`; root owner is
`workflows/AGENTS.root.md` relative to that root. Applicable owners loaded at intake
include root delegation/planning/recovery; manager/delegation/controls; architecture
and research/dispatch; coding standards/conventions, validation, Git usage,
worktrees/publication, subagent coordination, GitHub Actions and usage accounting.
There were no unresolved instruction-read gaps after bounded recovery. On context
recovery load their applicable authoritative text again as required; this record
is a locator, not replacement instructions or new authority.

Next gates: finish subsystem fixes and real-device tests; integrate examples,
CLI/MCP/editor play/relocatable package; independent critique and combined checks;
align root docs and desktop CI; remove superseded C++/Lua/mobile/dependency paths;
validate/publish the standalone engine; only then migrate Unsealed and its engine
pin. Its sole existing script is normalized 2 m/s Player movement; planned story,
quests and other narrative features are not new implementation scope.
