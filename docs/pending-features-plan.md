# Pending engine features

Revision: 2026-10-26; starting main `91b961c2b7fa12e1114f8f45884eb1d15ccd0aee`.
Initial plan commit: `7d614e9`. Main contains accepted audio/server changes through
`ebc765c`. Larger branches are assembled in `codex/pending-integration`, currently
`3b4f289` before this plan update. Validated milestones include XR, animation graph/IK/morphs, ML, scene
editing/hot reload, shared shadows, compressed decoding and video. The branch also
contains source-reviewed retargeting, procedural/authored audio, voice, sequencer,
UI presentation, material graphs, editor graphs/settings and physics-audio work
for combined validation. Those source-only milestones remain unaccepted until
execution checks pass. Nothing has been published; final checks remain pending.

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
World is handed off and the AI manager has now stopped; Codex continues scripting in its own worktree. Existing Claude worktrees remain with their owner. This chat owns Animation,
Editor, platform services, Audio and second-round Rendering. Do not resolve or
commit the other managers' dirty worktrees. Consume their completed revisions
after handoff. Root owns this record, cross-stream contracts and integration.

## Stages

### 1. World completion (external owner)

- Status: done.
- Objective: finish batch B and final checks for tilemaps, PCG, streaming,
  terrain, foliage, water and large-world coordinates.
- Handoff revision: `255035b`, including checked batch B `38d687f`.
- Outcome: integrated into clean local main `598e395`; the handoff is a verified
  ancestor. Merged main passes formatting, strict workspace Clippy and the full
  locked workspace suite: 5,015 passed, zero failed, 153 ignored. MCP execution
  passes 14 runnable tests. Vulkan on the RTX 5080 passes all 11 World GPU tests
  present in the source and the separate desktop default-render test. No World
  renderer file differs from the handoff revision.
- Networking and World registrations are both retained. Recomputed CLI totals:
  read 31, write-enabled 62, exec 34, write+exec 65.
- Cleanup: the Claude manager retains its worktree/branch retirement. Root has
  reported the verified main ancestry. No push has occurred.
- Followups: shared-shadow adoption is stage 6; the later combined branch still
  needs its own execution gates. Deferred World limits remain documented.
### 2. AI-gameplay and scripting completion

- Status: in progress; completed gameplay handed off, scripting transferred to Codex.
- User handoff: Claude stopped at its weekly usage gate. Clean revision
  `d88464d` contains crowds, graph/BT/ST, perception, EQS, smart objects,
  predicted abilities, inventory, quests and dialogue. The author reports focused
  fmt/Clippy/tests passed; combined Codex validation remains required.
- Root-owned continuation: `codex/ai-scripting`, starting from combined `ee57d57`,
  merges the completed AI handoff and implements saved scripting slices V0–V4
  and T1–T2. Claude worktrees and branches remain with their existing owner.
- Design source: `_records/ai-gameplay-script-design.md` in the external worktree
  record directory. Verify its explicitly unconfirmed Rhai APIs and prefab path.
  Use the shared graph crate, typed capability checks, bounded execution,
  transactional built-in writes, deterministic events and last-good reload.
- Acceptance: actual visual and Rhai runtime behavior, adversarial resource and
  capability tests, independent scripting security review with fixes, default and
  no-text builds, strict lint and the final combined workspace gate.
- Ownership: scripting manager has three slots including itself within the user
  limit of eight. Root schedules all heavy builds and owns final integration.
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
  milestone `7b5fddd` uses Symphonia 0.6.1 for WAV/FLAC/Vorbis/MP3 and is integrated
  as `b41c636`. Its 335 tests, scoped strict Clippy and fmt passed after independent
  review fixes. Authored streamed/procedural sources are source-reviewed at
  `5cdf40c`, including durable worker failures and shared emitter integration;
  native audio execution passed 355 tests (two hardware/cost tests ignored) at
  integrated revision `775fb33`; project runtime checks remain in progress.

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
  review, 125 focused tests, fmt and scoped Clippy. IK `a70064d` is accepted and
  integrated as `164b9ed`: 141 focused tests, scoped strict Clippy and fmt passed,
  including the reviewed initial-control repair. Retargeting `ee8192d` passed
  source review and is assembled for validation. The first combined compile found
  a stale joint-field reference; `9839386` corrects it to the target loop index.
  All 28 native animation tests, including retargeting, pass at `775fb33`;
  project retargeting checks remain in progress. The graph milestone and committed AI foundation
  are assembled at `b62ef9e`; the external manager retains remaining AI work.

