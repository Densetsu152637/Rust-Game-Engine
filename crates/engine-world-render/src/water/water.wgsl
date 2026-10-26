// world.water: displaced water surfaces with Fresnel sky/sun reflection, screen-space
// refraction of the HDR scene, depth-based absorption and crest/shoreline foam. The vertex
// stage mirrors engine_water (GerstnerSet::evaluate and OceanFrame::texel_at) so the GPU
// surface matches the CPU queries.
#import engine::view

struct WaterUniform {
    world_from_local: mat4x4<f32>,
    // x, y: half extent; z: grid cells per side; w: mode (0 flat, 1 Gerstner, 2 ocean, 3 river).
    params: vec4<f32>,
    // x: wave time; y: Gerstner wave count; z: ocean patch length; w: ocean grid side.
    params2: vec4<f32>,
    // rgb shallow colour, w refraction strength.
    shallow: vec4<f32>,
    // rgb deep colour, w roughness.
    deep: vec4<f32>,
    // rgb absorption per metre, w crest foam threshold.
    absorption: vec4<f32>,
    // rgb foam colour, w foam intensity.
    foam: vec4<f32>,
    // rgb fog colour, w fog density.
    fog: vec4<f32>,
    // x: shore foam depth; y: 1 with a per-vertex shore texture.
    shore: vec4<f32>,
    // Gerstner: [dir.x, dir.z, k, omega] and [amplitude, chop, phase, 0].
    waves_a: array<vec4<f32>, 16>,
    waves_b: array<vec4<f32>, 16>,
}

@group(1) @binding(0) var<uniform> water: WaterUniform;
// Ocean frame: (dx, h, dz, Jacobian determinant) and (normal, 0), one texel per grid sample.
@group(1) @binding(1) var ocean_displacement: texture_2d<f32>;
@group(1) @binding(2) var ocean_normal: texture_2d<f32>;
// Per grid vertex (amplitude factor, shoreline foam).
@group(1) @binding(3) var shore_grid: texture_2d<f32>;
// Copies of the HDR scene colour and depth taken before the water pass.
@group(1) @binding(4) var scene_color: texture_2d<f32>;
@group(1) @binding(5) var scene_depth: texture_depth_2d;
@group(1) @binding(6) var linear_clamp: sampler;
@group(2) @binding(0) var<uniform> shadow_index: vec4<u32>;

struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) foam: f32,
}

struct Displaced {
    offset: vec3<f32>,
    normal: vec3<f32>,
    det: f32,
}

fn displaced_normal(jx: vec2<f32>, jz: vec2<f32>, slope: vec2<f32>) -> vec3<f32> {
    let tx = vec3<f32>(1.0 + jx.x, slope.x, jx.y);
    let tz = vec3<f32>(jz.x, slope.y, 1.0 + jz.y);
    return normalize(cross(tz, tx));
}

fn gerstner(x0: vec2<f32>, scale: f32) -> Displaced {
    var offset = vec3<f32>(0.0);
    var jx = vec2<f32>(0.0);
    var jz = vec2<f32>(0.0);
    var slope = vec2<f32>(0.0);
    let t = water.params2.x;
    let count = u32(water.params2.y);
    for (var i = 0u; i < count; i++) {
        let a4 = water.waves_a[i];
        let b4 = water.waves_b[i];
        let d = a4.xy;
        let k = a4.z;
        let theta = k * dot(d, x0) - a4.w * t + b4.z;
        let s = sin(theta);
        let c = cos(theta);
        let a = b4.x * scale;
        let q = b4.y * scale;
        offset += vec3<f32>(q * d.x * c, a * s, q * d.y * c);
        let j = -q * k * s;
        jx += vec2<f32>(j * d.x * d.x, j * d.x * d.y);
        jz += vec2<f32>(j * d.x * d.y, j * d.y * d.y);
        slope += a * k * c * d;
    }
    var out: Displaced;
    out.offset = offset;
    out.normal = displaced_normal(jx, jz, slope);
    out.det = (1.0 + jx.x) * (1.0 + jz.y) - jz.x * jx.y;
    return out;
}

fn ocean_texel(tex: texture_2d<f32>, c: vec2<i32>) -> vec4<f32> {
    let n = i32(water.params2.w);
    return textureLoad(tex, ((c % n) + n) % n, 0);
}

// Periodic bilinear lookup at rest position x0 (like OceanFrame::texel_at).
fn ocean_sample(tex: texture_2d<f32>, x0: vec2<f32>) -> vec4<f32> {
    let g = x0 * (water.params2.w / water.params2.z);
    let base = floor(g);
    let f = g - base;
    let c = vec2<i32>(base);
    let top = mix(ocean_texel(tex, c), ocean_texel(tex, c + vec2<i32>(1, 0)), f.x);
    let bottom = mix(ocean_texel(tex, c + vec2<i32>(0, 1)), ocean_texel(tex, c + vec2<i32>(1, 1)), f.x);
    return mix(top, bottom, f.y);
}

fn crest_foam(det: f32) -> f32 {
    let threshold = water.absorption.w;
    if threshold <= 0.0 {
        return select(0.0, 1.0, det <= 0.0);
    }
    return clamp((threshold - det) / threshold, 0.0, 1.0);
}

