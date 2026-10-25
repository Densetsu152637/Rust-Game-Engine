// Desktop RT integration passes (no ray query): the RT G-buffer
// reconstruction, the screen-space SSAO fallback and the blit of an effect
// signal into its screen-lighting input. See docs/rendering-raytracing.md
// ("Frame integration").

struct Params {
    // Inverse jittered view-projection (G-buffer) / jittered clip_from_world (SSAO).
    matrix: mat4x4<f32>,
    view_from_world: mat4x4<f32>,
    // width, height, samples or blit mode, frame index.
    size: vec4<u32>,
    // SSAO radius, bias; blit roughness cutoff.
    values: vec4<f32>,
}

// ------------------------------------------------------------ G-buffer

@group(0) @binding(0) var g_depth: texture_depth_2d;
@group(0) @binding(1) var g_normals: texture_2d<f32>;
@group(0) @binding(2) var g_position: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> g_params: Params;

// World position (w 1) where the normal prepass covered the pixel, else 0.
@compute @workgroup_size(8, 8)
fn gbuffer(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = g_params.size.xy;
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let p = vec2<i32>(id.xy);
    let depth = textureLoad(g_depth, p, 0);
    let normal = textureLoad(g_normals, p, 0);
    if depth >= 1.0 || normal.w < 0.5 {
        textureStore(g_position, p, vec4<f32>(0.0));
        return;
    }
    let uv = (vec2<f32>(id.xy) + vec2<f32>(0.5)) / vec2<f32>(size);
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let world = g_params.matrix * clip;
    textureStore(g_position, p, vec4<f32>(world.xyz / world.w, 1.0));
}

// ---------------------------------------------------------------- SSAO

@group(0) @binding(4) var s_position: texture_2d<f32>;
@group(0) @binding(5) var s_normals: texture_2d<f32>;
@group(0) @binding(6) var s_output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(7) var<uniform> s_params: Params;

fn hash(value: u32) -> u32 {
    var x = value;
    x ^= x >> 16u;
    x *= 0x7feb352du;
    x ^= x >> 15u;
    x *= 0x846ca68bu;
    x ^= x >> 16u;
    return x;
}

fn unit(value: u32) -> f32 {
    return f32(hash(value) >> 8u) / 16777216.0;
}

// Hemisphere SSAO from the world positions and normals: `samples` cosine-
// weighted directions around the normal at distances up to `radius`; a
// sample counts as occluded when the visible surface at its projected pixel
// lies in front of it by more than `bias`, within `radius` of the pixel's
// surface. Output x = 1 - occluded / samples (1 = open).
@compute @workgroup_size(8, 8)
fn ssao(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = s_params.size.xy;
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let p = vec2<i32>(id.xy);
    let position = textureLoad(s_position, p, 0);
    let normal = textureLoad(s_normals, p, 0);
    if position.w < 0.5 || normal.w < 0.5 {
        textureStore(s_output, p, vec4<f32>(1.0, 0.0, 0.0, 0.0));
        return;
    }
    let n = normalize(normal.xyz);
    let helper = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), abs(n.x) > 0.9);
    let t = normalize(cross(helper, n));
    let b = cross(n, t);
    let radius = s_params.values.x;
    let bias = s_params.values.y;
    let samples = max(s_params.size.z, 1u);
    let seed = (id.y * size.x + id.x) * 9781u + s_params.size.w * 6271u;
    var occluded = 0.0;
    for (var i = 0u; i < samples; i++) {
        let u1 = unit(seed + i * 2u);
        let u2 = unit(seed + i * 2u + 1u);
        let r = sqrt(u1);
        let phi = 6.2831853 * u2;
        let local = vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(1.0 - u1, 0.0)));
        let dir = t * local.x + b * local.y + n * local.z;
        let k = (f32(i) + 0.5) / f32(samples);
        let distance = radius * mix(0.1, 1.0, k * k);
        let sample_point = position.xyz + n * bias + dir * distance;
        let clip = s_params.matrix * vec4<f32>(sample_point, 1.0);
        if clip.w <= 0.0 {
            continue;
        }
        let ndc = clip.xy / clip.w;
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        if any(uv < vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) {
            continue;
        }
        let q = vec2<i32>(uv * vec2<f32>(size));
        let scene = textureLoad(s_position, q, 0);
        if scene.w < 0.5 {
            continue;
        }
        let sample_depth = -(s_params.view_from_world * vec4<f32>(sample_point, 1.0)).z;
        let scene_depth = -(s_params.view_from_world * vec4<f32>(scene.xyz, 1.0)).z;
        let in_range = length(scene.xyz - position.xyz) < radius;
        if scene_depth < sample_depth - bias && in_range {
            occluded += 1.0;
        }
    }
    textureStore(s_output, p, vec4<f32>(1.0 - occluded / f32(samples), 0.0, 0.0, 0.0));
}

// ---------------------------------------------------------------- blit

@group(0) @binding(8) var b_input: texture_2d<f32>;
@group(0) @binding(9) var b_output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(10) var<uniform> b_params: Params;

// Mode 0 shadow (x, replace flag 1), 1 ambient occlusion (x), 2 indirect
// (rgba), 3 reflection (rgb, roughness cutoff where traced, else a = -1).
@compute @workgroup_size(8, 8)
fn blit(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = b_params.size.xy;
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let p = vec2<i32>(id.xy);
    let value = textureLoad(b_input, p, 0);
    var out = value;
    switch b_params.size.z {
        case 0u: {
            out = vec4<f32>(value.x, 1.0, 0.0, 0.0);
        }
        case 1u: {
            out = vec4<f32>(value.x, 0.0, 0.0, 0.0);
        }
        case 3u: {
            if value.a >= 0.0 {
                out = vec4<f32>(value.rgb, b_params.values.z);
            } else {
                out = vec4<f32>(0.0, 0.0, 0.0, -1.0);
            }
        }
        default: {}
    }
    textureStore(b_output, p, out);
}
