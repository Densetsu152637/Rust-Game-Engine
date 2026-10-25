// Tier 2: software two-level BVH traversal in a compute shader. This is
// GPU-accelerated BVH traversal, NOT hardware ray tracing. It walks the same
// flat `BvhNode` arrays as the CPU tracer (engine-acoustics `gpu_layout`): mesh
// BVHs first, then the instance BVH starting at `settings.top_base`. Child and
// leaf indices are absolute within their tree's array; children follow their
// parent. Prepended to common.wgsl.

struct BvhNode {
    min: vec3<f32>,
    left_or_first: u32,
    max: vec3<f32>,
    count: u32,
}

@group(0) @binding(7) var<storage, read> nodes: array<BvhNode>;

const STACK_SIZE: u32 = 64u;
// Node visits per intersect call (both levels). A ray that exceeds it, or
// overflows a stack, is counted in `aborted` and terminated.
const MAX_VISITS: u32 = 65536u;

fn safe_inverse(d: vec3<f32>) -> vec3<f32> {
    let tiny = vec3<f32>(1e-30);
    let s = select(vec3<f32>(1.0), vec3<f32>(-1.0), d < vec3<f32>(0.0));
    let m = max(abs(d), tiny);
    return s / m;
}

// Entry distance of the ray into the node box, or a value > t_max on a miss.
fn box_entry(node: BvhNode, origin: vec3<f32>, inv: vec3<f32>, t_min: f32, t_max: f32) -> f32 {
    let a = (node.min - origin) * inv;
    let b = (node.max - origin) * inv;
    let lo = min(a, b);
    let hi = max(a, b);
    let enter = max(max(lo.x, lo.y), max(lo.z, t_min));
    let exit = min(min(hi.x, hi.y), min(hi.z, t_max));
    return select(t_max * 2.0 + 1.0, enter, enter <= exit);
}

// Double-sided Moller-Trumbore. Returns t in (t_min, t_max) or -1.
fn triangle_t(tri: Triangle, origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32) -> f32 {
    let e1 = tri.v1.xyz - tri.v0.xyz;
    let e2 = tri.v2.xyz - tri.v0.xyz;
    let p = cross(dir, e2);
    let det = dot(e1, p);
    if det == 0.0 {
        return -1.0;
    }
    let inv_det = 1.0 / det;
    let s = origin - tri.v0.xyz;
    let u = dot(s, p) * inv_det;
    if u < 0.0 || u > 1.0 {
        return -1.0;
    }
    let q = cross(s, e1);
    let v = dot(dir, q) * inv_det;
    if v < 0.0 || u + v > 1.0 {
        return -1.0;
    }
    let t = dot(e2, q) * inv_det;
    if t > t_min && t <= t_max {
        return t;
    }
    return -1.0;
}

fn transform_point(m: array<vec4<f32>, 3>, p: vec3<f32>) -> vec3<f32> {
    let h = vec4<f32>(p, 1.0);
    return vec3<f32>(dot(m[0], h), dot(m[1], h), dot(m[2], h));
}

fn transform_vector(m: array<vec4<f32>, 3>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(m[0].xyz, v), dot(m[1].xyz, v), dot(m[2].xyz, v));
}

fn intersect(origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32) -> Hit {
    var hit: Hit;
    hit.kind = HIT_MISS;
    hit.t = t_max;
    hit.triangle = 0u;
    hit.instance = 0u;
    var visits = 0u;
    let inv = safe_inverse(dir);
    var top_stack: array<u32, 64>;
    var top_sp = 1u;
    top_stack[0] = 0u;
    while top_sp > 0u {
        top_sp -= 1u;
        let top = nodes[settings.top_base + top_stack[top_sp]];
        visits += 1u;
        if visits > MAX_VISITS {
            hit.kind = HIT_ABORTED;
            return hit;
        }
        if box_entry(top, origin, inv, t_min, hit.t) > hit.t {
            continue;
        }
        if top.count == 0u {
            if top_sp + 2u > STACK_SIZE {
                hit.kind = HIT_ABORTED;
                return hit;
            }
            // Near child on top of the stack.
            let l = top.left_or_first;
            let near_left = box_entry(nodes[settings.top_base + l], origin, inv, t_min, hit.t)
                <= box_entry(nodes[settings.top_base + l + 1u], origin, inv, t_min, hit.t);
            top_stack[top_sp] = select(l, l + 1u, near_left);
            top_stack[top_sp + 1u] = select(l + 1u, l, near_left);
            top_sp += 2u;
            continue;
        }
        for (var k = 0u; k < top.count; k += 1u) {
            let instance_index = top.left_or_first + k;
            let instance = instances[instance_index];
            if (instance.mask & 0xffu) == 0u || instance.node_offset == 0xffffffffu {
                continue;
            }
            // Object-space ray with an unnormalized direction keeps t in world units.
            let o = transform_point(instance.object_from_world, origin);
            let d = transform_vector(instance.object_from_world, dir);
            let d_inv = safe_inverse(d);
            var stack: array<u32, 64>;
            var sp = 1u;
            stack[0] = instance.node_offset;
            while sp > 0u {
                sp -= 1u;
                let node = nodes[stack[sp]];
                visits += 1u;
                if visits > MAX_VISITS {
                    hit.kind = HIT_ABORTED;
                    return hit;
                }
                if box_entry(node, o, d_inv, t_min, hit.t) > hit.t {
                    continue;
                }
                if node.count == 0u {
                    if sp + 2u > STACK_SIZE {
                        hit.kind = HIT_ABORTED;
                        return hit;
                    }
                    let l = node.left_or_first;
                    let near_left = box_entry(nodes[l], o, d_inv, t_min, hit.t)
                        <= box_entry(nodes[l + 1u], o, d_inv, t_min, hit.t);
                    stack[sp] = select(l, l + 1u, near_left);
                    stack[sp + 1u] = select(l + 1u, l, near_left);
                    sp += 2u;
                    continue;
                }
                for (var j = 0u; j < node.count; j += 1u) {
                    let triangle = node.left_or_first + j;
                    let t = triangle_t(triangles[triangle], o, d, t_min, hit.t);
                    if t > 0.0 {
                        hit.kind = HIT_TRIANGLE;
                        hit.t = t;
                        hit.triangle = triangle;
                        hit.instance = instance_index;
                    }
                }
            }
        }
    }
    return hit;
}
