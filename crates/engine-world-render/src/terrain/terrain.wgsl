// world.terrain: instanced CDLOD patches with GPU geomorphing from the height texture and
// splat-layer blending. The vertex stage mirrors engine_terrain::lod (morphed_height,
// coarse_height, morph_factor) exactly, so GPU and CPU meshes agree. The same surface feeds
// the colour pass (vs_main / fs_main), the normals prepass (fs_normals), the terrain shadow
// map (vs_shadow / fs_shadow) and the motion pass (vs_motion / fs_motion).
#import engine::lighting

struct TerrainUniform {
    world_from_local: mat4x4<f32>,
    normal_from_local: mat4x4<f32>,
    // world_from_local of the view's previous frame (motion vectors).
    previous_world_from_local: mat4x4<f32>,
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

// Terrain shadow map of the view (colour pass only): one layer per cascade of the first
// directional light, rendered with the engine's cascade matrices by world.terrain.shadow.
struct TerrainShadowParams {
    // x: 1 when the map holds this frame's terrain depth; y: layer edge in texels.
    params: vec4<f32>,
}
@group(2) @binding(0) var terrain_shadow_map: texture_depth_2d_array;
@group(2) @binding(1) var terrain_shadow_sampler: sampler_comparison;
@group(2) @binding(2) var<uniform> terrain_shadow: TerrainShadowParams;
// Shadow pass only: x = the cascade being rendered.
@group(2) @binding(3) var<uniform> terrain_shadow_cascade: vec4<u32>;

struct Output {
    @builtin(position) @invariant position: vec4<f32>,
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

// One patch vertex: sample coordinate, fine and coarse heights.
struct TerrainSample {
    v: vec2<i32>,
    local_xz: vec2<f32>,
    fine: f32,
    coarse: f32,
}

// grid: vertex (i, 0, j) of the patch. node: origin x, origin z, step, level.
fn terrain_sample(grid: vec3<f32>, node: vec4<u32>) -> TerrainSample {
    let step = i32(node.z);
    let v = min(vec2<i32>(node.xy) + vec2<i32>(i32(grid.x), i32(grid.z)) * step, terrain_last());
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
    var s: TerrainSample;
    s.v = v;
    s.local_xz = vec2<f32>(v) * terrain.params.x;
    s.fine = fine;
    s.coarse = coarse;
    return s;
}

// Geomorphed height for a viewer at local `viewer` with vertical gap `gap`.
// morph: start, end.
fn terrain_morphed(s: TerrainSample, morph: vec2<f32>, viewer: vec2<f32>, gap: f32) -> f32 {
    let delta = s.local_xz - viewer;
    let distance = sqrt(dot(delta, delta) + gap * gap);
    let k = clamp((distance - morph.x) / (morph.y - morph.x), 0.0, 1.0);
    return s.fine + k * (s.coarse - s.fine);
}

// Full-resolution sample normal (central differences, one-sided at the border), local space.
fn terrain_local_normal(v: vec2<i32>) -> vec3<f32> {
    let last = terrain_last();
    let spacing = terrain.params.x;
    let left = max(v.x - 1, 0);
    let right = min(v.x + 1, last.x);
    let down = max(v.y - 1, 0);
    let up = min(v.y + 1, last.y);
    let dx = (terrain_height(vec2<i32>(right, v.y)) - terrain_height(vec2<i32>(left, v.y))) / (f32(right - left) * spacing);
    let dz = (terrain_height(vec2<i32>(v.x, up)) - terrain_height(vec2<i32>(v.x, down))) / (f32(up - down) * spacing);
    return normalize(vec3<f32>(-dx, 1.0, -dz));
}

// World position of a patch vertex for the instance's current viewer.
// morph: start, end, viewer gap. viewer: terrain-local viewer position.
fn terrain_world(s: TerrainSample, morph: vec4<f32>, viewer: vec4<f32>) -> vec4<f32> {
    let height = terrain_morphed(s, morph.xy, viewer.xz, morph.z);
    return terrain.world_from_local * vec4<f32>(s.local_xz.x, height, s.local_xz.y, 1.0);
}

@vertex fn vs_main(
    @location(0) grid: vec3<f32>,
    @location(4) node: vec4<u32>,
    @location(5) morph: vec4<f32>,
    @location(6) viewer: vec4<f32>,
) -> Output {
    let s = terrain_sample(grid, node);
    let world = terrain_world(s, morph, viewer);
    var out: Output;
    out.position = engine_view.clip_from_world * world;
    out.world = world.xyz;
    out.local_xz = s.local_xz;
    out.normal = normalize((terrain.normal_from_local * vec4<f32>(terrain_local_normal(s.v), 0.0)).xyz);
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

// ------------------------------------------------------------ terrain shadow map

// Visibility of `world` (normal `n`) in terrain shadow layer `layer`, rendered with
// engine_shadows[index]'s orthographic matrix. The engine's depth and normal bias (in
// texels) are applied with this map's texel size. 3 x 3 PCF; outside the view: lit.
fn terrain_shadow_view(index: u32, layer: u32, world: vec3<f32>, n: vec3<f32>) -> f32 {
    let shadow = engine_shadows[index];
    let m = shadow.clip_from_world;
    // An orthographic cascade spans 2 / |row 0| world units over the layer edge.
    let row = vec3<f32>(m[0].x, m[1].x, m[2].x);
    let size = terrain_shadow.params.y;
    let texel = 2.0 / (length(row) * size);
    let p = m * vec4<f32>(world + n * (shadow.bias.y * texel), 1.0);
    let ndc = p.xyz / p.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || ndc.z > 1.0 {
        return 1.0;
    }
    let reference = max(ndc.z, 0.0) - shadow.bias.x * texel / max(shadow.range.y - shadow.range.x, 0.000001);
    var sum = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let at = clamp(uv + vec2<f32>(f32(x), f32(y)) / size, vec2<f32>(0.0), vec2<f32>(1.0));
            sum += textureSampleCompareLevel(terrain_shadow_map, terrain_shadow_sampler, at, i32(layer), reference);
        }
    }
    return sum / 9.0;
}

// Fraction of the first directional light reaching `world` past the terrain itself: the
// engine's cascade selection and blend over the terrain shadow map. 1 without one.
fn terrain_shadow_visibility(world: vec3<f32>, n: vec3<f32>) -> f32 {
    if terrain_shadow.params.x < 0.5 || engine_lights.counts.y == 0u {
        return 1.0;
    }
    let light = engine_lights.lights[0];
    let cascade = engine_cascade_index(light, world);
    if cascade < 0 {
        return 1.0;
    }
    let base = u32(light.shadow_index);
    let index = base + u32(cascade);
    var visibility = terrain_shadow_view(index, u32(cascade), world, n);
    let blend = light.params.y;
    let split = engine_shadows[index].range;
    let start = split.w - blend * (split.w - split.z);
    let depth = engine_view_depth(world);
    if blend > 0.0 && depth > start {
        var next = 1.0;
        if u32(cascade) + 1u < u32(light.params.x) {
            next = terrain_shadow_view(index + 1u, u32(cascade) + 1u, world, n);
        }
        visibility = mix(visibility, next, clamp((depth - start) / max(split.w - start, 0.000001), 0.0, 1.0));
    }
    return visibility;
}

// engine_direct_lighting with the terrain's own shadow multiplied into the first directional
// light's screen visibility (the engine shadow atlas holds no terrain).
fn terrain_direct_lighting(surface: EngineSurface, world: vec3<f32>, v: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    let directional = engine_lights.counts.y;
    if directional > 0u {
        var screen = engine_screen_shadow_at(frag).xy;
        screen.x *= terrain_shadow_visibility(world, surface.normal);
        sum += engine_light_contribution_screen(engine_lights.lights[0], surface, world, v, true, screen);
    }
    for (var i = 1u; i < directional; i++) {
        sum += engine_light_contribution(engine_lights.lights[i], surface, world, v, true);
    }
    if engine_clusters.params.x == 1u {
        for (var i = directional; i < engine_lights.counts.x; i++) {
            sum += engine_light_contribution(engine_lights.lights[i], surface, world, v, true);
        }
        return sum;
    }
    let cluster = engine_cluster_index(frag, engine_view_depth(world));
    let count = engine_clusters.counts[cluster];
    let first = cluster * engine_clusters.dims.w;
    for (var i = 0u; i < count; i++) {
        let light = engine_lights.lights[engine_cluster_lights[first + i]];
        sum += engine_light_contribution(light, surface, world, v, true);
    }
    return sum;
}

@fragment fn fs_main(input: Output) -> @location(0) vec4<f32> {
    let albedo = terrain_albedo_at(input.local_xz);
    var surface: EngineSurface;
    surface.albedo = albedo;
    surface.metallic = 0.0;
    surface.roughness = 0.9;
    surface.normal = normalize(input.normal);
    let v = normalize(engine_camera_position() - input.world);
    let direct = terrain_direct_lighting(surface, input.world, v, input.position.xy);
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

struct ShadowOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) local_xz: vec2<f32>,
}

