// Built-in light clustering (engine.lights.clusters): one invocation per
// froxel tests every local light (bounding sphere, spot cone, area light
// half-space) against the froxel's view-space bounds and writes its list.
// Each workgroup loads lights in batches of 64 into shared memory, already in
// view space, so a light is fetched and transformed once per workgroup.
#import engine::view

struct ClusterGridOut {
    dims: vec4<u32>,
    depth: vec4<f32>,
    tile: vec4<f32>,
    params: vec4<u32>,
    counts: array<u32>,
}

@group(1) @binding(0) var<storage, read_write> grid: ClusterGridOut;
@group(1) @binding(1) var<storage, read_write> cluster_lights: array<u32>;
// x clusters over capacity, y longest list, z stored entries, w nonempty clusters.
@group(1) @binding(2) var<storage, read_write> stats: array<atomic<u32>, 4>;

const BATCH: u32 = 64u;

// View-space position and range.
var<workgroup> batch_spheres: array<vec4<f32>, 64>;
// View-space direction of travel; w: spot cos outer, or area two-sided flag.
var<workgroup> batch_axes: array<vec4<f32>, 64>;
var<workgroup> batch_kinds: array<u32, 64>;

// View-space point of the jittered camera ray through `ndc` at view depth `depth`.
fn froxel_point(ndc: vec2<f32>, depth: f32) -> vec3<f32> {
    let a = engine_view.view_from_clip * vec4<f32>(ndc, 0.0, 1.0);
    let b = engine_view.view_from_clip * vec4<f32>(ndc, 1.0, 1.0);
    let p0 = a.xyz / a.w;
    let p1 = b.xyz / b.w;
    let t = (depth + p0.z) / (p0.z - p1.z);
    return mix(p0, p1, t);
}

@compute @workgroup_size(64)
fn build(@builtin(global_invocation_id) id: vec3<u32>, @builtin(local_invocation_index) local: u32) {
    let dims = grid.dims;
    let cluster = id.x;
    let valid = cluster < dims.x * dims.y * dims.z;
    let cx = cluster % dims.x;
    let cy = (cluster / dims.x) % dims.y;
    let cz = min(cluster / (dims.x * dims.y), dims.z - 1u);
    let near = grid.depth.x;
    let ratio = grid.depth.y / near;
    let d0 = near * pow(ratio, f32(cz) / f32(dims.z));
    let d1 = near * pow(ratio, f32(cz + 1u) / f32(dims.z));
    let x0 = f32(cx) / f32(dims.x) * 2.0 - 1.0;
    let x1 = f32(cx + 1u) / f32(dims.x) * 2.0 - 1.0;
    let y0 = 1.0 - f32(cy + 1u) / f32(dims.y) * 2.0;
    let y1 = 1.0 - f32(cy) / f32(dims.y) * 2.0;
    var lo = vec3<f32>(1.0e30);
    var hi = vec3<f32>(-1.0e30);
    for (var corner = 0u; corner < 8u; corner++) {
        let ndc = vec2<f32>(select(x0, x1, (corner & 1u) != 0u), select(y0, y1, (corner & 2u) != 0u));
        let p = froxel_point(ndc, select(d0, d1, (corner & 4u) != 0u));
        lo = min(lo, p);
        hi = max(hi, p);
    }
    let center = 0.5 * (lo + hi);
    let half_size = 0.5 * (hi - lo);
    let radius = length(half_size);
    let capacity = dims.w;
    let base = cluster * capacity;
    var count = 0u;
    // Read-only storage values: uniform across the workgroup.
    let first = engine_lights.counts.y;
    let total = engine_lights.counts.x;
    for (var start = first; start < total; start += BATCH) {
        let index = start + local;
        if index < total {
            let light = engine_lights.lights[index];
            let position = (engine_view.view_from_world * vec4<f32>(light.position, 1.0)).xyz;
            let axis = (engine_view.view_from_world * vec4<f32>(light.direction, 0.0)).xyz;
            let extra = select(light.params.z, light.params.y, light.kind == ENGINE_LIGHT_SPOT);
            batch_spheres[local] = vec4<f32>(position, light.range);
            batch_axes[local] = vec4<f32>(axis, extra);
            batch_kinds[local] = light.kind;
        }
        workgroupBarrier();
        let size = min(BATCH, total - start);
        for (var j = 0u; j < size; j++) {
            let sphere = batch_spheres[j];
            let delta = clamp(sphere.xyz, lo, hi) - sphere.xyz;
            if dot(delta, delta) > sphere.w * sphere.w {
                continue;
            }
            let kind = batch_kinds[j];
            let axis = batch_axes[j];
            if kind == ENGINE_LIGHT_SPOT {
                // Cone against the froxel's bounding sphere.
                let to_center = center - sphere.xyz;
                let along = dot(to_center, axis.xyz);
                let sin_outer = sqrt(max(1.0 - axis.w * axis.w, 0.0));
                let lateral = sqrt(max(dot(to_center, to_center) - along * along, 0.0));
                if axis.w * lateral - along * sin_outer > radius || along < -radius {
                    continue;
                }
            }
            if kind == ENGINE_LIGHT_AREA && axis.w < 0.5 {
                // One-sided: nothing behind the emitting plane.
                if dot(center - sphere.xyz, axis.xyz) + dot(half_size, abs(axis.xyz)) < 0.0 {
                    continue;
                }
            }
            if valid && count < capacity {
                cluster_lights[base + count] = start + j;
            }
            count++;
        }
        workgroupBarrier();
    }
    if !valid {
        return;
    }
    grid.counts[cluster] = min(count, capacity);
    if count > capacity {
        atomicAdd(&stats[0], 1u);
    }
    atomicMax(&stats[1], count);
    atomicAdd(&stats[2], min(count, capacity));
    if count > 0u {
        atomicAdd(&stats[3], 1u);
    }
}