### 5. Editor scene recovery and graph authoring

- Status: in progress (scene accepted; reusable graph canvas validation pending). The canvas
  uses the committed engine-graph foundation; shell wiring follows scene handoff.
- Objective: finish scene WIP `9e4a063`, multi-selection/gizmos/picking/settings,
  then reusable node/port canvas with graph-kind palettes, edits, undo and save.
- Owned paths: engine-editor, engine-editor-settings, editor docs/tests.
- Contract: scene edits use SceneDocument transactions; graph edits use
  engine-graph validation and atomic edits. No duplicate document semantics.
- Acceptance: real egui interaction tests, one undo per gesture, parented
  transforms, cancelled/stale gestures, graph persistence and validation.
- Outcome: scene `7d215a7` accepted and integrated as `f52cf2f`; 113 tests, scoped
  strict Clippy and fmt passed after picker review fixes. Generic graph canvas and
  actual editor shell wiring plus typed MCP settings are source-reviewed at
  `bea8c12` and assembled for combined validation. All six native graph-editor
  tests pass at `7fc0c85`, including real egui palette, drag, wiring, delete and
  undo gestures. Combined shell and MCP settings tests remain pending.

### 6. Render integration seams

- Status: in progress (shared shadows accepted; UI seams source checkpoint); depends on baseline and
  committed World rendering contracts for terrain/foliage integration.
- Objective: shared shadow targets/writer API, terrain and foliage casters,
  ordered UI overlays and final compositing including Frame Generation, and
  the two UI pipeline hooks. Establish scene-effects and temporal-resolve seams.
- Owned paths: engine-render-pipeline, world-render adapter, UI-render and DLSS
  integration; root serializes shared pipeline edits.
- Acceptance: fake/native caster GPU readback, actual terrain/foliage shadows,
  ordered UI pixels and compositing contract tests; hardware FG separately.
- Outcome: shared-shadow API `0aba854` integrated as `f56981d`; 42 CPU and 17 real
  Vulkan tests, scoped strict Clippy and fmt passed. World is merged. Terrain/foliage shared-atlas adoption is now being implemented in `codex/world-shadows`; water casting is opt-in opaque depth, disabled by default. Casters must cover every allocated light tile, preserve off-camera casters and avoid advancing motion/LOD histories in Shadow. Ordered overlays, Composite ping-pong and late
  UI replay across generated frames are source-reviewed at `ca609ef`, including
  lazy RT registration and disabled-hook repairs; execution remains pending.

### 7. Platform services

- Status: in progress (XR, ML, server and video milestones accepted; final combined checks pending).
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
  Video `cc4e0b8` is accepted and integrated as `2bdf9bb`: 97 focused tests, scoped
  strict Clippy and fmt passed, using real FFmpeg/ffprobe 7.0.2. It uses explicit
  host executables, 30fps CPU RGBA, shared audio/caption timing and 3D material
  texture streams. Sprite texture-stream integration and hardware presentation
  timing remain unverified/outstanding; no audible-device claim is made.

### 8. Remaining animation/editor/audio integrations

- Status: in progress (hot reload and morph accepted; native audio, voice and sequencer tests pass); runtime integration checks remain and
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
  through the existing generation barrier. Hot reload `60cb571` is accepted:
  final checks passed 10 lifecycle cases, identity deadline and inherited-pipe
  regressions, actual engine reload acceptance, strict Clippy and fmt. It also
  repairs canonical Windows private-save paths; five older symlink cases lacked
  privileges, while real junction rejection passed. Build artifacts retain the
  existing explicit-cleanup policy. Morph `93cf862` is accepted and integrated as
  `8d9b88b`: 51 focused tests and strict Clippy/fmt passed, including actual morph
  deformation before skinning and graph/IK compatibility.
  Sequencer source is assembled at `d422209`; seven native tests pass at `7fc0c85`. It runs before
  simulation and native scrubbing must immediately refresh skin deformation.
  Procedural audio `5cdf40c` passed source critique and all 355 audio native tests,
  including DSP/producer failure tests. Authored runtime validation remains pending. These source checkpoints are
  incomplete milestones, not accepted features.
