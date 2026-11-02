//! GPU particles: fixed-capacity emitters simulated by compute passes once
//! per scene tick and drawn as camera-facing billboards.
//!
//! This module is the CPU side of `engine.particles` (see
//! `docs/rendering-particles.md`): the [`ParticleEmitter`] description and its
//! validation, the explicit [`ParticleTick`], the deterministic spawn plan, the
//! GPU uniform layout, the shared random generator, and a CPU
//! [`ParticleReference`] that performs the same step as the compute shader.
//! The GPU resources, the tick entry point and the render pass live in the
//! private `gpu` module.

mod gpu;

use anyhow::{Result, ensure};
use bytemuck::{Pod, Zeroable};
use glam::{Vec3, Vec4};

pub(crate) use gpu::{ParticleShared, descriptor, install};

/// Most live emitters. More is rejected as invalid input.
pub const MAX_EMITTERS: usize = 16;
/// Largest capacity of one emitter, in particles.
pub const MAX_PARTICLES_PER_EMITTER: u32 = 65_536;
/// Largest sum of the capacities of all live emitters.
pub const MAX_TOTAL_PARTICLES: u32 = 262_144;
/// Longest accepted emitter id, in bytes.
pub const MAX_EMITTER_ID_BYTES: usize = 64;
/// Longest simulation step, in seconds. Larger steps are rejected, not split.
pub const MAX_TICK_DT: f32 = 0.25;
/// Largest accepted position or gravity component magnitude.
pub const MAX_PARTICLE_EXTENT: f32 = 100_000.0;
/// Largest spawn rate, in particles per second.
pub const MAX_SPAWN_RATE: f32 = 1_000_000.0;
/// Longest particle lifetime, in seconds.
pub const MAX_LIFETIME: f32 = 3600.0;
/// Largest particle speed, in world units per second.
pub const MAX_SPEED: f32 = 10_000.0;
/// Largest `spread`, in radians (the tangent of the half angle must stay finite).
pub const MAX_SPREAD: f32 = 1.5;
/// Largest billboard half size, in world units.
pub const MAX_PARTICLE_SIZE: f32 = 1000.0;
/// Largest colour channel (HDR scene units).
pub const MAX_PARTICLE_RADIANCE: f32 = 1000.0;
/// Largest drag coefficient, per second.
pub const MAX_DRAG: f32 = 1000.0;
/// Documented GPU versus CPU-reference tolerance of positions and velocities
/// after a handful of ticks: `|gpu - cpu| <= TOLERANCE * max(1, |cpu|)`.
/// Spawn decisions, counts and slots are exact; only floating-point
/// contraction (fused multiply-add) and `normalize` rounding differ.
pub const REFERENCE_TOLERANCE: f32 = 1e-3;
/// Threads per workgroup of both compute entry points.
pub const WORKGROUP_SIZE: u32 = 64;

/// How an emitter's billboards combine with the scene colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParticleBlend {
    /// Premultiplied alpha "over". Overlapping particles blend in slot order
    /// (spawn order, wrapping at the ring head); there is no depth sort.
    #[default]
    Alpha,
    /// Add `rgb * alpha`. Order independent, so no sort is needed.
    Additive,
}

/// One emitter: a fixed-capacity pool, a spawn rate, and the launch, motion
/// and look of its particles. Positions are world space (after the applied
/// floating origin); particles do not follow the emitter once spawned.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleEmitter {
    /// Stable identity across ticks: 1..=[`MAX_EMITTER_ID_BYTES`] bytes. An id
    /// that disappears frees its buffers; a changed `capacity` restarts it.
    pub id: String,
    /// Spawn centre.
    pub position: Vec3,
    /// Emission axis; finite and nonzero (normalised on use).
    pub direction: Vec3,
    /// Pool size, `1..=MAX_PARTICLES_PER_EMITTER`. Fixed for the life of the
    /// emitter; spawns beyond it are dropped and counted.
    pub capacity: u32,
    /// Particles per second, `0..=MAX_SPAWN_RATE`. Fractions carry over.
    pub spawn_rate: f32,
    /// Lifetime range in seconds, `0 < min <= max <= MAX_LIFETIME`.
    pub lifetime: [f32; 2],
    /// Launch speed range, `0 <= min <= max <= MAX_SPEED`.
    pub speed: [f32; 2],
    /// Half angle of the square-section launch cone in radians,
    /// `0..=MAX_SPREAD`: the launch direction is
    /// `normalize(axis + tan(spread) * (u * tangent + v * bitangent))` with `u`
    /// and `v` uniform in `[-1, 1]`.
    pub spread: f32,
    /// Half size of the axis-aligned spawn box around `position`.
    pub spawn_extent: f32,
    /// Constant acceleration.
    pub gravity: Vec3,
    /// Linear drag per second: velocity is scaled by `max(1 - drag * dt, 0)`.
    pub drag: f32,
    /// Billboard half size at birth and at death (linear in age).
    pub size: [f32; 2],
    /// Linear RGBA at birth and at death (linear in age); alpha `0..=1`.
    pub start_color: Vec4,
    pub end_color: Vec4,
    pub blend: ParticleBlend,
    /// Seed of the deterministic random streams.
    pub seed: u32,
}

