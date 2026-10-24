# CPU profiling

`EngineApp` measures actual CPU wall time with `std::time::Instant`. It retains
the newest 120 simulation ticks by default. This telemetry never changes the
fixed `Time.dt`, `Time.elapsed` or `Time.tick` used by gameplay.

```rust
use engine_core::{EngineApp, Stage};

let mut app = EngineApp::new();
app.set_profile_capacity(240)?;
app.schedule_mut(Stage::Update).add_systems(|| { /* game work */ });
app.run_fixed(300, 1.0 / 60.0)?;
let profile = app.profile();
assert_eq!(profile.frames.len(), 240);
assert_eq!(profile.frames_dropped, 60);
# Ok::<(), engine_core::anyhow::Error>(())
```

`ProfileSnapshot` and its nested records implement `serde::Serialize`. The
snapshot has schema 1 and copies the retained history, so inspecting or exporting
it does not consume measurements. Frames are ordered oldest to newest.

Each frame records its simulation tick, measured `duration_ns`, success status,
executed stages and live entity counts for the simulation and render worlds.
The counts include ECS internal entities, not just authored game objects; they
sum archetype lengths rather than allocated entity slots, so despawns reduce them.
Counting and snapshot serialization happen outside the measured frame interval.
Frame timing begins after startup and advancing simulation time, and covers
Update, PostUpdate (including the fallible service hook), Extract, Render and
tracker maintenance. If a stop request runs Shutdown
inside that tick, that stage is included too.

Stage aggregates include Startup and Shutdown even when they run outside a tick.
Each aggregate records calls, returned failures, total, maximum and most recent
nanoseconds. A failed schedule initialization is recorded with its actual elapsed
time; the original error still propagates and marks the app failed. A partial
frame contains only stages that executed. Rejected deltas and failed startup
produce no invented frame. Panics remain panics and do not promise a completed
profile sample or recovery.

`set_profile_capacity` accepts 0 through `MAX_PROFILE_CAPACITY` (10,000). Shrinking
evicts the oldest samples immediately; zero disables frame retention while
preserving stage aggregates and total frame/failure counts. Invalid capacities
return an error without changing the prior configuration. Counts and nanoseconds
saturate at their integer limits. Retention is bounded independently of run length.

These are **CPU stage measurements**, not GPU timestamps, per-system costs,
allocator statistics, or presentation frame rates. A Render stage can submit GPU
work that finishes later. Desktop input, image capture, host drawing, pacing and
audio device callbacks outside `EngineApp::tick` are not measured here. Project
`NativeScript` callbacks and event delivery run before the core tick and are also
outside these stage measurements. Instrumented
code has clock, counter and retention overhead; use comparable release builds for
performance comparisons. The native `Instant` clock also does not provide a
browser runtime; the WebAssembly CI job checks compilation only.

Run `cargo test -p engine-core --test profiling` for actual elapsed-work,
bounded-retention, resizing, failure, stop and entity-lifetime coverage.
