// Spatio-temporal denoiser (SVGF-style; Schied et al. 2017), shared by its
// passes. Plain compute: no ray query. Every pass binds the parameters at
// binding 0 and the G-buffer at bindings 1 (world position) and 2 (world
// normal) of group 0. Working textures are Rgba32Float.

struct DnParams {
    // xyz: camera position.
    eye: vec4<f32>,
    // x: reprojection depth tolerance (relative), y: reprojection normal
    // threshold (cosine), z: max history length (frames).
    temporal: vec4<f32>,
    // x: phi depth, y: phi normal, z: phi luminance.
    edge: vec4<f32>,
    // x, y: size; z: signal kind (DN_*); w: 1 = ignore history (first frame
    // or camera cut).
    size: vec4<u32>,
    // x: a-trous step width in pixels; y: min history for temporal variance.
    pass_info: vec4<u32>,
}

const DN_SCALAR: u32 = 0u;
const DN_RGB: u32 = 1u;
const DN_RGB_HIT: u32 = 2u;

@group(0) @binding(0) var<uniform> dn: DnParams;
@group(0) @binding(1) var dn_world_position: texture_2d<f32>;
@group(0) @binding(2) var dn_world_normal: texture_2d<f32>;

struct DnSurface {
    valid: bool,
    position: vec3<f32>,
    normal: vec3<f32>,
    // Distance from the camera.
    depth: f32,
}

fn dn_in_bounds(q: vec2<i32>) -> bool {
    return q.x >= 0 && q.y >= 0 && q.x < i32(dn.size.x) && q.y < i32(dn.size.y);
}

fn dn_surface(q: vec2<i32>) -> DnSurface {
    let p = textureLoad(dn_world_position, q, 0);
    let n = textureLoad(dn_world_normal, q, 0).xyz;
    var s: DnSurface;
    s.valid = p.w >= 0.5 && dot(n, n) > 1e-12;
    if s.valid {
        s.position = p.xyz;
        s.normal = normalize(n);
        s.depth = max(distance(p.xyz, dn.eye.xyz), 1e-6);
    }
    return s;
}

// The filtered channels of a raw signal texel.
fn dn_signal(raw: vec4<f32>) -> vec3<f32> {
    if dn.size.z == DN_SCALAR {
        return vec3<f32>(raw.x, 0.0, 0.0);
    }
    return raw.xyz;
}

fn dn_luminance(c: vec3<f32>) -> f32 {
    if dn.size.z == DN_SCALAR {
        return c.x;
    }
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Edge stop on the distance of q from p's tangent plane, relative to p's
// distance from the camera.
fn dn_depth_weight(p: DnSurface, q: DnSurface) -> f32 {
    let plane = abs(dot(p.normal, q.position - p.position));
    return exp(-plane / (dn.edge.x * p.depth));
}

fn dn_normal_weight(p: DnSurface, q: DnSurface) -> f32 {
    return pow(max(0.0, dot(p.normal, q.normal)), dn.edge.y);
}
