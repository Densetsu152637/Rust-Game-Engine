// engine-render-rt ray-query library (docs/rendering-raytracing.md, "WGSL library").
//
// Composed by `engine_render_rt::wgsl::library(group)`, which replaces RT_GROUP
// below with the scene bind group index. The final module must start with
// `enable wgpu_ray_query;` (`wgsl::compose` adds it).
//
// Geometry is opaque; instance masks are 8 bits. `t` is in units of `dir`
// (world distance when `dir` is a unit vector).

struct RtInstanceData {
    geometry: u32,
    material: u32,
    custom_index: u32,
    flags: u32,
}

struct RtGeometryData {
    vertex_offset: u32,
    index_offset: u32,
    triangle_count: u32,
    flags: u32,
}

const RT_GEOMETRY_HAS_NORMALS: u32 = 1u;

@group(RT_GROUP) @binding(0) var rt_tlas: acceleration_structure;
@group(RT_GROUP) @binding(1) var<storage, read> rt_instances: array<RtInstanceData>;
@group(RT_GROUP) @binding(2) var<storage, read> rt_geometries: array<RtGeometryData>;
@group(RT_GROUP) @binding(3) var<storage, read> rt_indices: array<u32>;
@group(RT_GROUP) @binding(4) var<storage, read> rt_positions: array<f32>;
@group(RT_GROUP) @binding(5) var<storage, read> rt_normals: array<f32>;

// Closest hit of one ray. `hit == false` leaves the other fields zero except
// `t = t_max`.
struct RtHit {
    hit: bool,
    t: f32,
    // Barycentric weights of vertices 1 and 2; vertex 0 has 1 - x - y.
    barycentrics: vec2<f32>,
    // The instance's 24-bit custom index (RtInstanceDesc::custom_index).
    custom_index: u32,
    // TLAS slot; indexes rt_instances.
    instance_index: u32,
    // Triangle index within the mesh.
    primitive_index: u32,
    front_face: bool,
    object_to_world: mat4x3<f32>,
    world_to_object: mat4x3<f32>,
}

// Surface attributes of a hit, fetched from the scene buffers.
struct RtSurface {
    position: vec3<f32>,
    // Unit world-space normal of the object-space winding (v1 - v0) x (v2 - v0);
    // not face-forwarded.
    geometric_normal: vec3<f32>,
    // Interpolated unit world-space vertex normal, or geometric_normal when the
    // mesh has no normals.
    shading_normal: vec3<f32>,
    material_index: u32,
    geometry_index: u32,
}

// Any-hit visibility: true when anything with a matching mask lies in
// [t_min, t_max] along the ray. Terminates on the first hit.
fn rt_occluded(origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32, mask: u32) -> bool {
    var rq: ray_query;
    rayQueryInitialize(&rq, rt_tlas, RayDesc(RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_TERMINATE_ON_FIRST_HIT, mask & 0xffu, t_min, t_max, origin, dir));
    loop {
        if !rayQueryProceed(&rq) {
            break;
        }
    }
    return rayQueryGetCommittedIntersection(&rq).kind != RAY_QUERY_INTERSECTION_NONE;
}

fn rt_closest_hit(origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32, mask: u32) -> RtHit {
    var rq: ray_query;
    rayQueryInitialize(&rq, rt_tlas, RayDesc(RAY_FLAG_FORCE_OPAQUE, mask & 0xffu, t_min, t_max, origin, dir));
    loop {
        if !rayQueryProceed(&rq) {
            break;
        }
    }
    let x = rayQueryGetCommittedIntersection(&rq);
    var hit: RtHit;
    hit.t = t_max;
    if x.kind == RAY_QUERY_INTERSECTION_TRIANGLE {
        hit.hit = true;
        hit.t = x.t;
        hit.barycentrics = x.barycentrics;
        hit.custom_index = x.instance_custom_data;
        hit.instance_index = x.instance_index;
        hit.primitive_index = x.primitive_index;
        hit.front_face = x.front_face;
        hit.object_to_world = x.object_to_world;
        hit.world_to_object = x.world_to_object;
    }
    return hit;
}

fn rt_vec3(buffer_base: u32, from_normals: bool) -> vec3<f32> {
    if from_normals {
        return vec3<f32>(rt_normals[buffer_base], rt_normals[buffer_base + 1u], rt_normals[buffer_base + 2u]);
    }
    return vec3<f32>(rt_positions[buffer_base], rt_positions[buffer_base + 1u], rt_positions[buffer_base + 2u]);
}

// Object-space vertex indices (already offset into the shared buffers) of a
// hit triangle.
fn rt_triangle_vertices(geometry: RtGeometryData, primitive: u32) -> vec3<u32> {
    let base = geometry.index_offset + primitive * 3u;
    return vec3<u32>(rt_indices[base], rt_indices[base + 1u], rt_indices[base + 2u]) + vec3<u32>(geometry.vertex_offset);
}

// Surface of a hit returned by rt_closest_hit for the same origin and dir.
fn rt_surface(hit: RtHit, origin: vec3<f32>, dir: vec3<f32>) -> RtSurface {
    let instance = rt_instances[hit.instance_index];
    let geometry = rt_geometries[instance.geometry];
    let v = rt_triangle_vertices(geometry, hit.primitive_index);
    let p0 = rt_vec3(v.x * 3u, false);
    let p1 = rt_vec3(v.y * 3u, false);
    let p2 = rt_vec3(v.z * 3u, false);
    // Normals transform with the inverse transpose: n_world = transpose(W) n_obj
    // where W is the linear part of world_to_object.
    let w = mat3x3<f32>(hit.world_to_object[0], hit.world_to_object[1], hit.world_to_object[2]);
    var surface: RtSurface;
    surface.position = origin + dir * hit.t;
    surface.geometric_normal = normalize(cross(p1 - p0, p2 - p0) * w);
    surface.shading_normal = surface.geometric_normal;
    if (geometry.flags & RT_GEOMETRY_HAS_NORMALS) != 0u {
        let b = hit.barycentrics;
        let n = rt_vec3(v.x * 3u, true) * (1.0 - b.x - b.y) + rt_vec3(v.y * 3u, true) * b.x + rt_vec3(v.z * 3u, true) * b.y;
        surface.shading_normal = normalize(n * w);
    }
    surface.material_index = instance.material;
    surface.geometry_index = instance.geometry;
    return surface;
}
