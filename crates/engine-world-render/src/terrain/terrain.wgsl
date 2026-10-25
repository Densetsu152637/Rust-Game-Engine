// world.terrain: instanced CDLOD patches with GPU geomorphing from the height texture and
// splat-layer blending. The vertex stage mirrors engine_terrain::lod (morphed_height,
// coarse_height, morph_factor) exactly, so GPU and CPU meshes agree.
#import engine::lighting

struct TerrainUniform {
    world_from_local: mat4x4<f32>,
    normal_from_local: mat4x4<f32>,
    // x columns, y rows, z layer count, w reserved.
    dims: vec4<u32>,
    // x spacing.
    params: vec4<f32>,
    // Linear RGBA tint per layer.
    tints: array<vec4<f32>, 8>,
    // x world units per texture repeat, y 1 when the layer has a texture.
    layers: array<vec4<f32>, 8>,
}

@group(1) @binding(0) var<uniform> terrain: TerrainUniform;
// R32Float heights, one texel per sample (textureLoad only).
@group(1) @binding(1) var terrain_heights: texture_2d<f32>;
// Splat weights of layers 0-3 and 4-7, one texel per sample.
@group(1) @binding(2) var terrain_splat0: texture_2d<f32>;
@group(1) @binding(3) var terrain_splat1: texture_2d<f32>;
// One texel per cell, 1 for holes.
@group(1) @binding(4) var terrain_holes: texture_2d<f32>;
// Layer albedo textures (white for layers without one).
@group(1) @binding(5) var terrain_albedo: texture_2d_array<f32>;
@group(1) @binding(6) var terrain_clamp: sampler;
@group(1) @binding(7) var terrain_repeat: sampler;

struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) local_xz: vec2<f32>,
    @location(2) normal: vec3<f32>,
}

fn terrain_last() -> vec2<i32> {
    return vec2<i32>(i32(terrain.dims.x) - 1, i32(terrain.dims.y) - 1);
}

fn terrain_height(p: vec2<i32>) -> f32 {
    return textureLoad(terrain_heights, clamp(p, vec2<i32>(0), terrain_last()), 0).r;
}

// grid: vertex (i, 0, j) of the patch. node: origin x, origin z, step, level.
// morph: start, end, viewer gap. viewer: terrain-local viewer position.
@vertex fn vs_main(
    @location(0) grid: vec3<f32>,
    @location(4) node: vec4<u32>,
    @location(5) morph: vec4<f32>,
    @location(6) viewer: vec4<f32>,
) -> Output {
    let step = i32(node.z);
    let last = terrain_last();
    let v = min(vec2<i32>(node.xy) + vec2<i32>(i32(grid.x), i32(grid.z)) * step, last);
    let fine = terrain_height(v);
    let q = v / step;
    let odd = vec2<i32>(q.x & 1, q.y & 1);
    var coarse = fine;
    if odd.x == 1 && odd.y == 0 {
        coarse = 0.5 * (terrain_height(v - vec2<i32>(step, 0)) + terrain_height(v + vec2<i32>(step, 0)));
    } else if odd.x == 0 && odd.y == 1 {
        coarse = 0.5 * (terrain_height(v - vec2<i32>(0, step)) + terrain_height(v + vec2<i32>(0, step)));
    } else if odd.x == 1 && odd.y == 1 {
        coarse = 0.5 * (terrain_height(v + vec2<i32>(-step, step)) + terrain_height(v + vec2<i32>(step, -step)));
    }
    let spacing = terrain.params.x;
    let local_xz = vec2<f32>(v) * spacing;
    let delta = local_xz - viewer.xz;
    let distance = sqrt(dot(delta, delta) + morph.z * morph.z);
    let k = clamp((distance - morph.x) / (morph.y - morph.x), 0.0, 1.0);
    let height = fine + k * (coarse - fine);
    // Full-resolution sample normal (central differences, one-sided at the border).
    let left = max(v.x - 1, 0);
    let right = min(v.x + 1, last.x);
    let down = max(v.y - 1, 0);
    let up = min(v.y + 1, last.y);
    let dx = (terrain_height(vec2<i32>(right, v.y)) - terrain_height(vec2<i32>(left, v.y))) / (f32(right - left) * spacing);
    let dz = (terrain_height(vec2<i32>(v.x, up)) - terrain_height(vec2<i32>(v.x, down))) / (f32(up - down) * spacing);
    let n = normalize(vec3<f32>(-dx, 1.0, -dz));
    let world = terrain.world_from_local * vec4<f32>(local_xz.x, height, local_xz.y, 1.0);
    var out: Output;
    out.position = engine_view.clip_from_world * world;
    out.world = world.xyz;
    out.local_xz = local_xz;
    out.normal = normalize((terrain.normal_from_local * vec4<f32>(n, 0.0)).xyz);
    return out;
}

fn terrain_is_hole(local_xz: vec2<f32>) -> bool {
    let cells = terrain_last() - vec2<i32>(1);
    let cell = clamp(vec2<i32>(floor(local_xz / terrain.params.x)), vec2<i32>(0), cells);
    return textureLoad(terrain_holes, cell, 0).r > 0.5;
}

fn terrain_albedo_at(local_xz: vec2<f32>) -> vec3<f32> {
    let size = vec2<f32>(f32(terrain.dims.x), f32(terrain.dims.y));
    let uv = (local_xz / terrain.params.x + vec2<f32>(0.5)) / size;
    let w0 = textureSampleLevel(terrain_splat0, terrain_clamp, uv, 0.0);
    let w1 = textureSampleLevel(terrain_splat1, terrain_clamp, uv, 0.0);
    var weights = array<f32, 8>(w0.x, w0.y, w0.z, w0.w, w1.x, w1.y, w1.z, w1.w);
    var albedo = vec3<f32>(0.0);
    for (var i = 0u; i < terrain.dims.z; i++) {
        var color = terrain.tints[i];
        let layer = terrain.layers[i];
        let texel = textureSample(terrain_albedo, terrain_repeat, local_xz / layer.x, i32(i));
        color = select(color, color * texel, layer.y > 0.5);
        albedo += weights[i] * color.rgb;
    }
    return albedo;
}

@fragment fn fs_main(input: Output) -> @location(0) vec4<f32> {
    let albedo = terrain_albedo_at(input.local_xz);
    var surface: EngineSurface;
    surface.albedo = albedo;
    surface.metallic = 0.0;
    surface.roughness = 0.9;
    surface.normal = normalize(input.normal);
    let v = normalize(engine_camera_position() - input.world);
    let direct = engine_direct_lighting(surface, input.world, v, input.position.xy, true);
    let ambient = engine_indirect_lighting(surface, v, input.position.xy);
    if terrain_is_hole(input.local_xz) {
        discard;
    }
    return vec4<f32>(direct + ambient, 1.0);
}

// Normals prepass: world unit normal (xyz) and coverage (w), like engine.prepass.normals.
@fragment fn fs_normals(input: Output) -> @location(0) vec4<f32> {
    if terrain_is_hole(input.local_xz) {
        discard;
    }
    return vec4<f32>(normalize(input.normal), 1.0);
}