impl Default for ParticleEmitter {
    fn default() -> Self {
        Self {
            id: "emitter".to_owned(),
            position: Vec3::ZERO,
            direction: Vec3::Y,
            capacity: 1024,
            spawn_rate: 100.0,
            lifetime: [1.0, 2.0],
            speed: [1.0, 3.0],
            spread: 0.35,
            spawn_extent: 0.0,
            gravity: Vec3::new(0.0, -9.81, 0.0),
            drag: 0.0,
            size: [0.1, 0.05],
            start_color: Vec4::ONE,
            end_color: Vec4::new(1.0, 1.0, 1.0, 0.0),
            blend: ParticleBlend::Alpha,
            seed: 1,
        }
    }
}

fn in_range(value: f32, low: f32, high: f32) -> bool {
    value.is_finite() && (low..=high).contains(&value)
}

fn color_ok(color: Vec4) -> bool {
    in_range(color.x, 0.0, MAX_PARTICLE_RADIANCE)
        && in_range(color.y, 0.0, MAX_PARTICLE_RADIANCE)
        && in_range(color.z, 0.0, MAX_PARTICLE_RADIANCE)
        && in_range(color.w, 0.0, 1.0)
}

impl ParticleEmitter {
    /// Reject non-finite values, zero or excess capacity, negative rates,
    /// lifetimes, speeds or sizes, and anything outside the documented
    /// ranges. Pure; does not touch the GPU.
    pub fn validate(&self) -> Result<()> {
        let id = &self.id;
        ensure!(
            (1..=MAX_EMITTER_ID_BYTES).contains(&id.len()),
            "particle emitter ids are 1..={MAX_EMITTER_ID_BYTES} bytes"
        );
        ensure!(
            self.position.is_finite() && self.position.abs().max_element() <= MAX_PARTICLE_EXTENT,
            "emitter {id}: position must be finite and within {MAX_PARTICLE_EXTENT} of the origin"
        );
        let length_squared = self.direction.length_squared();
        ensure!(
            self.direction.is_finite() && length_squared.is_finite() && length_squared > 1e-12,
            "emitter {id}: direction must be finite and nonzero"
        );
        ensure!(
            (1..=MAX_PARTICLES_PER_EMITTER).contains(&self.capacity),
            "emitter {id}: capacity must be 1..={MAX_PARTICLES_PER_EMITTER}, got {}",
            self.capacity
        );
        ensure!(
            in_range(self.spawn_rate, 0.0, MAX_SPAWN_RATE),
            "emitter {id}: spawn_rate must be finite and within 0..={MAX_SPAWN_RATE}"
        );
        ensure!(
            in_range(self.lifetime[0], f32::MIN_POSITIVE, MAX_LIFETIME)
                && in_range(self.lifetime[1], self.lifetime[0], MAX_LIFETIME),
            "emitter {id}: lifetime must satisfy 0 < min <= max <= {MAX_LIFETIME}"
        );
        ensure!(
            in_range(self.speed[0], 0.0, MAX_SPEED)
                && in_range(self.speed[1], self.speed[0], MAX_SPEED),
            "emitter {id}: speed must satisfy 0 <= min <= max <= {MAX_SPEED}"
        );
        ensure!(
            in_range(self.spread, 0.0, MAX_SPREAD),
            "emitter {id}: spread must be within 0..={MAX_SPREAD} radians"
        );
        ensure!(
            in_range(self.spawn_extent, 0.0, MAX_PARTICLE_EXTENT),
            "emitter {id}: spawn_extent must be within 0..={MAX_PARTICLE_EXTENT}"
        );
        ensure!(
            self.gravity.is_finite() && self.gravity.abs().max_element() <= MAX_PARTICLE_EXTENT,
            "emitter {id}: gravity must be finite and within {MAX_PARTICLE_EXTENT} per axis"
        );
        ensure!(
            in_range(self.drag, 0.0, MAX_DRAG),
            "emitter {id}: drag must be within 0..={MAX_DRAG}"
        );
        ensure!(
            self.size
                .iter()
                .all(|s| in_range(*s, 0.0, MAX_PARTICLE_SIZE)),
            "emitter {id}: size must be within 0..={MAX_PARTICLE_SIZE}"
        );
        ensure!(
            color_ok(self.start_color) && color_ok(self.end_color),
            "emitter {id}: colour channels must be within 0..={MAX_PARTICLE_RADIANCE} and alpha within 0..=1"
        );
        Ok(())
    }
}

