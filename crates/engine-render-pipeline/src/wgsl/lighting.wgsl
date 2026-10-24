// engine::lighting - direct lighting from the engine's light list: the
// clustered light lookup, windowed inverse-square falloff, spot cones,
// rectangular area lights, the metallic-roughness BRDF and shadow filtering
// (cascades, cube faces, PCF and PCSS). engine::material imports it; a
// material calls engine_direct_lighting (or the lower-level functions).
// Every function here is safe in non-uniform control flow (no derivatives).
#import engine::view
#import engine::brdf

// One light's incident light at a point, before the BRDF.
struct EngineIncident {
    // Unit direction toward the light (an area light's representative point).
    l: vec3<f32>,
    // Irradiance in shading units (colour x intensity x falloff x cosine),
    // without shadows.
    irradiance: vec3<f32>,
}

// Positive distance in front of the camera (the cluster slice coordinate).
fn engine_view_depth(world: vec3<f32>) -> f32 {
    return -(engine_view.view_from_world * vec4<f32>(world, 1.0)).z;
}

// Spot cone factor: 1 inside the inner cone, smoothstep to 0 at the outer.
fn engine_spot_factor(light: EngineLight, l: vec3<f32>) -> f32 {
    return smoothstep(light.params.y, light.params.x, dot(-l, light.direction));
}

// Exact irradiance factor of an area light's rectangle (luminance 1) at
// `world` with normal `n`: the Lambertian polygon integral over the part of
// the rectangle above the surface's horizon.
fn engine_area_form_factor(light: EngineLight, world: vec3<f32>, n: vec3<f32>) -> f32 {
    if light.params.z < 0.5 && dot(world - light.position, light.direction) <= 0.0 {
        return 0.0;
    }
    let right = light.tangent * light.params.x;
    let up = cross(light.direction, light.tangent) * light.params.y;
    let center = light.position - world;
    var corners = array<vec3<f32>, 4>(
        center - right - up,
        center + right - up,
        center + right + up,
        center - right + up,
    );
    var clipped: array<vec3<f32>, 5>;
    var count = 0u;
    for (var i = 0u; i < 4u; i++) {
        let a = corners[i];
        let b = corners[(i + 1u) % 4u];
        let da = dot(a, n);
        let db = dot(b, n);
        if da > 0.0 {
            clipped[count] = a;
            count++;
        }
        if (da > 0.0) != (db > 0.0) {
            clipped[count] = a + (b - a) * (da / (da - db));
            count++;
        }
    }
    if count < 3u {
        return 0.0;
    }
    var sum = 0.0;
    for (var i = 0u; i < count; i++) {
        let a = normalize(clipped[i]);
        let b = normalize(clipped[(i + 1u) % count]);
        let c = cross(a, b);
        let s = length(c);
        let theta = acos(clamp(dot(a, b), -1.0, 1.0));
        sum += dot(c, n) * select(theta / s, 1.0, s < 0.000001);
    }
    return 0.5 * abs(sum);
}

// The point of an area light's rectangle closest to the ray `world + t r`
// (the representative point of its specular highlight).
fn engine_area_point(light: EngineLight, world: vec3<f32>, r: vec3<f32>) -> vec3<f32> {
    let denominator = dot(r, light.direction);
    var p = light.position;
    if abs(denominator) > 0.0001 {
        let t = dot(light.position - world, light.direction) / denominator;
        if t > 0.0 {
            p = world + r * t;
        }
    }
    let bitangent = cross(light.direction, light.tangent);
    let d = p - light.position;
    let u = clamp(dot(d, light.tangent), -light.params.x, light.params.x);
    let v = clamp(dot(d, bitangent), -light.params.y, light.params.y);
    return light.position + light.tangent * u + bitangent * v;
}

