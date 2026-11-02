// engine.decals: oriented-box decals projected onto scene depth, applied to
// the lit HDR scene colour (OpaqueEffects stage).
//
// One draw per decal: a fullscreen triangle scissored to the decal's screen
// rectangle. The destination already holds the previous colour, so pixels the
// decal rejects are discarded and keep it.
#import engine::fullscreen

struct DecalParams {
    // Inverse of the jittered view-projection.
    world_from_clip: mat4x4<f32>,
    // Box-local coordinates; the decal volume is [-1, 1]^3 there.
    box_from_world: mat4x4<f32>,
    // World-space projection axis (local +Z).
    axis: vec4<f32>,
    // rgb albedo, a opacity.
    albedo_opacity: vec4<f32>,
    // rgb emissive, w blend (0 multiply, 1 paint).
    emissive_blend: vec4<f32>,
    // x edge fade, y cos(max angle), z cos(max angle - angle fade), w textured.
    params: vec4<f32>,
    // xy render size, zw reciprocals.
    size: vec4<f32>,
}

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var scene_depth: texture_depth_2d;
@group(0) @binding(2) var scene_normals: texture_2d<f32>;
@group(0) @binding(3) var<uniform> decal: DecalParams;
@group(0) @binding(4) var decal_texture: texture_2d<f32>;
@group(0) @binding(5) var decal_sampler: sampler;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return engine_fullscreen_position(index);
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(frag.xy);
    let depth = textureLoad(scene_depth, pixel, 0);
    // Sky and cleared depth are not surfaces.
    if depth >= 1.0 {
        discard;
    }
    // World position from depth.
    let uv = (frag.xy) * decal.size.zw;
    let ndc = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let world_h = decal.world_from_clip * ndc;
    let world = world_h.xyz / world_h.w;
    // Reject outside the unit box.
    let local = (decal.box_from_world * vec4<f32>(world, 1.0)).xyz;
    let inside = abs(local);
    if inside.x > 1.0 || inside.y > 1.0 || inside.z > 1.0 {
        discard;
    }
    // Edge fade: distance to the nearest box face as a fraction of the half extent.
    var edge = 1.0;
    let edge_fade = decal.params.x;
    if edge_fade > 0.0 {
        let to_face = 1.0 - max(inside.x, max(inside.y, inside.z));
        edge = smoothstep(0.0, edge_fade, to_face);
    }
    // Angle fade: the surface normal against the projection axis. Pixels
    // without a normal (coverage 0) skip the fade.
    var angle = 1.0;
    let n = textureLoad(scene_normals, pixel, 0);
    if n.w > 0.5 {
        let c = dot(normalize(n.xyz), decal.axis.xyz);
        let hard = decal.params.y;
        let soft = decal.params.z;
        if c <= 0.0 {
            angle = 0.0;
        } else if soft > hard + 1e-6 {
            angle = smoothstep(hard, soft, c);
        } else {
            angle = select(0.0, 1.0, c >= hard);
        }
    }
    var color = vec3<f32>(1.0);
    var alpha = 1.0;
    if decal.params.w > 0.5 {
        let t = textureSampleLevel(
            decal_texture, decal_sampler, vec2<f32>(local.x * 0.5 + 0.5, 0.5 - local.y * 0.5), 0.0);
        color = t.rgb;
        alpha = t.a;
    }
    // Coverage: where the decal applies at all (texture alpha, edge, angle).
    let cover = alpha * edge * angle;
    if cover <= 0.0 {
        discard;
    }
    let mask = decal.albedo_opacity.a * cover;
    let albedo = decal.albedo_opacity.rgb * color;
    let src = textureLoad(scene, pixel, 0);
    var rgb: vec3<f32>;
    if decal.emissive_blend.w < 0.5 {
        rgb = src.rgb * mix(vec3<f32>(1.0), albedo, mask);
    } else {
        rgb = mix(src.rgb, albedo * luma(src.rgb), mask);
    }
    rgb += decal.emissive_blend.rgb * cover;
    return vec4<f32>(rgb, src.a);
}