/// One step of the particle clock: the host's scene tick.
///
/// The simulation advances exactly when it receives a tick whose `id`
/// differs from the last one it simulated; the same id again is a no-op, so
/// rendering a tick for any number of views or presentations cannot advance
/// it. Ids are opaque: hosts normally pass a monotonically increasing scene
/// tick number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParticleTick {
    pub id: u64,
    /// Seconds simulated by this tick, finite and in `(0, MAX_TICK_DT]`.
    pub dt: f32,
}

impl ParticleTick {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.dt.is_finite() && self.dt > 0.0 && self.dt <= MAX_TICK_DT,
            "particle tick dt must be finite and within (0, {MAX_TICK_DT}] seconds, got {}",
            self.dt
        );
        Ok(())
    }
}

/// Validate a tick and the complete emitter set it will simulate: finite
/// values, per-emitter ranges, unique ids, and the emitter and total
/// capacity budgets. Pure.
pub fn validate_tick(tick: &ParticleTick, emitters: &[ParticleEmitter]) -> Result<()> {
    tick.validate()?;
    ensure!(
        emitters.len() <= MAX_EMITTERS,
        "at most {MAX_EMITTERS} particle emitters, got {}",
        emitters.len()
    );
    let mut total = 0u64;
    for (index, emitter) in emitters.iter().enumerate() {
        emitter.validate()?;
        ensure!(
            emitters[..index].iter().all(|other| other.id != emitter.id),
            "duplicate particle emitter id {}",
            emitter.id
        );
        total += u64::from(emitter.capacity);
    }
    ensure!(
        total <= u64::from(MAX_TOTAL_PARTICLES),
        "particle capacities total {total}, above the {MAX_TOTAL_PARTICLES} limit"
    );
    Ok(())
}

/// One particle slot, as the compute shader stores it. A slot is alive
/// exactly when `lifetime > 0`; a dead slot is all zeros.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuParticle {
    pub position: [f32; 3],
    pub age: f32,
    pub velocity: [f32; 3],
    pub lifetime: f32,
}

impl GpuParticle {
    pub fn is_alive(&self) -> bool {
        self.lifetime > 0.0
    }

    /// Whether `self` matches `reference` within [`REFERENCE_TOLERANCE`].
    pub fn matches(&self, reference: &Self) -> bool {
        let close = |a: f32, b: f32| (a - b).abs() <= REFERENCE_TOLERANCE * b.abs().max(1.0);
        (0..3).all(|i| {
            close(self.position[i], reference.position[i])
                && close(self.velocity[i], reference.velocity[i])
        }) && close(self.age, reference.age)
            && close(self.lifetime, reference.lifetime)
    }
}

/// Bytes of one [`GpuParticle`].
pub const PARTICLE_BYTES: u64 = std::mem::size_of::<GpuParticle>() as u64;

/// GPU counters of one emitter (`alive` is recomputed every tick; the others
/// accumulate and wrap at 2^32).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct ParticleCounters {
    /// Live particles after the last tick.
    pub alive: u32,
    /// Particles spawned into a free slot.
    pub spawned: u32,
    /// Spawns dropped because their ring slot was still alive.
    pub dropped: u32,
    /// Particles killed at the end of their lifetime.
    pub killed: u32,
}

/// Bytes of [`ParticleCounters`].
pub const COUNTERS_BYTES: u64 = std::mem::size_of::<ParticleCounters>() as u64;

/// The per-tick emitter parameters, as the compute and draw shaders read
/// them (`Emitter` in `particles_sim.wgsl` and `particles_draw.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct EmitterUniform {
    /// xyz spawn centre, w spawn box half extent.
    pub origin: [f32; 4],
    /// xyz unit axis, w `tan(spread)`.
    pub axis: [f32; 4],
    /// xyz unit tangent, w minimum speed.
    pub tangent: [f32; 4],
    /// xyz unit bitangent, w maximum speed.
    pub bitangent: [f32; 4],
    /// xyz gravity, w drag.
    pub gravity: [f32; 4],
    /// x minimum lifetime, y maximum lifetime, z dt.
    pub life_dt: [f32; 4],
    /// x capacity, y ring head, z spawn count, w seed.
    pub counts: [u32; 4],
    /// x spawn index of this tick's first spawn.
    pub base: [u32; 4],
    pub start_color: [f32; 4],
    pub end_color: [f32; 4],
    /// x start size, y end size.
    pub size: [f32; 4],
}