// Incident light of `light` at `world` (normal `n`, view direction `v`).
fn engine_light_incident(light: EngineLight, world: vec3<f32>, n: vec3<f32>, v: vec3<f32>) -> EngineIncident {
    var incident: EngineIncident;
    let radiance = light.color * light.intensity;
    if light.kind == ENGINE_LIGHT_DIRECTIONAL {
        incident.l = light.direction;
        incident.irradiance = radiance * max(dot(n, light.direction), 0.0);
        return incident;
    }
    let to_light = light.position - world;
    let distance = length(to_light);
    if light.kind == ENGINE_LIGHT_AREA {
        let r = reflect(-v, n);
        let toward = engine_area_point(light, world, r) - world;
        incident.l = toward / max(length(toward), 0.000001);
        incident.irradiance = radiance * engine_area_form_factor(light, world, n) *
            engine_range_window(distance, light.range);
        return incident;
    }
    incident.l = to_light / max(distance, 0.000001);
    var scale = max(dot(n, incident.l), 0.0) * engine_distance_falloff(distance, light.range);
    if light.kind == ENGINE_LIGHT_SPOT {
        scale *= engine_spot_factor(light, incident.l);
    }
    incident.irradiance = radiance * scale;
    return incident;
}

// ------------------------------------------------------------- shadows

fn engine_shadow_linear(shadow: EngineShadow, raw: f32) -> f32 {
    let near = shadow.range.x;
    let far = shadow.range.y;
    if shadow.bias.w > 0.5 {
        return near * far / (far - raw * (far - near));
    }
    return near + raw * (far - near);
}

fn engine_shadow_raw(shadow: EngineShadow, linear: f32) -> f32 {
    let near = shadow.range.x;
    let far = shadow.range.y;
    if shadow.bias.w > 0.5 {
        return far * (linear - near) / (linear * (far - near));
    }
    return (linear - near) / (far - near);
}

// Sample i of an n-point Vogel (golden angle) disk of radius 1.
fn engine_vogel(i: u32, n: u32) -> vec2<f32> {
    let radius = sqrt((f32(i) + 0.5) / f32(n));
    let angle = f32(i) * 2.39996323;
    return radius * vec2<f32>(cos(angle), sin(angle));
}

struct EngineShadowTexel {
    uv: vec2<f32>,
    lo: vec2<f32>,
    hi: vec2<f32>,
    texel: f32,
    layer: i32,
}

fn engine_shadow_compare(t: EngineShadowTexel, offset: vec2<f32>, reference: f32) -> f32 {
    let uv = clamp(t.uv + offset * t.texel, t.lo, t.hi);
    return textureSampleCompareLevel(engine_shadow_atlas, engine_shadow_sampler, uv, t.layer, reference);
}

