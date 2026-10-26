# Pending engine features

Revision: 2026-10-26; starting main `91b961c2b7fa12e1114f8f45884eb1d15ccd0aee`.
Initial plan commit: `7d614e9`. Main contains accepted audio/server changes through
`ebc765c`. Larger branches are assembled in `codex/pending-integration`, currently
`6183242` with reviewed XR, animation graph and ML support. Nothing has been published this session;
final integrated checks remain pending.

This is the implementation record for the pending-feature request. Existing
spatial audio, first-round rendering, physics, UI and networking are the baseline.
Their remaining limitations still apply. This plan supersedes older plan prose
only for the pending work listed here; feature documentation remains authoritative
for implemented behavior. See [testing](testing.md), [architecture](architecture.md),
[rendering architecture](rendering-architecture.md), and [scripting](scripting.md).

## Goals and constraints

- Every requested feature has executable native/runtime behavior and focused
  tests; a schema, interface or mock alone does not establish implementation.
- The integrated revision passes `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets --locked -- -D warnings`, and
  `cargo test --workspace --locked`, plus the example checks in `testing.md`.
- Rendering changes have relevant real GPU readback evidence. Microphone, VR,
  playback timing and OS-specific behavior are reported separately from headless
  tests; unavailable checks remain unverified.
- Scripting receives an independent scoped security review, with reproducible
  findings fixed and tested before final integration.
- Reviewed commits are integrated without discarding existing work; final
  publication satisfies repository review/CI requirements.

The user set a limit of eight parallel workers for this session. The user also
confirmed that prior World and AI-gameplay managers are still running elsewhere.
They retain those areas and their existing worktrees. This chat owns Animation,
Editor, platform services, Audio and second-round Rendering. Do not resolve or
commit the other managers' dirty worktrees. Consume their completed revisions
after handoff. Root owns this record, cross-stream contracts and integration.

## Stages

### 1. World completion (external owner)

- Status: in progress.
- Objective: finish batch B and final checks for tilemaps, PCG, streaming,
  terrain, foliage, water and large-world coordinates.
- Existing revisions: parent `5ada8a3`; unchecked batch B `38d687f`.
- Owned paths: existing World manager's worktrees and World feature crates/docs.
- Acceptance: project PCG/streaming/tilemap regression tests, MCP tilemap tests,
  relevant GPU tests, and required integrated checks.
- Handoff: request/consume completed commits; historical 4,679-test evidence is
  not validation of batch B or the eventual combined revision.

### 2. AI-gameplay completion (external owner)

- Status: in progress.
- Objective: perception, EQS, smart objects, predicted abilities,
  inventory/quests/dialogue, Rhai, visual scripts and security review.
- Existing revision: `834c047` includes engine-graph, BT/ST and crowds.
- Owned paths: existing AI manager's worktrees, engine-ai/engine-graph and
  gameplay project adapters. Perception/EQS/smart-object drafts were dirty at
  inspection and remain with that manager.
- Acceptance: native and runtime behavior tests, prediction confirm/reject and
  replay tests, bounded script execution/adversarial tests, independent review.
- Handoff: downstream animation/editor consume committed engine-graph APIs;
  coordinate any graph registry changes at integration.

### 3. Audio streaming and media clock

- Status: in progress; depends on baseline only.
- Objective: bounded streamed PCM sources, pause/seek/underrun/EOF semantics,
  source clock epochs, incremental compressed decoding and cancellation.
- Owned paths: engine-audio and focused audio documentation/tests.
- Contract: preserve clip playback; callbacks do not decode or block. A global
  submitted-device-frame counter is not a per-source presentation clock.
- Acceptance: deterministic offline mixed output and clock/seek/backpressure
  tests; codec fixtures and malformed input limits; device checks separately.
- Outcome: bounded streaming/clock milestone accepted at `dc061859`, integrated
  as `d6823dc`; 326 audio tests, fmt and scoped Clippy passed. Compressed decoding
  is a separate in-progress slice using Symphonia 0.6.1; not yet validated.

### 4. Animation graph, blend spaces, IK and retargeting

- Status: in progress; depends on committed engine-graph foundation.
- Objective: executable pose graph and state transitions, 1D/2D blends,
  validated IK solvers/retarget maps and project runtime integration.
- Owned paths: engine-animation, animation-specific project modules/tests/docs.
- Contract: one authoritative pose writer per target; preserve physics/ragdoll
  ownership and morph-before-skin order.