/// Bytes of [`EmitterUniform`].
pub const EMITTER_UNIFORM_BYTES: u64 = std::mem::size_of::<EmitterUniform>() as u64;

/// What one tick spawns: the deterministic result of [`EmitterCursor::plan`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpawnPlan {
    /// Ring position of the first spawn.
    pub head: u32,
    /// Spawn index of the first spawn (feeds the random streams).
    pub base: u32,
    /// Spawns attempted this tick, at most the capacity.
    pub count: u32,
    /// Spawns beyond the capacity of one tick, dropped without an attempt.
    pub excess: u32,
}

/// The CPU-side clock of one emitter: the fractional spawn carry, the ring
/// head and the number of spawn attempts so far. The GPU path and the
/// reference both advance one of these, so their spawn plans are identical.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EmitterCursor {
    accumulator: f32,
    head: u32,
    attempts: u64,
    rate_limited: u64,
}

impl EmitterCursor {
    /// Plan the spawns of a `dt`-second tick and advance the cursor. Spawn
    /// `k` of the tick targets ring slot `(head + k) % capacity`.
    pub fn plan(&mut self, emitter: &ParticleEmitter, dt: f32) -> SpawnPlan {
        self.accumulator += emitter.spawn_rate * dt;
        let whole = self.accumulator.floor();
        self.accumulator -= whole;
        let requested = whole.min(u32::MAX as f32) as u32;
        let count = requested.min(emitter.capacity);
        let excess = requested - count;
        let plan = SpawnPlan {
            head: self.head,
            base: self.attempts as u32,
            count,
            excess,
        };
        self.head =
            ((u64::from(self.head) + u64::from(count)) % u64::from(emitter.capacity)) as u32;
        self.attempts += u64::from(count);
        self.rate_limited += u64::from(excess);
        plan
    }

    /// Spawn attempts planned so far (each one tried a ring slot).
    pub fn attempts(&self) -> u64 {
        self.attempts
    }

    /// Spawns dropped because a single tick asked for more than the capacity.
    pub fn rate_limited(&self) -> u64 {
        self.rate_limited
    }

    /// Slots that can ever have held a particle: instances the draw needs.
    pub fn high_water(&self, capacity: u32) -> u32 {
        self.attempts.min(u64::from(capacity)) as u32
    }
}

/// The launch basis of `axis` (unit): `(tangent, bitangent)`.
pub fn launch_basis(axis: Vec3) -> (Vec3, Vec3) {
    let helper = if axis.y.abs() < 0.99 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let tangent = axis.cross(helper).normalize();
    (tangent, axis.cross(tangent))
}

/// The uniform of `emitter` for a tick of `dt` seconds with `plan`.
pub fn emitter_uniform(emitter: &ParticleEmitter, plan: &SpawnPlan, dt: f32) -> EmitterUniform {
    let axis = emitter.direction.normalize();
    let (tangent, bitangent) = launch_basis(axis);
    EmitterUniform {
        origin: emitter.position.extend(emitter.spawn_extent).to_array(),
        axis: axis.extend(emitter.spread.tan()).to_array(),
        tangent: tangent.extend(emitter.speed[0]).to_array(),
        bitangent: bitangent.extend(emitter.speed[1]).to_array(),
        gravity: emitter.gravity.extend(emitter.drag).to_array(),
        life_dt: [emitter.lifetime[0], emitter.lifetime[1], dt, 0.0],
        counts: [emitter.capacity, plan.head, plan.count, emitter.seed],
        base: [plan.base, 0, 0, 0],
        start_color: emitter.start_color.to_array(),
        end_color: emitter.end_color.to_array(),
        size: [emitter.size[0], emitter.size[1], 0.0, 0.0],
    }
}

/// PCG output hash; the same function as `pcg` in `particles_sim.wgsl`.
pub fn pcg_hash(v: u32) -> u32 {
    let state = v.wrapping_mul(747_796_405).wrapping_add(2_891_336_653);
    let word = ((state >> ((state >> 28) + 4)) ^ state).wrapping_mul(277_803_737);
    (word >> 22) ^ word
}

/// A uniform value in `[0, 1)` of random stream `stream` of spawn `n`: 24
/// random bits scaled by 2^-24, exactly representable, so the CPU and GPU
/// values are bit-identical.
pub fn unit_random(seed: u32, n: u32, stream: u32) -> f32 {
    let h = pcg_hash(seed ^ pcg_hash(n ^ pcg_hash(stream)));
    (h >> 8) as f32 * (1.0 / 16_777_216.0)
}