// Visibility (0 shadowed .. 1 lit) of `world` (normal `n`) in one shadow
// view. Points outside the view are lit.
fn engine_shadow_view_visibility(index: u32, world: vec3<f32>, n: vec3<f32>) -> f32 {
    let shadow = engine_shadows[index];
    let perspective = shadow.bias.w > 0.5;
    let p0 = shadow.clip_from_world * vec4<f32>(world, 1.0);
    let axis = select(engine_shadow_linear(shadow, p0.z / p0.w), p0.w, perspective);
    let world_texel = select(shadow.bias.z, shadow.bias.z * max(axis, shadow.range.x), perspective);
    let p = shadow.clip_from_world * vec4<f32>(world + n * (shadow.bias.y * world_texel), 1.0);
    if p.w <= 0.0 {
        return 1.0;
    }
    let ndc = p.xyz / p.w;
    let local_uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    if any(local_uv < vec2<f32>(0.0)) || any(local_uv > vec2<f32>(1.0)) || ndc.z > 1.0 {
        return 1.0;
    }
    let receiver = engine_shadow_linear(shadow, max(ndc.z, 0.0)) - shadow.bias.x * world_texel;
    let reference = engine_shadow_raw(shadow, max(receiver, shadow.range.x + 0.000001));
    let page = f32(textureDimensions(engine_shadow_atlas).x);
    var t: EngineShadowTexel;
    t.texel = 1.0 / page;
    t.uv = shadow.rect.xy + local_uv * shadow.rect.z;
    t.lo = shadow.rect.xy + vec2<f32>(0.5 * t.texel);
    t.hi = shadow.rect.xy + vec2<f32>(shadow.rect.z - 0.5 * t.texel);
    t.layer = i32(shadow.rect.w);
    let mode = u32(shadow.sampling.x);
    if mode == 0u {
        return engine_shadow_compare(t, vec2<f32>(0.0), reference);
    }
    if mode == 1u {
        let radius = i32(shadow.sampling.y);
        var sum = 0.0;
        for (var y = -radius; y <= radius; y++) {
            for (var x = -radius; x <= radius; x++) {
                sum += engine_shadow_compare(t, vec2<f32>(f32(x), f32(y)), reference);
            }
        }
        let taps = f32((2 * radius + 1) * (2 * radius + 1));
        return sum / taps;
    }
    // PCSS: blocker search, penumbra estimate, variable-radius filter.
    let size = shadow.sampling.z;
    let max_radius = shadow.sampling.w;
    var search: f32;
    if perspective {
        search = size * (receiver - shadow.range.x) / (receiver * shadow.range.x) / shadow.bias.z;
    } else {
        search = tan(0.5 * size) * (receiver - shadow.range.x) / shadow.bias.z;
    }
    search = clamp(search, 1.0, max_radius);
    var blockers = 0.0;
    var blocker_sum = 0.0;
    let dims = vec2<f32>(textureDimensions(engine_shadow_atlas));
    for (var i = 0u; i < 16u; i++) {
        let uv = clamp(t.uv + engine_vogel(i, 16u) * search * t.texel, t.lo, t.hi);
        let stored = engine_shadow_linear(shadow, textureLoad(engine_shadow_atlas, vec2<i32>(uv * dims), t.layer, 0));
        if stored < receiver {
            blockers += 1.0;
            blocker_sum += stored;
        }
    }
    if blockers < 0.5 {
        return 1.0;
    }
    let blocker = max(blocker_sum / blockers, shadow.range.x + 0.000001);
    var penumbra: f32;
    if perspective {
        penumbra = size * (receiver - blocker) / blocker / (shadow.bias.z * receiver);
    } else {
        penumbra = tan(0.5 * size) * (receiver - blocker) / shadow.bias.z;
    }
    let radius = clamp(penumbra, 1.0, max_radius);
    var sum = 0.0;
    for (var i = 0u; i < 32u; i++) {
        sum += engine_shadow_compare(t, engine_vogel(i, 32u) * radius, reference);
    }
    return sum / 32.0;
}

// Cascade (0-based) of a shadowed directional `light` covering `world`, or
// -1 (not directional, unshadowed, or beyond the last cascade).
fn engine_cascade_index(light: EngineLight, world: vec3<f32>) -> i32 {
    if light.kind != ENGINE_LIGHT_DIRECTIONAL || light.shadow_index < 0 {
        return -1;
    }
    let depth = engine_view_depth(world);
    let count = u32(light.params.x);
    for (var i = 0u; i < count; i++) {
        if depth <= engine_shadows[u32(light.shadow_index) + i].range.w {
            return i32(i);
        }
    }
    return -1;
}

// Cube face (EngineShadow offset) of `direction` from a point/area light:
// +X, -X, +Y, -Y, +Z, -Z by major axis.
fn engine_cube_face(direction: vec3<f32>) -> u32 {
    let a = abs(direction);
    if a.x >= a.y && a.x >= a.z {
        return select(1u, 0u, direction.x > 0.0);
    }
    if a.y >= a.z {
        return select(3u, 2u, direction.y > 0.0);
    }
    return select(5u, 4u, direction.z > 0.0);
}

