# Navigation meshes

`engine-navigation::bake(&BakeInput)` builds an immutable polygon mesh from
explicit world-space triangle floors and solid axis-aligned box obstacles.
It supports disconnected islands, ramps, steps and up to eight overlapping
vertical floor layers per XZ cell. Existing `Grid`, `Graph` and waypoint
followers remain available.

Run the obstacle-detour example with
`cargo run -p engine-navigation --example navmesh`.

## Bake contract

Coordinates are Y-up. Triangle winding does not matter. The bake clips triangles
against a bounded XZ raster, groups coplanar fragments into layers, rejects
positive-area duplicate/overlapping floor triangles, and retains only completely
covered cells. Coverage uses double-precision clipping and polygon subtraction,
not an accepted percentage of missing area: any unresolved positive-area
remainder blocks the cell. Subtraction uses original triangle boundaries to
avoid creating cracks from repeatedly clipped shared edges. Each retained cell
becomes a convex quad on its source plane.
Quads connect only through shared cell edges whose endpoint height differences
fit `max_step`. Disconnected floors never acquire arbitrary waypoint links.

`BakeSettings` defaults are `cell_size: 0.25`, `agent_radius: 0.25`,
`agent_height: 1.8`, `max_slope_degrees: 45`, `max_step: 0.3`.
Steep surfaces cannot supply walkable floors. Upper triangle fragments and box
obstacles remove floors without headroom. Each candidate retains its lowest
overhead elevation, including partial ceilings. Radius clearance checks the
entire candidate cell's maximum agent-top elevation against neighboring overhead
spans; step portals also check the highest transition elevation against both
endpoint spans. Radius clearance uses conservative square erosion, rounded
**up** to whole cells. This guarantees at least the
requested horizontal radius against missing/blocked cells, but can reject a
passage that would fit an exact circular agent. Choose resolution accordingly.

This is a layered, explicit-surface baker, not arbitrary-scene voxelization or
Recast equivalence. Supply wall/solid geometry as obstacle boxes. Vertical input
triangles have no floor footprint and do not become walls. Triangles meeting at
different slopes inside a cell can leave a conservative hole; align surface
changes/steps with the raster or use smaller cells. Coplanarity/overlap tests
use small floating-point tolerances; coverage never deliberately fills an
unresolved hole. Numerical ambiguity can conservatively reject usable cells.
Off-mesh jumps, swimming, dynamic carving, local crowd avoidance and automatic
physics obstacle extraction are not supplied.

Limits are 65,536 vertices, 32,768 triangles, 4,096 obstacles, 65,536 occupied
XZ cells/polygons, eight layers and 128 clipped fragments per cell (262,144
total), and four million bake operations. Coverage subtraction is charged to
that work budget before clipping, with at most 1,024 remainder polygons per
cell and 64 vertices per remainder. Coordinate magnitudes cannot exceed
100,000; all values must be finite. Oversized bounds are rejected before raster loops.
Radius/cell ratio is at most 128. Degenerate triangles, invalid indices,
overlapping floors and invalid settings return errors; zero walkable polygons
is a valid bake result.

## Queries and native callers

`NavMesh::nearest(position, QuerySettings)` returns the closest geometric point
on a polygon within `max_distance`, including on sloped floors. Its polygon
scan budget fails explicitly if insufficient. `find_path(start, goal, query)`
returns `NavPath::Reached { points, polygons }`, `Unreachable`, or
`BudgetExceeded`. Invalid/nonfinite inputs return errors. Endpoints are projected
onto their closest eligible polygons; queries do not promise the original
off-mesh endpoints are traversable. Overlapping-floor ties resolve deterministically.

Queries search polygon adjacency with A*, then string-pull shared portals.
Portal crossings are lifted to each floor plane, preserving ramp/step changes;
only collinear 3D points are removed. Paths remain within the eroded polygons.
They are corridor-smoothed, not guaranteed globally shortest across every
possible corridor. The query defaults are `max_distance: 2`,
`max_polygons: 65536`, `max_expansions: 16384`, `max_points: 4096`.
`max_points` bounds intermediate portal crossings as well as returned points;
use a larger limit for long corridors. Funnel restarts have a four-million
operation ceiling. Queries do not return partial paths on exhaustion.

## Project assets and runtime callbacks

