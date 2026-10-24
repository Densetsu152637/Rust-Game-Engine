# engine-navigation

Deterministic, bounded weighted-grid and waypoint-graph pathfinding with a
distance-based path follower. This crate has no ECS or physics dependency.

`Grid::new(width, height, costs, diagonal)` accepts row-major cells where `None`
is blocked and `Some(positive_cost)` is the cost of entering a cell. Grid A*
uses the minimum traversable cost times Manhattan distance (four-way) or
Chebyshev distance (eight-way). Diagonal steps have the same cell cost as
orthogonal steps and cannot cut between blocked corner neighbors. `set_cost`
supports obstacle changes between searches.

`Graph::new(edges)` accepts a directed adjacency list of `(node, positive_cost)`
edges. Its search uses a zero heuristic (Dijkstra), making no unsupported
assumption about arbitrary graph geometry. Equal-cost choices are ordered by
node index, yielding repeatable paths.

Both `find_path` APIs return `PathResult::Reached { nodes, cost }`, `Unreachable`,
or `BudgetExceeded`. Invalid coordinates or malformed graphs return errors.
`SearchBudget` bounds expanded nodes; it does not return a misleading partial
path when exhausted. A zero budget may still return a start-equals-goal path.
Costs accumulate in u64. Grids/graphs cap at one million cells/nodes, and graphs
cap at eight million edges. Search allocates storage proportional to the input;
the expansion budget is not a promise of a fixed memory footprint.

`Agent` follows Vec3 waypoints, consuming distance across multiple segments in
one `advance(dt, speed)` call without overshooting the destination. It does not
perform collision avoidance, steering around moving obstacles, or replanning.
Coordinates are finite and bounded to 1e8; dt is 0..60 seconds and speed 0..1e6.
The host can assign world-space waypoints from grid cells or authored graphs.

This is grid/graph navigation, not polygon NavMesh baking or crowd simulation.
Integrations should query/update their obstacle representation before searching,
and submit movement to the appropriate kinematic controller afterward.

Validation: `cargo test -p engine-navigation` checks weighted detours,
deterministic output, unreachable goals, budget exhaustion, corner blocking,
directed graph costs, invalid input, and multisegment following.