- New source decisions: atomic procedural reset controls are source-reviewed at
  `9f7a312`. Extend the existing AudioEmitter/AudioSources lifecycle for resident
  compressed clips, file streams and graphs; do not create a second emitter.
  A separate engine-voice-audio bridge owns explicit CPAL capture, fixed-profile
  Opus and existing network receiver-to-PCM playback. Capture construction is
  device-free; hardware tests remain opt-in and separate from synthetic ingress.
  Voice source `a9f68cb` includes reviewed capture recovery, encoder-state purge,
  drained-speaker cleanup and mute bounds. Verified portable CMake and existing
  MSVC built and linked native Opus successfully. Nine voice tests pass at
  `7fc0c85`, including actual codec-to-mixer playback, PLC, capture conversion and
  lifecycle behavior. The microphone test remains ignored.
  Sequencer shared integration is source-reviewed at `d422209`, including early
  scheduling, immediate deformation refresh and startup/transition cleanup;
  native compilation and tests pass; project runtime tests remain in progress.
  Physics/audio source `d705f174` passed review: accepted targeted 3D contact events
  reach audio after pose synchronization; optional box-collider acoustic geometry
  honors mask/material/mobility and removal. Other collider-only shapes are
  explicitly unsupported; existing rendered-model geometry remains available.

### 9. Rendering round two

- Status: in progress (material graph native tests pass; Hi-Z kernel source underway); remaining effects depend on
  stage 6 and relevant World/material contracts.
- Objective: IBL/probes/lightmaps; SSR/DOF/motion blur; AA/vendor-neutral upscale;
  atmosphere/fog/clouds/decals/GPU particles; Hi-Z with LOD; HDR display output;
  material graph; 2D lighting/tilemap rendering.
- Owned paths: feature crates/shaders, pipeline integration and renderer docs.
- Contract: rendering and World share one documented tilemap render format;
  temporal consumers share valid depth/motion/history and reset semantics.
- Acceptance: per-feature GPU readbacks showing output differences and resource
  lifetime/resize behavior, CPU input validation, integration tests; HDR display
  presentation remains separate from encoding/math checks.
- First independent slice: engine-material-graph compiles shared engine-graph
  documents to the existing Mesh/Sprite material shader contract. Reuse the
  existing shader field for authored graph assets. Root serializes graph registry
  and runtime material adapters. The compiler must validate one output sink with
  reverse reachability, not GraphKind's forward single-root rule.
  Source `725e545` and its mesh/sprite authored adapters passed review; six native
  tests pass at `7fc0c85`. Authored runtime and four real GPU tests remain pending. Distinct graph-color/physical-control regressions
  cover the reviewed unlit-color repair.
- Shared pipeline prerequisites: one owner must add render-size scene-colour
  ping-pong after opaque/sky and before transparents, adapt every later consumer
  including DLSS, and define per-view temporal success/abort/reset/forget hooks.
  Existing Prepass screen lighting cannot provide same-frame SSR. Shaded normal,
  roughness and F0 need real producers before SSR; RT currently reconstructs
  positions and uses a zero roughness input.
- Shared-seam design checkpoint (source `8afd976`, implementation pending): add
  an ordered `OpaqueEffects` stage between Sky and Transparent, with one lazy
  render-size HDR scratch texture. `scene_color()` is authoritative for every
  later attachment, host upscale input, native upscale and readback. Each effect
  obtains distinct source/destination once and writes the full image.
  Commit camera, staged mesh models and participating feature histories together
  after scene encoding/submission through Tonemap, before Overlay/Composite.
  Aborted scene attempts discard pending CPU histories and reset the next attempt;
  late presentation failure retains the committed scene. Add CPU-only reset,
  finish and forget callbacks, unique per-view evaluation IDs, and adapt existing
  RT denoiser/auto-exposure/SR/FG bookkeeping. A submission is not GPU completion.
  Test noncommuting color passes followed by transparency on every upscale route,
  aborted-frame camera/model consistency, and late-UI retries without repeating
  scene evaluation or advancing FG twice. One owner edits all shared seam files.
