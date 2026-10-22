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

This checkpoint continues the active rewrite; it is not a completion claim until
hosted CI passes. The user selected **heavy**, authorized removing mobile, and
asked for CI with 1000+ behavioural tests and a pull request iterated until its
checks pass (rather than a direct push to `main`). Instruction entry points:
`D:\Git Repositories\Agent-Custom-Workflow\AGENTS.md` and
`workflows/AGENTS.root.md`.

Integrated on local `main` (all streams cherry-picked by root after review):

- Examples and sample script ported from Lua to external Rust crates
  (`f5be46d`); all `.lua` files removed. Example crates are standalone workspace
  roots with ignored lockfiles.
- Behavioural suites: engine-core 467 tests (graph scheduling, deferred commands,
  determinism, timestep, lifecycle); engine-project ~1,400 (manifests,
  confinement, gameplay, input/UI, persistence); audio 260, desktop 320, MCP 181,
  editor 85; examples ~101. GPU, audio-device and interactive Win32 tests remain
  `#[ignore]`.
- Fixes found by the suites: fractional viewport fit (`d4be883`), trailing
  whitespace wrapping (`885ca72`), transient Windows atomic-rename refusal
  (`a8b368c`).
- Legacy C++, Lua, mobile, vendored third-party code, GLSL shaders and C++ CI
  removed (`885efc1`); docs rewritten for the Rust engine (`d8b9a47`); Rust CI
  with a 1000-test gate (`35b927f`).
- `engine` CLI, runtime entry, packaging and editor play host (`2d53abc`):
  ancestor Cargo workspace/config isolation fixed, last-good generation retained
  on failed builds, a process-wait hang fixed.
- Workspace lockfile regenerated (`bf210aa`).

`engine-core` advances exactly one fixed step per `tick`; frame pacing lives in
the desktop runner (one 1/60 s tick per redraw, resynchronized after more than
250 ms behind) rather than in an engine accumulator.

Unsealed: the Player script is ported to the external Rust crate
`unsealed-gameplay` on Unsealed branch `codex/rust-port` (`224f0c9`, 24 tests,
CI workflow). Its engine submodule pin is updated only after the engine branch is
pushed.

Next gates: combined CI-equivalent validation on the integrated revision; push
the engine branch and open its PR; iterate until hosted checks pass; pin that
engine commit in Unsealed and open its PR; retire task worktrees under
`build/rust-worktrees/` and Unsealed's `build/worktrees/`. Hosted GPU, visible
window, audio-device and Linux desktop behaviour remain unverified by automation.