// Terrain depth into shadow layer `terrain_shadow_cascade.x` with the matrix of that cascade
// of the first directional light; cascades the light does not have are clipped away.
@vertex fn vs_shadow(
    @location(0) grid: vec3<f32>,
    @location(4) node: vec4<u32>,
    @location(5) morph: vec4<f32>,
    @location(6) viewer: vec4<f32>,
) -> ShadowOutput {
    let s = terrain_sample(grid, node);
    let world = terrain_world(s, morph, viewer);
    var out: ShadowOutput;
    out.local_xz = s.local_xz;
    out.position = vec4<f32>(0.0, 0.0, -1.0, 1.0);
    let cascade = terrain_shadow_cascade.x;
    if engine_lights.counts.y > 0u {
        let light = engine_lights.lights[0];
        if light.shadow_index >= 0 && cascade < u32(light.params.x) {
            out.position = engine_shadows[u32(light.shadow_index) + cascade].clip_from_world * world;
        }
    }
    return out;
}

// Holes cast no shadow.
@fragment fn fs_shadow(input: ShadowOutput) {
    if terrain_is_hole(input.local_xz) {
        discard;
    }
}

struct MotionOutput {
    @builtin(position) @invariant position: vec4<f32>,
    @location(0) local_xz: vec2<f32>,
    @location(1) current: vec4<f32>,
    @location(2) previous: vec4<f32>,
}