- Independent source preparation underway: `codex/render-hiz` owns a conservative
  forward-Z MAX pyramid and bounded rectangle queries only. Odd-sized mip levels
  must cover every source texel; invalid/uncertain queries stay visible. Actual
  LOD visibility consumption remains pending. `codex/render-post-effects` owns
  native depth-of-field and motion-blur features on existing output-size post I/O;
  authored settings and real GPU validation follow native source handoff.
- Dependency order after those prerequisites:
  1. IBL uses prefiltered specular cubemaps, diffuse SH and a BRDF LUT; local
     probes add bounded deterministic selection/blending. Validate constant
     environment response, roughness, orientation and volume transitions.
  2. Lightmaps add real secondary UVs and per-instance atlas transforms, with
     explicit probe-diffuse replacement/addition. Test separate atlas regions on
     shared geometry. Supplied baked-data consumption does not establish baking.
  3. SSR uses current opaque colour/depth/material data, bounded tracing and IBL
     misses; define RT-versus-SSR selection. DOF and motion blur can use existing
     output-size post I/O, with depth-aware taps and correct motion scaling.
  4. Spatial AA precedes a native WGSL temporal resolve/upscaler. One owner supplies
     jitter and one Upscale winner runs; test disocclusion, cuts, size changes,
     re-enable and mode switching. Do not label a native algorithm as FSR/XeSS.
  5. Atmosphere, fog and clouds are separate executable scattering/extinction/
     volume-rendering slices; GPU tests must show sun, density, depth and wind
     effects. Decals use depth-projected volumes and explicit material response.
  6. GPU particle simulation runs once per scene tick, not once per view or FG
     presentation. Read back state/counts against a seeded CPU reference before
     accepting rendered output and bounded overflow behavior.
  7. Wire real LOD selection/hysteresis into submission, then current-frame Hi-Z
     max-depth mips and actual visibility/indirect draw consumption. A visibility
     buffer without skipped draws is incomplete; camera culling keeps shadows.
  8. HDR requires OS/backend display colour-space support and compatible output,
     UI paper-white and FG encoding; float intermediates alone are insufficient.
  9. Reuse World's committed tile chunk geometry/Draw contract from `255035b`:
     16x16 chunks, y-up local positions, v-down oriented atlas UVs, stable chunk
     identity/revision and layer opacity. Add real lit sprite/tile shaders and
     bounded 2D light inputs while preserving sprite interleaving and UI rules.

### 10. Final integration, review and publication

- Status: pending; depends on stages 1–9.
- Objective: combine reviewed commits, run required workspace/example checks,
  resolve cross-stream conflicts, align docs and publish validated work.
- Acceptance: goals above, required repository reviews/CI, clean owned changes,
  explicit hardware limitations and reconciliation of every task worktree.
- Outcome: not yet established. Source-only assembly in the isolated integration
  branch does not count as an accepted feature or final merge. Combined native
  checks use one root-owned build slot and a dedicated cache per checkout; never share Cargo target directories across main and integration after observed stale dependency metadata;
  required project, editor, MCP, GPU and final workspace/example gates follow.

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
| codex-audio-streams | `b41c636` | Procedural audio; `codex/audio-procedural`, old branch retained at `7b5fddd` | active |
| codex-animation-graph | `834c047` | Graph/blends; `codex/animation-graph` | active |
| codex-render-contracts | `91b961c` | Shared shadows; `codex/render-contracts` | active |
| codex-pending-integration | `697e36f` | Root integration and final checks; `codex/pending-integration` | active |
| codex-material-graph | `2ebdfcd` | Material compiler; `codex/material-graph` | active |
| codex-voice-audio | `2ebdfcd` | Capture/Opus/playback bridge; `codex/voice-audio` | active |
| codex-physics-audio | `5cdf40c` | 3D contacts/collider acoustics; `codex/physics-audio` | preserved at `d705f174`, validation pending |
| parity-editor-scene | `9e4a063` | Resume scene WIP; existing `claude/parity-editor-scene` | integrated, retained for existing owner |
| parity-editor-hotreload | `319c7df` | Runtime generation replacement; existing `claude/parity-editor-hotreload` | integrated, retained for existing owner |
| parity-editor-graph | `b62ef9e` | Reusable graph authoring; existing `claude/parity-editor-graph` | active, retain |
| parity-animation-morph | `b62ef9e` | Morph import/playback/deformation; existing `claude/parity-animation-morph` | integrated, retained; root combined build uses declared cache |
| parity-animation-cinematics | `f52cf2f` | Timeline source; existing `claude/parity-animation-cinematics` | preserved at `7b96bb9`, retain |
| parity-platform-services-ml | `609ccb2` | Complete ML; existing `claude/parity-platform-services-ml` | integrated, retained for existing owner |
| parity-platform-services-services | `7d614e9` | Existing server; existing `claude/parity-platform-services-services` | integrated, retained for existing owner |
| parity-platform-services-xr | `daa2888` | Reviewed repairs; existing `claude/parity-platform-services-xr` | integrated, retained for existing owner |
| parity-platform-services-video | `dc061859` | Video/audio/caption integration; existing `claude/parity-platform-services-video` | integrated, retained for existing owner |

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