// position: unit grid vertex (x, z in 0..1) or, for rivers, the local ribbon vertex.
fn water_output(position: vec3<f32>) -> Output {
    let mode = u32(water.params.w);
    var local = position;
    var normal = vec3<f32>(0.0, 1.0, 0.0);
    var foam = 0.0;
    if mode != 3u {
        let half = water.params.xy;
        let x0 = -half + 2.0 * half * position.xz;
        var scale = 1.0;
        var shore_foam = 0.0;
        if water.shore.y > 0.5 {
            let cell = vec2<i32>(round(position.xz * water.params.z));
            let s = textureLoad(shore_grid, cell, 0).xy;
            scale = s.x;
            shore_foam = s.y;
        }
        var d: Displaced;
        d.offset = vec3<f32>(0.0);
        d.normal = vec3<f32>(0.0, 1.0, 0.0);
        d.det = 1.0;
        if mode == 1u {
            d = gerstner(x0, scale);
        } else if mode == 2u {
            let t = ocean_sample(ocean_displacement, x0);
            d.offset = t.xyz * scale;
            d.det = 1.0 + (t.w - 1.0) * scale;
            let n = ocean_sample(ocean_normal, x0).xyz;
            d.normal = normalize(mix(vec3<f32>(0.0, 1.0, 0.0), n, scale));
        }
        local = vec3<f32>(x0.x + d.offset.x, d.offset.y, x0.y + d.offset.z);
        normal = d.normal;
        foam = max(crest_foam(d.det), shore_foam);
    }
    let world = water.world_from_local * vec4<f32>(local, 1.0);
    var out: Output;
    out.position = engine_view.clip_from_world * world;
    out.world = world.xyz;
    out.normal = normalize((water.world_from_local * vec4<f32>(normal, 0.0)).xyz);
    out.foam = foam;
    return out;
}

@vertex fn vs_main(@location(0) position: vec3<f32>) -> Output {
    return water_output(position);
}

@vertex fn vs_shadow(@location(0) position: vec3<f32>) -> Output {
    var out = water_output(position);
    out.position = engine_shadows[shadow_index.x].clip_from_world * vec4<f32>(out.world, 1.0);
    return out;
}

@fragment fn fs_shadow(input: Output) {}

fn world_at(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let p = engine_view.world_from_clip * vec4<f32>(ndc, depth, 1.0);
    return p.xyz / p.w;
}

fn depth_at(uv: vec2<f32>) -> f32 {
    let size = vec2<i32>(textureDimensions(scene_depth));
    let texel = clamp(vec2<i32>(uv * vec2<f32>(size)), vec2<i32>(0), size - vec2<i32>(1));
    return textureLoad(scene_depth, texel, 0);
}

@fragment fn fs_main(input: Output, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let camera = engine_camera_position();
    var n = normalize(input.normal);
    if !front {
        n = -n;
    }
    let v = normalize(camera - input.world);
    let size = vec2<f32>(textureDimensions(scene_depth));
    let uv = input.position.xy / size;
    let water_distance = distance(camera, input.world);
    // Refraction: offset the scene lookup along the surface slope; reject samples in front
    // of the water.
    var suv = clamp(uv + n.xz * water.shallow.w * 0.05, vec2<f32>(0.0), vec2<f32>(1.0));
    var depth = depth_at(suv);
    var behind = world_at(suv, depth);
    if depth < 1.0 && distance(camera, behind) < water_distance {
        suv = uv;
        depth = depth_at(suv);
        behind = world_at(suv, depth);
    }
    let scene = textureSampleLevel(scene_color, linear_clamp, suv, 0.0).rgb;
    let sky = depth >= 1.0;
    let thickness = select(distance(behind, input.world), 1000.0, sky);
    // Lighting: ambient plus the first directional light.
    let ambient = engine_ambient();
    var sun = vec3<f32>(0.0);
    var to_sun = vec3<f32>(0.0, 1.0, 0.0);
    if engine_light_count() > 0u {
        let light = engine_light(0u);
        if light.kind == ENGINE_LIGHT_DIRECTIONAL {
            sun = light.color * light.intensity;
            to_sun = normalize(light.direction);
        }
    }
    let lit = ambient + sun * max(to_sun.y, 0.0);
    // Body colour: the scene behind attenuated by Beer-Lambert absorption, plus in-scattered
    // light that shifts from the shallow to the deep colour with thickness.
    let transmittance = exp(-water.absorption.xyz * thickness);
    let mean_t = (transmittance.x + transmittance.y + transmittance.z) / 3.0;
    let scatter = mix(water.shallow.rgb, water.deep.rgb, 1.0 - mean_t);
    var body = scene * transmittance + scatter * (1.0 - mean_t) * lit;
    // Fresnel (Schlick, F0 = 0.02) reflection of the ambient sky and the sun highlight.
    let cos_v = clamp(dot(n, v), 0.0, 1.0);
    let fresnel = 0.02 + 0.98 * pow(1.0 - cos_v, 5.0);
    let r = reflect(-v, n);
    let sky_reflection = ambient * (1.0 + max(r.y, 0.0));
    let roughness = max(water.deep.w, 0.01);
    let exponent = 2.0 / (roughness * roughness) - 2.0;
    let h = normalize(to_sun + v);
    let highlight = sun * pow(max(dot(n, h), 0.0), exponent) * (exponent + 8.0) / 25.1327
        * max(dot(n, to_sun), 0.0);
    var color = mix(body, sky_reflection, fresnel) + highlight * fresnel;
    if !front {
        // From below: the refracted world above, tinted by the water.
        color = body;
    }
    // Shoreline foam where the ground is close below the surface, plus vertex foam.
    let ground_depth = select(input.world.y - behind.y, 1000.0, sky);
    let shore = 1.0 - smoothstep(0.0, max(water.shore.x, 0.001), ground_depth);
    let foam = clamp(max(input.foam, shore) * water.foam.w, 0.0, 1.0);
    color = mix(color, water.foam.rgb * lit, foam);
    return vec4<f32>(color, 1.0);
}