/// The particle the emit entry point writes for spawn `k` of the tick
/// described by `uniform`.
pub fn spawn_particle(uniform: &EmitterUniform, k: u32) -> GpuParticle {
    let n = uniform.base[0].wrapping_add(k);
    let seed = uniform.counts[3];
    let random = |stream| unit_random(seed, n, stream);
    let vec3 = |v: [f32; 4]| Vec3::new(v[0], v[1], v[2]);
    let lifetime = uniform.life_dt[0] + (uniform.life_dt[1] - uniform.life_dt[0]) * random(0);
    let speed = uniform.tangent[3] + (uniform.bitangent[3] - uniform.tangent[3]) * random(1);
    let a = random(2) * 2.0 - 1.0;
    let b = random(3) * 2.0 - 1.0;
    let direction = (vec3(uniform.axis)
        + uniform.axis[3] * (vec3(uniform.tangent) * a + vec3(uniform.bitangent) * b))
        .normalize();
    let offset = Vec3::new(
        random(4) * 2.0 - 1.0,
        random(5) * 2.0 - 1.0,
        random(6) * 2.0 - 1.0,
    ) * uniform.origin[3];
    GpuParticle {
        position: (vec3(uniform.origin) + offset).to_array(),
        age: 0.0,
        velocity: (direction * speed).to_array(),
        lifetime,
    }
}

/// What the integrate entry point does to one slot.
pub fn integrate_particle(particle: &GpuParticle, uniform: &EmitterUniform) -> IntegrateResult {
    if particle.lifetime <= 0.0 {
        return IntegrateResult::Empty;
    }
    let dt = uniform.life_dt[2];
    let age = particle.age + dt;
    if age >= particle.lifetime {
        return IntegrateResult::Killed;
    }
    let damp = (1.0 - uniform.gravity[3] * dt).max(0.0);
    let gravity = Vec3::new(uniform.gravity[0], uniform.gravity[1], uniform.gravity[2]);
    let velocity = (Vec3::from(particle.velocity) + gravity * dt) * damp;
    let position = Vec3::from(particle.position) + velocity * dt;
    IntegrateResult::Alive(GpuParticle {
        position: position.to_array(),
        age,
        velocity: velocity.to_array(),
        lifetime: particle.lifetime,
    })
}

/// Result of [`integrate_particle`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IntegrateResult {
    /// The slot was already dead.
    Empty,
    /// The particle reached the end of its life this step.
    Killed,
    /// The particle after the step.
    Alive(GpuParticle),
}

/// CPU implementation of the exact step the compute passes perform: the
/// reference the GPU state is read back against.
#[derive(Clone, Debug)]
pub struct ParticleReference {
    pub particles: Vec<GpuParticle>,
    pub cursor: EmitterCursor,
    pub counters: ParticleCounters,
}

impl ParticleReference {
    pub fn new(capacity: u32) -> Self {
        Self {
            particles: vec![GpuParticle::default(); capacity as usize],
            cursor: EmitterCursor::default(),
            counters: ParticleCounters::default(),
        }
    }

    /// Simulate one tick of `emitter` (whose capacity must equal this
    /// reference's): integrate and kill, then emit.
    pub fn step(&mut self, emitter: &ParticleEmitter, dt: f32) {
        debug_assert_eq!(self.particles.len(), emitter.capacity as usize);
        let plan = self.cursor.plan(emitter, dt);
        let uniform = emitter_uniform(emitter, &plan, dt);
        let mut alive = 0u32;
        for slot in &mut self.particles {
            match integrate_particle(slot, &uniform) {
                IntegrateResult::Empty => {}
                IntegrateResult::Killed => {
                    *slot = GpuParticle::default();
                    self.counters.killed = self.counters.killed.wrapping_add(1);
                }
                IntegrateResult::Alive(next) => {
                    *slot = next;
                    alive += 1;
                }
            }
        }
        for k in 0..plan.count {
            let index =
                ((u64::from(plan.head) + u64::from(k)) % u64::from(emitter.capacity)) as usize;
            if self.particles[index].is_alive() {
                self.counters.dropped = self.counters.dropped.wrapping_add(1);
            } else {
                self.particles[index] = spawn_particle(&uniform, k);
                self.counters.spawned = self.counters.spawned.wrapping_add(1);
                alive += 1;
            }
        }
        self.counters.alive = alive;
    }
}