// Motion of a terrain vertex: rasterized exactly like vs_main (so it passes the LessEqual
// test against the colour pass's depth), current position from the unjittered matrix,
// previous position from the previous frame's viewer (geomorph) and terrain transform under
// the previous view-projection. previous_viewer: local position and gap of the previous frame.
@vertex fn vs_motion(
    @location(0) grid: vec3<f32>,
    @location(4) node: vec4<u32>,
    @location(5) morph: vec4<f32>,
    @location(6) viewer: vec4<f32>,
    @location(7) previous_viewer: vec4<f32>,
) -> MotionOutput {
    let s = terrain_sample(grid, node);
    let world = terrain_world(s, morph, viewer);
    let previous_height = terrain_morphed(s, morph.xy, previous_viewer.xz, previous_viewer.w);
    let previous_world = terrain.previous_world_from_local
        * vec4<f32>(s.local_xz.x, previous_height, s.local_xz.y, 1.0);
    var out: MotionOutput;
    out.position = engine_view.clip_from_world * world;
    out.local_xz = s.local_xz;
    out.current = engine_view.unjittered_clip_from_world * world;
    out.previous = engine_view.previous_clip_from_world * previous_world;
    return out;
}

// Current-to-previous motion in render pixels (y down), like engine.prepass.motion.
@fragment fn fs_motion(input: MotionOutput) -> @location(0) vec4<f32> {
    if terrain_is_hole(input.local_xz) {
        discard;
    }
    var delta = vec2<f32>(0.0);
    if input.current.w > 0.000001 && input.previous.w > 0.000001 {
        let current = input.current.xy / input.current.w;
        let previous = input.previous.xy / input.previous.w;
        delta = (previous - current) * vec2<f32>(0.5, -0.5) * engine_view.render_size.xy;
    }
    return vec4<f32>(delta, 0.0, 1.0);
}