## Current integration and rendering execution checkpoint

Assembled revision `3b4f289` contains World, native Hi-Z `9895e8`, depth/motion
post-effects `71a0eb`, and indirect-lighting resources `d56e751`. These three
rendering slices passed independent source review; CPU/GPU execution remains
pending. No authored IBL/lightmap or post-effects integration is claimed yet.
Main `598e395` is frozen during merged-World tests; it contains World's `255035b`.
The integration MCP union is read 34, write-enabled 67, CLI exec 37 and both 70;
provider-free exec remains 34. Asset kinds total 26 unique entries.

Project execution before World produced 2,035 passes and nine fixture failures.
The unknown-kind fixture used newly valid video, material scenes omitted entity
names, and sequencer skinning fixtures omitted Transform/Camera3D. Repairs
`3afb4f4`, `ba9d8ec` and `0d36ad8` are assembled; reruns remain pending. Sequencer
origin-rebase integration exposed a separate exact AbsolutePosition baseline
ownership defect; its source repair and independent recheck are active.

Next Hi-Z slice: the existing Hi-Z worker owns a new native pipeline visibility
module and tests consuming the actual GPU visibility mask, stably compacting
existing instance records and generating indexed/nonindexed indirect arguments.
Per-batch group-3 ranges are aligned and first-instance is zero. GPU tests must
render diagnostic tiles with depth disabled and inspect actual surviving counts;
a mask-only test is insufficient. No shared instance/material ABI changes.

Subsequent serialized integration adds trusted OccluderDepth then Visibility
stages before Opaque, using a separate depth texture containing only certified
current-frame geometry. Existing custom-shader prepass depth is not trusted.
CPU LOD is selected before all camera prepasses; its chain bounds enclose every
level, projection uses all transformed corners and uncertainty stays visible.
Custom displacement/alpha, unsupported World paths, query overflow and missing
capabilities take an explicit visible/direct fallback. Shadow uses its own caster
plan, independent of camera visibility. Authored MeshLods and desktop upload,
actual selected geometry/hysteresis, occluder movement, resize/jitter/multiview,
and off-camera shadow regressions remain mandatory before feature acceptance.

Build-cache ownership: main uses target/world-merge-validation/build; integration
will use its own target/validation-build. The old morph cache is retained only as
evidence. Full integration cargo fmt --all hit Windows command-line error 206;
affected packages formatted individually. Whole-workspace formatting still needs
a successful package-batched check or the shorter main path.
### Assembled rendering followups awaiting execution

Authored depth/motion adapter `313c0ca` passed source review and is assembled via
`09c8dbf`, with its local dependency locked in `ec88509`. Optional Camera3D
settings reach native per-view controls; absent settings disable them and explicit
feature-filter false takes precedence. Project and desktop pixel regressions are
written but unrun.

World shared-shadow adoption `c803ea1` passed source review after repairs and is
assembled via `6fb2d85`. Terrain and foliage use the shared light atlas; per-tile
instance buffers prevent queued uploads from overwriting earlier light views.
Foliage previews color LOD hysteresis without advancing it. GPU water casting is
opt-in opaque depth; transparent CPU water explicitly reports unsupported casting.
The new schema is closed and includes cast_shadows. GPU receiver tests cover
cross-material shadows, disjoint light tiles, alpha gaps, retained LOD silhouette
and wave-driven water shadows. These tests still require execution.

