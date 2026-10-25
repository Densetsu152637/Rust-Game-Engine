# Crowds, carving and off-mesh links

Everything here builds on the baked NavMesh of [Navigation meshes](navigation.md). The baked
mesh stays immutable and its serialized navigation documents (`version` 1) are unchanged; all of
the following is additive state layered on top, in `crates/engine-navigation` and in the
`engine.CrowdAgent`, `engine.NavigationObstacle`, `engine.OffMeshLink` and `engine.Formation`
authored components.

## Crowd steering (ORCA)

`Crowd` holds up to 4,096 agents (default cap 256) with a radius, maximum speed and arrival
radius. Each step an agent derives a preferred velocity from its NavMesh path (the waypoint
follower `Agent` stores the path and cursor; `NavMesh::find_route` produces it), then solves the
RVO2 reciprocal-velocity-obstacle linear program on the XZ plane against at most
`max_neighbors` (default 12, cap 64) nearest neighbours inside `neighbor_distance`. Y follows the
path segment being walked. Static obstacle segments (`set_segment`) add one-sided ORCA lines using
the closest point on the segment, plus a hard push-out; this is an approximation of full RVO2
obstacle-vertex handling.

Determinism: agents are stored in id order, the neighbour search is a sorted-array spatial hash
(no `HashMap` iteration), neighbours order by distance then index, velocities are computed from a
snapshot, and nothing is random. Exactly head-on agents are separated by a small fixed
per-agent preferred-velocity nudge (`symmetry_nudge`, default 2% of max speed, hashed from the
agent id). Identical inputs give bitwise-identical positions. A hard overlap pass
(`contact_iterations`) is a backstop; `CrowdStats::contact_corrections` reports how often it fired
(zero in the measured scenarios).

Measured (dt = 0.05 s, radius 0.4, speed 2 m/s, `cargo test -p engine-navigation --test crowd`):

| Scenario | Min gap over run | Ticks until all arrived |
| --- | --- | --- |
| Head-on swap, 2 agents, 20 m | +0.020 m | 201 |
| 4-way crossing, 32 agents | +0.013 m | 437 |
| 3x3 formation, worst slot error | 0.047 m (arrival radius 0.05) | - |

Native use:

```rust
let mut crowd = Crowd::new(CrowdSettings::default())?;
let id = crowd.add_agent(AgentDesc { position, ..Default::default() })?;
let nav = NavContext { mesh: &mesh, overlay: Some(&overlay), query: QuerySettings::default() };
crowd.set_goal(id, goal, &nav)?;
for event in crowd.step(dt, Some(&nav))? { /* Arrived, LinkEntered, LinkExited, Repathed, PathFailed */ }
```

## Formations

`set_formation(id, FormationDesc { slots, anchor, yaw, follow_heading })` defines `[right, forward]`
slot offsets around a fixed point or a leader agent. `assign_formation` matches members to slots
greedily by distance (ties by agent id, then slot index), which is deterministic and is only
recomputed when you call it. Members steer straight to their slot with normal avoidance.

## Dynamic obstacles (carving)

`NavOverlay::new(&mesh)` holds a per-polygon block counter. `set_obstacle` blocks every polygon
whose cell overlaps the obstacle footprint (box or cylinder) inflated by the carve margin
(default: the baked agent radius) and whose height range is within agent height of the obstacle.
Nothing is re-baked. Precision: carving is cell resolution and conservative, so the blocked area
may exceed the inflated footprint by up to one `cell_size`; a path never enters the inflated
footprint (measured clearance 0.2 m beyond it with 0.5 m cells). Overlapping obstacles are
reference counted; `remove_obstacle` restores the original path exactly.

`NavMesh::find_route(start, goal, query, Some(&overlay))` honours the overlay. Staleness: the
overlay `revision()` changes on any obstacle or link change; `Crowd::invalidated_agents` lists
agents whose remaining path crosses a blocked polygon or uses a disabled link, and `step` with a
`NavContext` repaths them automatically (`CrowdEvent::Repathed` or `PathFailed`).

## Off-mesh links

`set_link(&mesh, id, OffMeshLinkDesc { start, end, cost, bidirectional, enabled, snap_distance })`
snaps both endpoints to the nearest mesh location. `find_route` searches polygon hops (cost 1
each) plus enabled links (cost in the same units; with links present the search is Dijkstra). A
`NavRoute` lists each `RouteLink` with the point indices of entry and exit. Example: two islands
joined by a cost-3 link, 7 hops before and 9 after, cost 19.0; disabled or removed links give
`Unreachable`. The crowd follower reports `LinkEntered`/`LinkExited` and moves straight along the
link at max speed (no avoidance) so gameplay can animate it.

## Authored components

All use `{"version":1,"data":{...}}` in `components.custom`, require `Transform`, and name the
NavMesh asset in `navmesh`. Agents, obstacles, links and formations that share a `navmesh` share
one carving overlay and one crowd.

- `engine.CrowdAgent`: `{"navmesh":"nav","target":[x,y,z],"radius":0.4,"max_speed":2,"arrival_radius":0.1,"formation":"<entity id>","repath":0}`.
  Bump `repath` to force a new route. `formation` overrides `target`. Exclusive with
  `engine.NavigationAgent`, character controllers and non-kinematic bodies.
- `engine.NavigationObstacle`: `{"navmesh":"nav","shape":"box","half_extents":[x,y,z],"radius":r,"half_height":h,"enabled":true}`
  (`shape` is `box` or `cylinder`), centred on the entity; axis aligned (rotation and scale are
  ignored). Moving or toggling it recarves and repaths affected agents.
- `engine.OffMeshLink`: `{"navmesh":"nav","end":[x,y,z],"cost":1,"bidirectional":true,"enabled":true,"snap_distance":1}`;
  the start is the entity position.
- `engine.Formation`: `{"navmesh":"nav","slots":[[right,forward]],"leader":"<entity id>","yaw":0,"follow_heading":false}`;
  members are the agents whose `formation` names this entity.

Events (delivered at the start of the next tick, like all engine events): `navigation.repathed`
(`{"target"}` or `{"reason":"invalidated"}`), `navigation.failed` (`{"target","reason"}` or
`{"reason":"blocked"}`), `navigation.arrived` (`{"position"}`), `navigation.link_entered` and
`navigation.link_exited` (`{"link":"<link entity id>"}`).

Limits: 1,024 agents, 256 obstacles, 256 links, 64 formations, 16 cached meshes (262,144
polygons), 256 slots per formation. The service runs after `NavigationAgent` and writes world
positions through the same transform helper, so a crowd agent owns its pose. Ragdoll bones are
not checked against crowd agents.

## MCP authoring

No dedicated tool is needed: `component_set` and `component_schema` accept the four components
like any `engine.*` component, and `asset_info` usage scans count their `navmesh` references.

Focused validation: `cargo test -p engine-navigation --test crowd --test overlay` and
`cargo test -p engine-project --test crowds`.

## Algorithm references

ORCA / RVO2: van den Berg, Guy, Lin, Manocha, "Reciprocal n-body collision avoidance" (2011),
with the incremental 2D linear programs of the RVO2 library.
