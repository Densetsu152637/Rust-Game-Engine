# Deterministic 2D gameplay mechanics

`engine_project::gameplay2d` supplies value-based mechanics that the authored
runtime and native game code share. `gameplay2d::World` is a plain struct (also a
Bevy `Resource`), so it can be unit-tested without a window or an ECS schedule. The
runtime keeps it synchronized with authoritative ECS transforms through
`runtime::move_entity`, `safe_position` and the sync step in `services`.

## Bodies and queries

A `Body` has a centre `position`, full positive `size`, an `offset` for the collider
relative to the position, a `trigger` flag and nonzero `layer` and `mask` bitfields
(defaults: unit size, layer 1, mask all bits). These correspond to the authored
`Collider2D` component. Coordinates, sizes and offsets are finite and bounded to
1,000,000 units per axis. IDs are non-empty, at most 256 bytes, and a world holds at
most `World::MAXIMUM_BODIES` (1024) bodies. Two bodies interact only when each
body's layer intersects the other's mask. Touching boxes do not overlap; overlap
needs positive area.

`upsert`, `remove`, `clear`, `body`, `ids`, `query(area, ignore)`, `overlaps(id)` and
`is_safe(body, ignore)` return deterministic, sorted results after mutual-mask
filtering.

## Movement

`move_body(id, delta)` sweeps the requested displacement against solid (non-trigger)
bodies. It tests the whole distance instead of choosing substeps, so high speed does
not tunnel; blocked axes stop and the remaining axis slides, and simultaneous corner
hits block both axes. A starting overlap is resolved by deterministic minimum
translation with a 32-iteration bound; if it cannot be resolved the call fails without
mutating the body. Unrepresentable contacts or out-of-range movement also fail
transactionally. The result (`MoveResult`) holds the resolved position and sorted
contact IDs. Other bodies are stationary during a single move, so callers define a
fixed update order. This is exploration movement, not a rigid-body impulse solver.

## Triggers

Trigger bodies never block, including while moved. Call `advance_triggers()` once
per tick after all final movement. It returns sorted `TriggerEvent { first, second,
phase }` values with `TriggerPhase::Enter`, `Stay` or `Exit`. A crossing that begins
and ends outside within one tick creates no overlap event. Removing a body produces
exits on the next call, and re-inserting it afterwards permits a fresh enter.
`clear()` (room replacement) silently discards pairs and bodies so old-room exits
cannot leak into the new room. Stable authored IDs identify bodies; runtime `Entity`
handles are validated separately.

Collision trigger events also notify attached audio sources with
`trigger:<entityId>:enter`, `:stay` and `:exit` (see
[Audio and persistence](desktop-services.md)). A play observer on `:stay` restarts
each tick, so use `:enter` for one-shot cues.

## Placement, following and flocking

- `find_safe(body, radius, step, ignore)` checks the preferred point, then eight
  fixed directions per ring up to 64 rings, and returns `None` if no sample is safe.
  It is a bounded placement query, not a navigation search.
- `follow_velocity(from, target, speed, separation)` returns a capped velocity toward
  the target and zero inside the separation radius.
- `flock_target(centre, index, count, radius)` returns stable circular slots; callers
  choose a safe slot with `find_safe` and apply movement themselves.

These helpers make no narrative decisions (save success, healing, acquisition, flock
size); that is game-owned logic, as `examples/desktop-2d` demonstrates in its
controller and progression modules.

## Tests

`crates/engine-project/tests/parity.rs` covers swept collision without tunneling,
touching and corner grazing, ordered triggers with removal exits and bounded safe
search, follow and flock. `examples/desktop-2d/tests/gameplay.rs` drives the real
`Runtime` with injected input for movement speed, normalized diagonals, solid sweeps,
followers and room transitions.