Hi-Z draw adapter `c2dc5cf` passed source review and is assembled. It consumes the
actual visibility mask and emits stable compacted instance ranges and real
indexed/nonindexed indirect draws. Exact ordered batch coverage and four-bind-group
capability checks fail before writes. This is an opt-in native adapter: trusted
occluder stages, projected bounds, generic LOD and actual frame submission wiring
remain pending. No skipped-draw or performance claim is established yet.

All 55 workspace packages passed individual formatting checks at `51b89f8`.
Subsequent authored adapters were separately formatted by their owners; final
integrated formatting and runtime gates remain required. Main's isolated workspace
test build continues at `598e395`; it is not a passing test result yet.

Native IBL/probe shading adapter is now in progress in codex/render-indirect,
based on `0232937`. Its single owner connects existing validated HDR resources to
built-in mesh shading and bounded per-draw probe selection, with actual pixel
oracles and neutral defaults for unaffected materials. This slice preserves the
208-byte instance ABI and does not yet add lightmap UV2, authored asset loading,
new scene-color stages or temporal lifecycle changes. Those remain explicit
followups; native resource creation alone did not establish shaded IBL behavior.

Sequencer/origin repair `aa8e501` passed independent source review and is assembled
in `ac9ced2`. Root track ownership survives legitimate rebases, preserves exact
absolute baselines plus representable local residuals, and yields to foreign
Transform or AbsolutePosition writes. Private freshness/residual state is excluded
from serialization. Runtime regressions cover immediate and repeated rebases,
paused/seek/Stop/replay, parent changes, near-limit baselines, tiny local keys and
native absolute mutations. Execution is still pending, including the existing
coordinate service suite, to validate the combined ownership and precision rules.

Merged main `598e395` passed the complete locked workspace test command: 5,015
passed, zero failed, 153 ignored across 331 reported suites. Its separate World
and default-desktop GPU gates are running. Strict workspace Clippy and formatting
also passed on that unchanged main revision; this evidence does not validate the
newer pending-integration feature set.

Native IBL/probe shading `94fa04b` passed source review and is assembled at
`2c1c665`. Per-view optional native lighting reaches actual built-in/graph-template
mesh shading, with bounded material variants and unchanged instance ABI. Its
GPU oracle sources cover HDR response, rotation, specular mip/BRDF response,
probe blending/priority, screen-lighting precedence, and view lifecycle. Authored
loading, World IBL shading and lightmap UV2/consumption remain pending. All new
IBL numerical and GPU execution checks remain unrun.

### Current acceptance checkpoint after AI handoff

Main World remains complete at `598e395`: formatting, strict workspace Clippy,
5,015 workspace passes, 11 World GPU passes and one desktop default-render pass.
No push has occurred. Older progress entries above record their original state.

Pending integration now includes IBL sampling repair `df9f7f2` and spatial AA
`ec013a6`, merged at `fa6dfb7`. Both passed independent source review. The earlier
pipeline CPU run passed 50 library tests but failed one of nine indirect tests;
regularized-GGX sampling coverage caused the grazing-angle error. The repair
keeps the independent oracle, thresholds and 4,096-sample limit unchanged.
Its real Rust test rerun is pending. The separate UI presentation-pin GPU test
passed on the RTX 5080 at `ee57d57`.

The full UI GPU build at `ee57d57` failed before test execution with E0716 in
World foliage's target-array lifetime. Its owner is repairing this mechanical
compile error; no UI pass is claimed. Logs are `target/combined-validation`.
The dedicated integration build cache is `target/validation-build`.

Spatial AA uses an optional HDR PostProcess pass before tonemapping/UI, with
native, authored camera and desktop adapters. Its source-reviewed tests compare
real GPU results to an independent supersampled analytic diagonal reference;
execution remains pending. Native lit 2D sprite implementation is in progress,
reusing World's tile geometry, existing normal/MR slots and sprite ordering.
Project/desktop tile lighting and opted-in hybrid routing follow that native
slice; native-only checks will not establish complete World rendering acceptance.

Worktree record addition: `codex-ai-scripting`, path
`D:/Git-Worktrees/Rust-Game-Engine/codex-ai-scripting`, branch `codex/ai-scripting`,
base `ee57d57`, creator/cleanup root, active scripting manager. Its own branch
integrates `d88464d` before scripting. No cross-checkout build cache reuse.