/// The result of [`crate::ScenePipeline::advance_particles`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParticleTickReport {
    pub tick: u64,
    /// False when the tick id repeated the last simulated one (a no-op).
    pub advanced: bool,
    /// Live emitters after the call.
    pub emitters: usize,
    /// Spawns queued on the GPU this call, over all emitters.
    pub queued_spawns: u32,
    /// Spawns dropped this call because one tick asked for more than a pool holds.
    pub rate_limited: u32,
}

/// Observable state of the simulation clock and pools.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParticleReport {
    /// Ticks actually simulated since the pipeline was created.
    pub simulated_ticks: u64,
    /// Repeated tick ids ignored.
    pub repeated_ticks: u64,
    /// Id of the last simulated tick.
    pub last_tick: Option<u64>,
    /// One entry per live emitter, in id order.
    pub emitters: Vec<ParticleEmitterReport>,
}

/// CPU-known state of one live emitter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParticleEmitterReport {
    pub id: String,
    pub capacity: u32,
    /// Ticks this emitter has simulated since it was created.
    pub ticks: u64,
    /// Spawn attempts so far (each tried one ring slot).
    pub spawn_attempts: u64,
    /// Spawns dropped before any attempt because a single tick asked for more
    /// than the capacity. Ring-slot drops are in the GPU counters.
    pub rate_limited: u64,
    /// Slots that can ever have held a particle (the draw's instance count).
    pub high_water: u32,
}

/// What the render pass drew for one view in its last frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParticleViewReport {
    pub emitters_drawn: usize,
    pub instances: u32,
}

/// A blocking readback of one emitter, for tests and tools.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleReadback {
    pub particles: Vec<GpuParticle>,
    pub counters: ParticleCounters,
    pub spawn_attempts: u64,
    pub rate_limited: u64,
}

impl ParticleReadback {
    /// Spawns lost to a full pool: busy ring slots plus per-tick excess.
    pub fn dropped_total(&self) -> u64 {
        u64::from(self.counters.dropped) + self.rate_limited
    }

