# Mesh LOD, HLOD and culling (`engine-mesh-lod`)

`crates/engine-mesh-lod` is a pure-Rust CPU crate (only dependency: `glam`, no GPU code).
It provides mesh simplification, LOD chains with screen-space selection, HLOD clustering
and frustum culling math for the renderer scalability work and for asset import.
It adds no authored components or MCP tools.

## Input contract

`MeshData { positions, normals: Option, uvs: Option, indices: Vec<u32> }`, triangles
counter-clockwise. `MeshData::validate` (called by every entry point) rejects: empty meshes,
index count not a multiple of 3, indices out of range, normal/UV length different from the
position count, and NaN/infinite values. Errors are `MeshLodError` variants.

## Simplification (`simplify`)

Garland-Heckbert quadric edge collapse with a lazily invalidated binary heap.

- Cost: sum of squared distances to the planes of adjacent faces (unweighted, so the cost
  is in squared object units). Candidates: either endpoint, the midpoint and, for interior
  edges, the quadric-optimal point (rejected if far from the edge).
- Attribute awareness: the ordering cost adds `local_area * (normal_weight * 0.5 * (1 - n_a.n_b)
  + uv_weight * |uv_a - uv_b|^2)`; attributes of the collapsed vertex are interpolated
  (normals renormalised). The attribute terms only order collapses; they are not part of the
  reported geometric error.
- Boundaries: open-boundary edges add heavy perpendicular quadric planes (`boundary_weight`),
  so boundary vertices only slide along collinear boundary edges at about zero cost. Collapsing
  a chord between two boundary vertices is forbidden. `lock_boundary` forbids touching them.
- Seams: after welding bit-identical vertices, any vertex sharing a position with another
  vertex (UV/normal seams, split poles) is **locked** and never removed. Non-manifold edges lock
  their vertices.
- Validity: link condition (manifold preserving), no collapse that flips a surviving triangle
  (new normal dot old normal <= 0.05 of the product of lengths) or creates a sliver (double
  area < 1e-5 x longest edge squared), and no collapse of a closed tetrahedron or single
  triangle into a sheet.
- Options: `SimplifyOptions::to_triangle_count(n)`, `to_max_error(e)` or both. `max_error`
  skips collapses whose quadric error (square root, object units) exceeds it. At least one is
  required.
- Result: `SimplifyResult { mesh, achieved_error, original_triangles, final_triangles,
  reached_target }`. `achieved_error` is `mesh_error(input, output)`.
- Deterministic: no hash iteration order is used for decisions; identical input gives
  identical output.

Limits: the triangle count may undershoot the target by one (an interior collapse removes two
triangles) and may stop above it if locked seams, boundaries or the validity rules block all
collapses (`reached_target == false`). Meshes where every edge is a seam (fully unwelded
flat-shaded meshes with distinct normals per face) cannot be reduced. Seam vertices are
locked rather than collapsed in lock-step, so heavily seamed meshes reduce less than welded ones.

## Error metric (`mesh_error`, `distance_to_surface`)

Approximate symmetric Hausdorff distance: vertices and triangle centroids of each mesh are
measured against the other mesh surface with exact point-triangle distance (uniform-grid
accelerated). It is a lower bound of the true Hausdorff distance (unsampled interior points
are not examined). Units are object units.

## LOD chains and selection

`build_lod_chain(mesh, &[LodTarget], &SimplifyOptions)`: level 0 is the original (error 0);
each next level is simplified from the previous one for `Ratio`, `Error` or `Triangles` targets,
its `error` is measured against the original and made non-decreasing.

Screen-space selection: projected pixels = `error * viewport_height / (2 * d * tan(fov/2))`,
with `d = max(distance - bounding_radius, 1e-4)` (nearest point of the bounding sphere,
conservative). `select_lod(errors, sphere, distance, params, previous)` returns the coarsest
level with projected error <= `pixel_threshold`. With `previous` and `hysteresis = h`, it goes
coarser only when the level fits `threshold * (1 - h)` and finer only when the current level
exceeds `threshold * (1 + h)`.

Ratio mode (Unity LODGroup style): `screen_height_fraction` = `radius / (d * tan(fov/2))`;
`ScreenSizeLods { thresholds (strictly decreasing), cull_below, hysteresis }.select(fraction,
previous)` returns `Some(level)` or `None` when culled, with the same dead-band idea
(`previous = None` means unknown or culled).

## HLOD (`build_hlod`)

Instances (`mesh` index + object-to-world `Mat4`) are clustered by recursive spatial-midpoint
splits of world AABB centres until each cluster has <= `max_cluster_instances` members and a
bounds side <= `max_cluster_extent` (a single oversize instance still forms its own cluster).
Each cluster members are merged in world space (normals via the inverse transpose; normals
and UVs only if all members have them; mirrored transforms get winding corrected) and
simplified to `min(ceil(merged * proxy_ratio), max_proxy_triangles)` triangles. Output per
cluster: `members` (ascending), `bounds`, `bounding_sphere`, `proxy`, `merged_triangles`,
`proxy_error`, and `switch_distance` = distance at which the proxy error projects to
`pixel_threshold` plus the cluster radius (so never less than the radius). Clusters are
ordered by first member. Separate closed objects cannot reduce below 4 triangles each.

## Culling (`Frustum`)

`Frustum::from_view_projection(&vp)`: Gribb-Hartmann planes for right-handed WebGPU clip space
(`0 <= z <= w`), normalised, inward normals, order left/right/bottom/top/near/far. Infinite far
or reversed-Z projections work (a degenerate plane becomes zero and accepts everything).
`test_sphere`/`test_aabb` return `Outside | Intersecting | Inside`; they never report Outside
for something visible, but a sphere or box near a frustum edge or corner may be reported
Intersecting although invisible (standard conservative plane test). `cull_spheres` and
`cull_aabbs` write ascending visible indices into a reusable `Vec<u32>`. `Aabb` and
`BoundingSphere` (Ritter versus box-centre, smaller wins) provide builders and transforms.
In glam 0.34 build matrices with `glam::camera::rh::proj::directx::perspective*`.

## Usage

```rust
use engine_mesh_lod::*;
let chain = build_lod_chain(&mesh, &[LodTarget::Ratio(0.5), LodTarget::Ratio(0.1)], &SimplifyOptions::default())?;
let errors = chain.errors();
let params = LodSelectionParams { vertical_fov: 1.0, viewport_height: 1080.0, pixel_threshold: 2.0, hysteresis: 0.1 };
let level = select_lod(&errors, &chain.bounds, camera_distance, &params, previous_level);
```

## Measured results (debug build, from the crate tests)

| Case | Result |
| --- | --- |
| 32x32 plane (2048 tris) to 4 | 4 tris, error 2.5e-15, area and bounds preserved |
| welded UV sphere 4900 tris to 490 | 490 tris, vertex radius deviation 0.79% of R, measured error 1.37% of R |
| sphere `max_error = 0.01` | 4900 -> 1688 tris, measured error 0.00265 |
| sphere target counts 3000/1000/333/120 | 3000/1000/332/120 |
| HLOD 5 spheres x 2 groups | 2 clusters, 560 -> 56 tris each |

No performance claims are made; timing was not measured.