Register a manifest asset with `kind: "navigation"`. Its file is a bounded
16 MiB JSON `NavMeshDocument`:

```json
{
  "version": 1,
  "input": {
    "vertices": [[0,0,0], [4,0,0], [4,0,4], [0,0,4]],
    "triangles": [[0,1,2], [0,2,3]],
    "obstacles": [],
    "settings": {"cell_size":0.25,"agent_radius":0.25,"agent_height":1.8,"max_slope_degrees":45,"max_step":0.3}
  }
}
```

Assets store source geometry/settings rather than trusting serialized topology.
`simulation::load_navmesh` validates and bakes the document, returning
`Arc<NavMesh>`. `navigation_from_meshes(project, asset_ids, settings, obstacles)`
uses registered mesh positions/indices to produce the same validated document.
It accepts at most 128 mesh assets, already positioned in world space; it does
not infer scene-instance transforms. Native code can construct `BakeInput` with
transformed vertices directly.

Author `engine.NavigationAgent` version 1 data as:

```json
{"navmesh":"level-navigation","target":[3,0,3],"speed":2,"repath":0,"query":{"max_distance":1,"max_expansions":16384}}
```

`navmesh` and `target` must appear together, with no nonempty `waypoints`.
Omit both for existing waypoint mode. A changed component or incremented native
`repath` counter queries from the current world pose. Successful projection
snaps the starting pose onto its polygon before following, so an unvalidated
off-mesh connector is never traversed. The goal is the projected endpoint.
Failed routes stop movement and emit `navigation.failed` with `target` and
`reason` (`unreachable` or `budget_exceeded`). Successful queries emit
`navigation.repathed` with `target` and `polygons`; arrival emits
`navigation.arrived` with `position`. Events use existing `EventBindings` and
`register_event_handler`, delivered at the beginning of the next runtime tick.
Invalid assets/configuration fail the simulation boundary rather than becoming
successful routes. Incrementing `repath` retries a failed route.

Runtime limits are 1,024 followers, 16 cached meshes, 262,144 cached polygons,
and one million reserved nearest-scan/search operations per sync; exhaustion
emits a route budget failure. Meshes stay immutable/cached for the scene;
rebake changed geometry and reset/reload the scene to use it. Native transform
writes must respect navigation pose ownership or request a repath. Existing
kinematic-only physics sharing and parent-transform conversion still apply.

Crowd steering, runtime obstacle carving, off-mesh links and formations are documented in
[Crowds, carving and off-mesh links](ai-crowds.md).

## MCP authoring

With write access, `navigation_bake` accepts `meshes` (asset IDs), `output`
(new project-relative JSON file in an existing directory), optional `settings`
and `obstacles`. It validates/bakes before publishing the file and never
overwrites an existing path. It returns polygon count, content hash and a
manifest asset entry with a placeholder ID. Add that entry to `project.json`
using normal revision/hash-aware `file_write`, choosing a unique asset ID.
The manifest is not silently changed by baking. Source data is copied into the
document; rebake after source edits.

Read-only `navigation_query` accepts `path`, `start`, `goal`, and optional `query`.
It can query the generated file before manifest registration. Both tools use
the shared project/navigation APIs, reject traversal/protected paths and do not
compile or execute game code. Asset discovery accepts navigation kinds and
reports settings, polygon counts and NavigationAgent uses.

## Algorithm references and decision

The explicit layered raster keeps dependency/build requirements small and
makes conservative clearance and work limits inspectable. A full Recast import
would provide richer arbitrary-scene processing but is outside this bounded
explicit-surface contract. The implementation follows the geometry/portal
separation described in Mikko Mononen's
[funnel explanation](https://digestingduck.blogspot.com/2010/03/simple-stupid-funnel-algorithm.html)
(2010): polygons establish a corridor; portals smooth it after clearance is
accounted for. Recast's
[erosion implementation](https://github.com/recastnavigation/recastnavigation/blob/main/Recast/Source/RecastArea.cpp)
is a primary reference for eroding walkable regions before path queries;
this implementation deliberately uses a simpler conservative square erosion.

Focused validation: `cargo test -p engine-navigation`,
`cargo test -p engine-project --test navmesh --test simulation`, and
`cargo test -p engine-mcp --test navigation`.