// Fraction of `light` reaching `world` (normal `n`): 1 without a shadow.
fn engine_light_visibility(light: EngineLight, world: vec3<f32>, n: vec3<f32>) -> f32 {
    if light.shadow_index < 0 {
        return 1.0;
    }
    let base = u32(light.shadow_index);
    if light.kind == ENGINE_LIGHT_SPOT {
        return engine_shadow_view_visibility(base, world, n);
    }
    if light.kind != ENGINE_LIGHT_DIRECTIONAL {
        return engine_shadow_view_visibility(base + engine_cube_face(world - light.position), world, n);
    }
    let cascade = engine_cascade_index(light, world);
    if cascade < 0 {
        return 1.0;
    }
    let index = base + u32(cascade);
    var visibility = engine_shadow_view_visibility(index, world, n);
    let blend = light.params.y;
    let split = engine_shadows[index].range;
    let start = split.w - blend * (split.w - split.z);
    let depth = engine_view_depth(world);
    if blend > 0.0 && depth > start {
        var next = 1.0;
        if u32(cascade) + 1u < u32(light.params.x) {
            next = engine_shadow_view_visibility(index + 1u, world, n);
        }
        visibility = mix(visibility, next, clamp((depth - start) / max(split.w - start, 0.000001), 0.0, 1.0));
    }
    return visibility;
}

// Outgoing radiance from one light (BRDF x irradiance x shadow).
fn engine_light_contribution(light: EngineLight, surface: EngineSurface, world: vec3<f32>, v: vec3<f32>, receive_shadows: bool) -> vec3<f32> {
    let incident = engine_light_incident(light, world, surface.normal, v);
    if all(incident.irradiance <= vec3<f32>(0.0)) {
        return vec3<f32>(0.0);
    }
    var visibility = 1.0;
    if receive_shadows {
        visibility = engine_light_visibility(light, world, surface.normal);
    }
    let brdf = engine_brdf(surface, v, incident.l);
    var specular = brdf.specular;
    if light.kind == ENGINE_LIGHT_AREA {
        // Representative-point energy normalisation (widened lobe).
        let alpha = max(surface.roughness, 0.045) * max(surface.roughness, 0.045);
        let radius = sqrt(4.0 * light.params.x * light.params.y / ENGINE_PI);
        let wide = clamp(alpha + radius / (2.0 * max(length(light.position - world), 0.0001)), 0.0, 1.0);
        specular *= (alpha / wide) * (alpha / wide);
    }
    return (brdf.diffuse + specular) * incident.irradiance * visibility;
}

// --------------------------------------------------------- clustering

// Cluster of a fragment at render pixel `frag` (top-left origin) and view
// depth `depth`.
fn engine_cluster_index(frag: vec2<f32>, depth: f32) -> u32 {
    let dims = engine_clusters.dims;
    let tile = engine_clusters.tile;
    let slicing = engine_clusters.depth;
    let x = min(u32(max(frag.x, 0.0) * tile.x), dims.x - 1u);
    let y = min(u32(max(frag.y, 0.0) * tile.y), dims.y - 1u);
    let slice = log(max(depth, slicing.x)) * slicing.z + slicing.w;
    let z = u32(clamp(slice, 0.0, f32(dims.z - 1u)));
    return x + dims.x * (y + dims.y * z);
}

// Direct lighting of every light affecting `world`: directional lights,
// then the local lights of the fragment's cluster (or every local light
// when clustering is off). `frag` is the fragment's @builtin(position).xy.
fn engine_direct_lighting(surface: EngineSurface, world: vec3<f32>, v: vec3<f32>, frag: vec2<f32>, receive_shadows: bool) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    let directional = engine_lights.counts.y;
    for (var i = 0u; i < directional; i++) {
        sum += engine_light_contribution(engine_lights.lights[i], surface, world, v, receive_shadows);
    }
    if engine_clusters.params.x == 1u {
        for (var i = directional; i < engine_lights.counts.x; i++) {
            sum += engine_light_contribution(engine_lights.lights[i], surface, world, v, receive_shadows);
        }
        return sum;
    }
    let cluster = engine_cluster_index(frag, engine_view_depth(world));
    let count = engine_clusters.counts[cluster];
    let first = cluster * engine_clusters.dims.w;
    for (var i = 0u; i < count; i++) {
        let light = engine_lights.lights[engine_cluster_lights[first + i]];
        sum += engine_light_contribution(light, surface, world, v, receive_shadows);
    }
    return sum;
}
