# Scheduled runtime events

`engine run <manifest> --headless --ticks 60 --events schedule.json` supplies
reproducible event input to compiled Rust gameplay. The schedule path is relative
to the command's working directory; absolute paths also work. Visible runs and
CLI screenshots use the same tick schedule. This starts a bounded run; it does
not control a separate, already-running process.

The file is a JSON array ordered by nondecreasing zero-based tick. Equal-tick
entries keep their listed FIFO order:

```json
[
  {"tick":0,"name":"game.damage","target":"player","payload":{"amount":3}},
  {"tick":30,"name":"game.reset","source":"player","target":"player","payload":{}}
]
```

Each entry requires `tick` and `name`. Optional `source` and `target` are authored
entity IDs, not ECS handles; missing `target` broadcasts. `payload` accepts JSON
and defaults to null. Unknown entry fields are rejected. The project must register
its callbacks and bind the event names normally; scheduling cannot define or
execute Rust source. A syntactically valid event with no interested handler is
still delivered and appears in the trace.

Immediately before each tick, the host resolves source and target IDs against the
active scene. Missing or despawned IDs fail explicitly. It validates and enqueues
the entire tick batch atomically, accounting for events already queued by gameplay.
A failed batch leaves that queue unchanged. Runtime dispatch occurs during that
same tick; callback-generated events retain the existing next-tick semantics.
Entity removal by an earlier callback can still suppress subsequent targeted
callbacks, as with ordinary runtime events.

Schedules are limited to 1 MiB encoded JSON, 4,096 entries total, 1,024 entries per
tick, 128 UTF-8 bytes per event name, 256 per entity ID, and 64 KiB encoded payload
per event. Names/IDs must be nonempty and contain no control characters. Every
tick must fall within the requested run. Runtime `EventLimits` also apply; a game
may lower queue or payload capacity. Inputs are read with a bounded reader.

## MCP

`project_run_headless` accepts the same array inline as optional `events`:

```json
{"ticks":60,"events":[{"tick":0,"name":"game.damage","target":"player","payload":{"amount":3}}]}
```

The existing `--allow-exec` permission is required, including when `--allow-write`
is enabled. Authoring-write access is unnecessary: validated event input is staged
inside the run's disposable user-data directory and removed with it. Authored
project files are not edited. Executing a generation still runs the project's
native gameplay with the host user's privileges.

## Observing delivery

CLI `RunReport.event_trace` contains `{events, count, truncated}`. Each recorded
event includes its zero-based delivery `tick`, name, stable source/target IDs and
payload. It records actual event dispatch batches across the run, including
scheduled, callback-generated, collision, animation and UI events; it is not an
echo of the requested schedule or an assertion that every bound callback ran.
Shutdown preserves this trace. `RunReport.scene.events` remains the final tick's
batch only, scoped to the current scene. The cross-run trace preserves deliveries
that occurred before a scene transition during the same tick.

The trace retains at most 4,096 records and 1 MiB of serialized record content.
After reaching either cap it stops retaining/copying payloads, sets `truncated`,
and continues a saturating `u64` total `count`. Array/envelope overhead is additional.
MCP presents at most the first 100 trace entries and 64 KiB of record content,
with `event_count` and `event_trace_truncated` preserving the full-run count and
whether either limit omitted data. Final authored component state is reported
through the existing entity summary.

Native hosts can construct `EventSchedule`, call `validate_run`, and invoke
`Runtime::enqueue_scheduled_events(&schedule, tick)` before each `Runtime::tick`.
`Runtime::delivered_events()` borrows the latest stable-ID records without taking
a full scene snapshot. `EventTrace::capture` provides the same bounded collector.
