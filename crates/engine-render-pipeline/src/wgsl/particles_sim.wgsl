// engine.particles simulation: integrate + kill, then emit. Mirrors the CPU
// reference in particles/mod.rs operation for operation (docs/rendering-particles.md).

struct Emitter {
    // xyz spawn centre, w spawn box half extent.
    origin: vec4<f32>,
    // xyz unit emission axis, w tan(spread).
    axis: vec4<f32>,
    // xyz unit tangent, w minimum speed.
    tangent: vec4<f32>,
    // xyz unit bitangent, w maximum speed.
    bitangent: vec4<f32>,
    // xyz gravity (world units per second squared), w linear drag per second.
    gravity: vec4<f32>,
    // x minimum lifetime, y maximum lifetime, z dt, w unused.
    life_dt: vec4<f32>,
    // x capacity, y ring head, z spawn count, w seed.
    counts: vec4<u32>,
    // x spawn index of the first spawn of this tick.
    base: vec4<u32>,
    start_color: vec4<f32>,
    end_color: vec4<f32>,
    // x start size, y end size.
    size: vec4<f32>,
}

// A slot is alive exactly when lifetime > 0.
struct Particle {
    position: vec3<f32>,
    age: f32,
    velocity: vec3<f32>,
    lifetime: f32,
}

struct Counters {
    alive: atomic<u32>,
    spawned: atomic<u32>,
    dropped: atomic<u32>,
    killed: atomic<u32>,
}

@group(0) @binding(0) var<uniform> emitter: Emitter;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(2) var<storage, read_write> counters: Counters;

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336653u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// Uniform in [0, 1): 24 random bits, exactly representable.
fn unit_random(seed: u32, n: u32, stream: u32) -> f32 {
    let h = pcg(seed ^ pcg(n ^ pcg(stream)));
    return f32(h >> 8u) * (1.0 / 16777216.0);
}

@compute @workgroup_size(64)
fn integrate(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= emitter.counts.x {
        return;
    }
    var p = particles[i];
    if p.lifetime <= 0.0 {
        return;
    }
    let dt = emitter.life_dt.z;
    let age = p.age + dt;
    if age >= p.lifetime {
        particles[i] = Particle(vec3<f32>(0.0), 0.0, vec3<f32>(0.0), 0.0);
        atomicAdd(&counters.killed, 1u);
        return;
    }
    let damp = max(1.0 - emitter.gravity.w * dt, 0.0);
    let velocity = (p.velocity + emitter.gravity.xyz * dt) * damp;
    p.position = p.position + velocity * dt;
    p.velocity = velocity;
    p.age = age;
    particles[i] = p;
    atomicAdd(&counters.alive, 1u);
}

@compute @workgroup_size(64)
fn emit(@builtin(global_invocation_id) id: vec3<u32>) {
    let k = id.x;
    if k >= emitter.counts.z {
        return;
    }
    let slot = (emitter.counts.y + k) % emitter.counts.x;
    if particles[slot].lifetime > 0.0 {
        atomicAdd(&counters.dropped, 1u);
        return;
    }
    let n = emitter.base.x + k;
    let seed = emitter.counts.w;
    let lifetime = emitter.life_dt.x + (emitter.life_dt.y - emitter.life_dt.x) * unit_random(seed, n, 0u);
    let speed = emitter.tangent.w + (emitter.bitangent.w - emitter.tangent.w) * unit_random(seed, n, 1u);
    let a = unit_random(seed, n, 2u) * 2.0 - 1.0;
    let b = unit_random(seed, n, 3u) * 2.0 - 1.0;
    let direction = normalize(emitter.axis.xyz + emitter.axis.w * (emitter.tangent.xyz * a + emitter.bitangent.xyz * b));
    let offset = vec3<f32>(
        unit_random(seed, n, 4u) * 2.0 - 1.0,
        unit_random(seed, n, 5u) * 2.0 - 1.0,
        unit_random(seed, n, 6u) * 2.0 - 1.0,
    ) * emitter.origin.w;
    particles[slot] = Particle(emitter.origin.xyz + offset, 0.0, direction * speed, lifetime);
    atomicAdd(&counters.alive, 1u);
    atomicAdd(&counters.spawned, 1u);
}