- Acceptance: `cargo test -p engine-animation`, project pose/deformation tests,
  degenerate/unreachable chains, rest/proportion mapping, graph diagnostics.
- Outcome: graph/blend milestone `71cc1c4` accepted after independent source
  review, 125 focused tests, fmt and scoped Clippy. IK source and regression
  tests are ready for validation after an initialization-order review fix;
  retargeting remains pending. The graph milestone and committed AI foundation
  are assembled at `b62ef9e`; the external manager retains remaining AI work.

### 5. Editor scene recovery and graph authoring

- Status: in progress (scene validation and reusable graph canvas). The canvas
  uses the committed engine-graph foundation; shell wiring follows scene handoff.
- Objective: finish scene WIP `9e4a063`, multi-selection/gizmos/picking/settings,
  then reusable node/port canvas with graph-kind palettes, edits, undo and save.
- Owned paths: engine-editor, engine-editor-settings, editor docs/tests.
- Contract: scene edits use SceneDocument transactions; graph edits use
  engine-graph validation and atomic edits. No duplicate document semantics.
- Acceptance: real egui interaction tests, one undo per gesture, parented
  transforms, cancelled/stale gestures, graph persistence and validation.

### 6. Render integration seams

- Status: in progress (shared-shadow contract); depends on baseline and
  committed World rendering contracts for terrain/foliage integration.
- Objective: shared shadow targets/writer API, terrain and foliage casters,
  ordered UI overlays and final compositing including Frame Generation, and
  the two UI pipeline hooks. Establish scene-effects and temporal-resolve seams.
- Owned paths: engine-render-pipeline, world-render adapter, UI-render and DLSS
  integration; root serializes shared pipeline edits.
- Acceptance: fake/native caster GPU readback, actual terrain/foliage shadows,
  ordered UI pixels and compositing contract tests; hardware FG separately.

### 7. Platform services

- Status: in progress (video validation; XR, ML and server milestones accepted).
- Objective: review OpenXR `daa2888`, finish ML WIP `609ccb2`, implement
  existing self-hosted services and real video decode/playback. The user clarified
  that services means completing/validating the existing server, not adding cloud
  saves, achievements or leaderboards.
- Owned paths: platform feature crates/adapters and focused docs/tests.
- Depends on: stage 3 for video A/V clock integration. Existing CaptionClock
  and texture streams are reused; no independent drifting subtitle clock.
- Acceptance: XR lifecycle/graphics review and tests; real model fixture
  inference; loopback service behavior; decoded video textures plus audio-driven
  frame/caption scheduling, seek/pause/EOF and malformed-input tests.
- Outcomes: existing-server work accepted at `099a88`, integrated as `ebc765c`;
  fixed late authentication/expired-request admission before dispatch. Its 119
  tests (including five real QUIC/TLS CLI tests), fmt and scoped Clippy passed.
  XR repair `3e8c09a` resolves six reviewed defects; 39 unit tests, one compile-fail
  doctest, eight project tests, three Vulkan GPU tests, fmt and scoped Clippy
  passed. Integrated as `8e6701f`; combined workspace checks remain pending.
  Headset interoperability and macOS execution remain unverified. ML `8a7b6d4`
  is accepted and integrated as `6183242`: 13 core tests, two native adapter tests,
  scoped strict Clippy and fmt pass, including output-contract and stale-handle
  repairs. CPU inference is supported; GPU inference remains explicitly unavailable.
  Video implementation uses an explicit FFmpeg executable backend with a shared
  audio/caption clock; frame resampling and dependency requirements must be
  documented and tested before acceptance.

### 8. Remaining animation/editor/audio integrations

- Status: in progress (hot-reload tooling and morph targets); other slices remain pending and
  depend on relevant earlier stages.
- Objective: morph import/animation/deformation, cinematics timeline/sequencer,
  editor hot reload/tooling, procedural audio graph, microphone/voice codec,
  streamed voice playback and physics3D-reactive spatial audio.
- Owned paths: scoped assets/animation/project/editor/tooling/audio/voice modules.
- Contracts: reload preserves last-good running generation on candidate failure;
  procedural graphs compile outside callbacks; capture/voice use existing
  bounded networking APIs; physics audio reuses acoustic/spatial propagation.
- Acceptance: actual morph+skin vertices; deterministic timeline seek/events;
  successful and failed reloads; graph PCM; codec/capture lifecycle and voice
  loopback; collision-driven audio and collider acoustic geometry behavior.