    /// Slots that are alive.
    pub fn alive_slots(&self) -> u32 {
        self.particles.iter().filter(|p| p.is_alive()).count() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emitter() -> ParticleEmitter {
        ParticleEmitter {
            capacity: 8,
            spawn_rate: 40.0,
            lifetime: [0.5, 0.5],
            speed: [2.0, 2.0],
            spread: 0.0,
            gravity: Vec3::ZERO,
            ..Default::default()
        }
    }

    #[test]
    fn default_emitter_and_tick_are_valid() {
        ParticleEmitter::default().validate().unwrap();
        ParticleTick { id: 1, dt: 0.016 }.validate().unwrap();
        validate_tick(&ParticleTick { id: 0, dt: 0.1 }, &[]).unwrap();
    }

    #[test]
    fn invalid_emitters_are_rejected_by_field() {
        type Mutation = fn(&mut ParticleEmitter);
        let cases: Vec<(&str, Mutation)> = vec![
            ("empty id", |e| e.id.clear()),
            ("long id", |e| e.id = "x".repeat(65)),
            ("nan position", |e| e.position.x = f32::NAN),
            ("far position", |e| e.position.y = 1e9),
            ("zero direction", |e| e.direction = Vec3::ZERO),
            ("infinite direction", |e| {
                e.direction = Vec3::splat(f32::INFINITY)
            }),
            ("zero capacity", |e| e.capacity = 0),
            ("huge capacity", |e| {
                e.capacity = MAX_PARTICLES_PER_EMITTER + 1
            }),
            ("negative rate", |e| e.spawn_rate = -1.0),
            ("nan rate", |e| e.spawn_rate = f32::NAN),
            ("huge rate", |e| e.spawn_rate = 1e9),
            ("zero lifetime", |e| e.lifetime = [0.0, 1.0]),
            ("negative lifetime", |e| e.lifetime = [-1.0, 1.0]),
            ("inverted lifetime", |e| e.lifetime = [2.0, 1.0]),
            ("long lifetime", |e| e.lifetime = [1.0, 1e6]),
            ("negative speed", |e| e.speed = [-1.0, 1.0]),
            ("inverted speed", |e| e.speed = [3.0, 1.0]),
            ("wide spread", |e| e.spread = 1.6),
            ("negative spread", |e| e.spread = -0.1),
            ("negative extent", |e| e.spawn_extent = -1.0),
            ("nan gravity", |e| e.gravity.z = f32::NAN),
            ("negative drag", |e| e.drag = -1.0),
            ("negative size", |e| e.size = [-0.1, 0.1]),
            ("nan size", |e| e.size[1] = f32::NAN),
            ("negative colour", |e| e.start_color.x = -0.5),
            ("alpha above one", |e| e.end_color.w = 1.5),
        ];
        for (name, mutate) in cases {
            let mut e = ParticleEmitter::default();
            mutate(&mut e);
            assert!(e.validate().is_err(), "{name} must be rejected");
        }
    }

    #[test]
    fn tick_and_set_validation_cover_dt_ids_and_budgets() {
        let ok = ParticleEmitter::default();
        for dt in [0.0, -0.1, f32::NAN, f32::INFINITY, MAX_TICK_DT * 2.0] {
            assert!(
                validate_tick(&ParticleTick { id: 1, dt }, std::slice::from_ref(&ok)).is_err(),
                "dt {dt}"
            );
        }
        let tick = ParticleTick {
            id: 1,
            dt: MAX_TICK_DT,
        };
        validate_tick(&tick, std::slice::from_ref(&ok)).unwrap();

        let twin = ParticleEmitter {
            id: "emitter".to_owned(),
            ..ok.clone()
        };
        assert!(
            validate_tick(&tick, &[ok.clone(), twin]).is_err(),
            "duplicate id"
        );

        let many: Vec<_> = (0..=MAX_EMITTERS)
            .map(|i| ParticleEmitter {
                id: format!("e{i}"),
                capacity: 1,
                ..ok.clone()
            })
            .collect();
        assert!(validate_tick(&tick, &many).is_err(), "too many emitters");
        assert!(validate_tick(&tick, &many[..MAX_EMITTERS]).is_ok());

        let big: Vec<_> = (0..5)
            .map(|i| ParticleEmitter {
                id: format!("big{i}"),
                capacity: MAX_PARTICLES_PER_EMITTER,
                ..ok.clone()
            })
            .collect();
        assert!(
            validate_tick(&tick, &big[..4]).is_ok(),
            "exactly the total budget"
        );
        assert!(validate_tick(&tick, &big).is_err(), "over the total budget");
    }

    #[test]
    fn spawn_plan_carries_fractions_and_wraps_the_ring() {
        let mut e = emitter();
        e.spawn_rate = 10.0;
        let mut cursor = EmitterCursor::default();
        // 0.25 s at 10/s is 2.5 particles: 2 now, the half carries over.
        let first = cursor.plan(&e, 0.25);
        assert_eq!((first.count, first.head, first.base), (2, 0, 0));
        let second = cursor.plan(&e, 0.25);
        assert_eq!((second.count, second.head, second.base), (3, 2, 2));
        assert_eq!(cursor.attempts(), 5);
        // Ring of 8: head 5 + 5 spawns wraps to 2.
        let third = cursor.plan(&e, 0.5);
        assert_eq!((third.count, third.head), (5, 5));
        let fourth = cursor.plan(&e, 0.1);
        assert_eq!((fourth.count, fourth.head), (1, 2));
        assert_eq!(cursor.high_water(e.capacity), 8);
    }

    #[test]
    fn spawn_plan_limits_one_tick_to_the_capacity() {
        let mut e = emitter();
        e.spawn_rate = 1000.0;
        let mut cursor = EmitterCursor::default();
        let plan = cursor.plan(&e, 0.1);
        assert_eq!((plan.count, plan.excess), (8, 92));
        assert_eq!(cursor.rate_limited(), 92);
        assert_eq!(cursor.attempts(), 8);
        e.spawn_rate = 0.0;
        assert_eq!(cursor.plan(&e, 0.1).count, 0);
    }

    #[test]
    fn random_streams_are_deterministic_unit_values() {
        for n in 0..64 {
            for stream in 0..7 {
                let a = unit_random(7, n, stream);
                assert_eq!(a, unit_random(7, n, stream));
                assert!((0.0..1.0).contains(&a), "{a}");
            }
        }
        assert_ne!(unit_random(1, 0, 0), unit_random(2, 0, 0));
        assert_ne!(unit_random(1, 0, 0), unit_random(1, 1, 0));
        assert_ne!(unit_random(1, 0, 0), unit_random(1, 0, 1));
        let mean: f32 = (0..4096).map(|n| unit_random(3, n, 0)).sum::<f32>() / 4096.0;
        assert!((mean - 0.5).abs() < 0.03, "mean {mean}");
    }

    #[test]
    fn launch_basis_is_orthonormal() {
        for axis in [
            Vec3::Y,
            Vec3::X,
            Vec3::NEG_Z,
            Vec3::new(1.0, 2.0, 3.0).normalize(),
        ] {
            let (t, b) = launch_basis(axis);
            assert!((t.length() - 1.0).abs() < 1e-5);
            assert!((b.length() - 1.0).abs() < 1e-5);
            assert!(t.dot(axis).abs() < 1e-5 && b.dot(axis).abs() < 1e-5);
            assert!(t.dot(b).abs() < 1e-5);
        }
    }

    #[test]
    fn uniform_layout_is_eleven_vec4s() {
        assert_eq!(EMITTER_UNIFORM_BYTES, 176);
        assert_eq!(PARTICLE_BYTES, 32);
        assert_eq!(COUNTERS_BYTES, 16);
    }

    #[test]
    fn reference_spawns_integrates_and_kills() {
        let e = emitter();
        let mut reference = ParticleReference::new(e.capacity);
        // 0.05 s at 40/s: 2 particles per tick, none expires for 0.5 s.
        for _ in 0..4 {
            reference.step(&e, 0.05);
        }
        assert_eq!(reference.counters.alive, 8);
        assert_eq!(reference.counters.spawned, 8);
        assert_eq!(reference.counters.dropped, 0);
        // Straight up at 2 m/s, no gravity or drag: the oldest moved 3 ticks.
        let oldest = reference.particles[0];
        assert!(
            (oldest.position[1] - 2.0 * 0.05 * 3.0).abs() < 1e-5,
            "{oldest:?}"
        );
        assert!(oldest.velocity[1] > 1.99 && oldest.velocity[1] < 2.01);
        // The oldest pair has taken 3 steps; 11 steps (age 0.55) is past its
        // 0.5 s life, with margin for float accumulation.
        let mut stopped = e.clone();
        stopped.spawn_rate = 0.0;
        for _ in 0..8 {
            reference.step(&stopped, 0.05);
        }
        assert!(reference.counters.alive < 8);
        for _ in 0..3 {
            reference.step(&stopped, 0.05);
        }
        assert_eq!(reference.counters.alive, 0);
        assert_eq!(reference.counters.killed, 8);
        assert!(reference.particles.iter().all(|p| !p.is_alive()));
    }

    #[test]
    fn reference_overflow_drops_and_counts_spawns() {
        let mut e = emitter();
        e.lifetime = [100.0, 100.0];
        e.spawn_rate = 400.0;
        let mut reference = ParticleReference::new(e.capacity);
        for _ in 0..5 {
            reference.step(&e, 0.05);
        }
        // 20 spawns per tick asked, 8 attempted per tick: the first fills the
        // pool, every later attempt hits a live slot.
        assert_eq!(reference.counters.alive, 8);
        assert_eq!(reference.counters.spawned, 8);
        assert_eq!(reference.counters.dropped, 32);
        assert_eq!(reference.cursor.rate_limited(), 60);
    }

    #[test]
    fn reference_is_deterministic_and_seeded() {
        let mut a = ParticleReference::new(8);
        let mut b = ParticleReference::new(8);
        let mut c = ParticleReference::new(8);
        let e = ParticleEmitter {
            spread: 0.5,
            spawn_extent: 0.2,
            drag: 0.5,
            ..emitter()
        };
        let other = ParticleEmitter {
            seed: 99,
            ..e.clone()
        };
        for _ in 0..6 {
            a.step(&e, 0.05);
            b.step(&e, 0.05);
            c.step(&other, 0.05);
        }
        assert_eq!(a.particles, b.particles);
        assert_ne!(a.particles, c.particles);
    }

    #[test]
    fn drag_and_gravity_shape_the_velocity() {
        let mut e = emitter();
        e.spawn_rate = 20.0;
        e.lifetime = [10.0, 10.0];
        e.gravity = Vec3::new(0.0, -10.0, 0.0);
        e.drag = 2.0;
        let mut reference = ParticleReference::new(e.capacity);
        reference.step(&e, 0.05);
        e.spawn_rate = 0.0;
        reference.step(&e, 0.1);
        let particle = reference.particles[0];
        // (2 - 10 * 0.1) * (1 - 2 * 0.1) = 0.8
        assert!((particle.velocity[1] - 0.8).abs() < 1e-4, "{particle:?}");
    }

    #[test]
    fn gpu_particle_matching_uses_the_documented_tolerance() {
        let a = GpuParticle {
            position: [1.0, 2.0, 3.0],
            age: 0.1,
            velocity: [0.0, 1.0, 0.0],
            lifetime: 1.0,
        };
        let mut b = a;
        b.position[0] += 0.5 * REFERENCE_TOLERANCE;
        assert!(b.matches(&a));
        b.position[0] += 10.0 * REFERENCE_TOLERANCE;
        assert!(!b.matches(&a));
    }
}