- Handoff: hot reload prepares a candidate while the old game runs, then activates
  through the existing generation barrier. Tooling source is in progress; editor
  action wiring waits for scene-editor handoff to avoid concurrent ownership.
  Morph import and weight playback feed a single geometry-deformation writer
  before skinning; shared simulation registration waits for IK handoff.

### 9. Rendering round two

- Status: pending; depends on stage 6 and relevant World/material contracts.
- Objective: IBL/probes/lightmaps; SSR/DOF/motion blur; AA/vendor-neutral upscale;
  atmosphere/fog/clouds/decals/GPU particles; Hi-Z with LOD; HDR display output;
  material graph; 2D lighting/tilemap rendering.
- Owned paths: feature crates/shaders, pipeline integration and renderer docs.
- Contract: rendering and World share one documented tilemap render format;
  temporal consumers share valid depth/motion/history and reset semantics.
- Acceptance: per-feature GPU readbacks showing output differences and resource
  lifetime/resize behavior, CPU input validation, integration tests; HDR display
  presentation remains separate from encoding/math checks.

### 10. Final integration, review and publication

- Status: pending; depends on stages 1–9.
- Objective: combine reviewed commits, run required workspace/example checks,
  resolve cross-stream conflicts, align docs and publish validated work.
- Acceptance: goals above, required repository reviews/CI, clean owned changes,
  explicit hardware limitations and reconciliation of every task worktree.
- Outcome: not yet established. No incomplete stage is considered merged.

## Session worktree record

Managed worktree creations are delayed in the host's ignored-instruction scan.
Manual Git worktrees in the configured directory allow implementation to proceed.
Root must reconcile the unused managed creations when they return. Existing
external worktrees are not cleanup targets.

| Host name | Base | Assigned purpose | State |
| --- | --- | --- | --- |
| world-completion | `5ada8a3` | Unused; host cannot verify ownership for attachment | retained pending host recovery |
| gameplay-completion | `834c047` | Unused managed creation; registration incomplete | retained pending host recovery |
| audio-foundation | `91b961c` | Unused managed creation; registration incomplete | retained pending host recovery |
| animation-features | `834c047` | Unused managed creation; registration incomplete | retained pending host recovery |
| codex-audio-streams | `91b961c` | Audio source/clock; `codex/audio-streams` | active |
| codex-animation-graph | `834c047` | Graph/blends; `codex/animation-graph` | active |
| codex-render-contracts | `91b961c` | Shared shadows; `codex/render-contracts` | active |
| codex-pending-integration | `697e36f` | Root integration and final checks; `codex/pending-integration` | active |
| parity-editor-scene | `9e4a063` | Resume scene WIP; existing `claude/parity-editor-scene` | active, retain |
| parity-editor-hotreload | `319c7df` | Runtime generation replacement; existing `claude/parity-editor-hotreload` | active, retain |
| parity-editor-graph | `b62ef9e` | Reusable graph authoring; existing `claude/parity-editor-graph` | active, retain |
| parity-animation-morph | `b62ef9e` | Morph import/playback/deformation; existing `claude/parity-animation-morph` | active, retain |
| parity-platform-services-ml | `609ccb2` | Complete ML; existing `claude/parity-platform-services-ml` | integrated, retained for existing owner |
| parity-platform-services-services | `7d614e9` | Existing server; existing `claude/parity-platform-services-services` | integrated, retained for existing owner |
| parity-platform-services-xr | `daa2888` | Reviewed repairs; existing `claude/parity-platform-services-xr` | integrated, retained for existing owner |
| parity-platform-services-video | `dc061859` | Video/audio/caption integration; existing `claude/parity-platform-services-video` | active, retain |

Manual and reused paths are `D:/Git-Worktrees/Rust-Game-Engine/<name>`; managed
paths are `D:/Git-Worktrees/<name>/Rust-Game-Engine`. Root creates task branches,
accepts handoffs, integrates and retires task-created checkouts. Reused paused
worktrees are preserved for their existing owners. Every worker reports exact
tested SHA, logs, residuals and stopped processes.

Baseline evidence: fmt and workspace Clippy passed on `91b961c`. The workspace
test compilation was interrupted after concurrent builds exhausted Windows
virtual memory (OS error 1455); no passing workspace-test claim is made. Logs live
in `target/task-baseline`. The required suite will run on the integrated revision.
All heavy validation now uses one root-scheduled build slot with `-j1`; source
work remains parallel within the eight-worker limit. Branch test results do not
replace final integration checks.
