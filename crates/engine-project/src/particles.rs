//! Authored GPU particles: the `ParticleEmitter` component and the per-tick
//! [`ParticleFrame`] a host drives into the renderer.
//!
//! An emitter sits on an entity with a `Transform`: it spawns at the entity's
//! world position and launches along its local +Y axis rotated into world
//! space. The renderer simulates all emitters once per scene tick on the GPU
//! (docs/rendering-particles.md); rendering never advances them.
use crate::{Error, Result};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};

/// The built-in particle feature of the render pipeline.
pub const PARTICLES_FEATURE: &str = "engine.particles";
/// Most enabled emitters per scene.
pub const MAX_PARTICLE_EMITTERS: usize = 16;
/// Largest `capacity` of one emitter.
pub const MAX_EMITTER_CAPACITY: u32 = 65_536;
/// Largest sum of the capacities of all enabled emitters.
pub const MAX_TOTAL_EMITTER_CAPACITY: u32 = 262_144;
/// Longest simulation step the renderer accepts; longer scene ticks are clamped.
pub const MAX_PARTICLE_DT: f32 = 0.25;

const MAX_EXTENT: f32 = 100_000.0;
const MAX_RADIANCE: f32 = 1000.0;

fn one() -> u32 {
    1
}

/// How an emitter's billboards combine with the scene colour.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ParticleBlend3D {
    /// Premultiplied alpha "over", in slot (spawn) order without a depth sort.
    #[default]
    Alpha,
    /// Add `rgb * alpha`: order independent.
    Additive,
}

/// A fixed-capacity GPU particle emitter at its entity's world position.
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ParticleEmitter {
    #[serde(default = "one")]
    pub version: u32,
    /// Disabled emitters are not extracted; their pool is freed.
    pub enabled: bool,
    /// Pool size, `1..=65536`. Spawns beyond it are dropped and counted.
    pub capacity: u32,
    /// Particles per second, `0..=1e6`.
    pub spawn_rate: f32,
    /// Lifetime range in seconds, `0 < min <= max <= 3600`.
    pub lifetime: [f32; 2],
    /// Launch speed range in metres per second, `0 <= min <= max <= 10000`.
    pub speed: [f32; 2],
    /// Half angle of the launch cone in radians, `0..=1.5`.
    pub spread: f32,
    /// Half size of the spawn box in metres, `0..=100000`.
    pub spawn_extent: f32,
    /// Launch axis in the entity's local space; finite and nonzero.
    pub direction: [f32; 3],
    /// World-space acceleration in metres per second squared.
    pub gravity: [f32; 3],
    /// Linear drag per second, `0..=1000`.
    pub drag: f32,
    /// Billboard half size in metres at birth and at death, each `0..=1000`.
    pub size: [f32; 2],
    /// Linear RGBA at birth, rgb `0..=1000`, alpha `0..=1`.
    pub start_color: [f32; 4],
    /// Linear RGBA at death.
    pub end_color: [f32; 4],
    pub blend: ParticleBlend3D,
    /// Seed of the deterministic random streams.
    pub seed: u32,
}

impl Default for ParticleEmitter {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            capacity: 1024,
            spawn_rate: 100.0,
            lifetime: [1.0, 2.0],
            speed: [1.0, 3.0],
            spread: 0.35,
            spawn_extent: 0.0,
            direction: [0.0, 1.0, 0.0],
            gravity: [0.0, -9.81, 0.0],
            drag: 0.0,
            size: [0.1, 0.05],
            start_color: [1.0; 4],
            end_color: [1.0, 1.0, 1.0, 0.0],
            blend: ParticleBlend3D::Alpha,
            seed: 1,
        }
    }
}

fn in_range(value: f32, low: f32, high: f32) -> bool {
    value.is_finite() && (low..=high).contains(&value)
}

fn color_ok(color: &[f32; 4]) -> bool {
    color[..3].iter().all(|c| in_range(*c, 0.0, MAX_RADIANCE)) && in_range(color[3], 0.0, 1.0)
}

impl ParticleEmitter {
    /// Reject unsupported versions, non-finite values, zero or excess
    /// capacity and anything outside the documented ranges.
    pub fn validate(&self) -> Result<()> {
        let length_squared: f32 = self.direction.iter().map(|v| v * v).sum();
        let ok = self.version == 1
            && (1..=MAX_EMITTER_CAPACITY).contains(&self.capacity)
            && in_range(self.spawn_rate, 0.0, 1_000_000.0)
            && in_range(self.lifetime[0], f32::MIN_POSITIVE, 3600.0)
            && in_range(self.lifetime[1], self.lifetime[0], 3600.0)
            && in_range(self.speed[0], 0.0, 10_000.0)
            && in_range(self.speed[1], self.speed[0], 10_000.0)
            && in_range(self.spread, 0.0, 1.5)
            && in_range(self.spawn_extent, 0.0, MAX_EXTENT)
            && self.direction.iter().all(|v| v.is_finite())
            && length_squared.is_finite()
            && length_squared > 1e-12
            && self
                .gravity
                .iter()
                .all(|v| in_range(*v, -MAX_EXTENT, MAX_EXTENT))
            && in_range(self.drag, 0.0, 1000.0)
            && self.size.iter().all(|v| in_range(*v, 0.0, 1000.0))
            && color_ok(&self.start_color)
            && color_ok(&self.end_color);
        if ok {
            Ok(())
        } else {
            Err(Error::new(
                "render.particles.invalid",
                "ParticleEmitter requires version 1, capacity 1..=65536, finite values in their \
                 documented ranges, 0 < lifetime min <= max and a nonzero direction",
            ))
        }
    }
}

/// One enabled emitter of a [`ParticleFrame`].
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleEmitterInstance {
    /// The entity's authored ID (or `entity:<bits>`): the emitter's identity
    /// across ticks.
    pub id: String,
    pub emitter: ParticleEmitter,
    /// Column-major world transform (position in column 3).
    pub world: [[f32; 4]; 4],
}

/// The particle input of one scene tick: what to simulate and when.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParticleFrame {
    /// Scene ticks completed so far. Feeding the same frame (same `tick`) to
    /// the renderer again does not advance the simulation.
    pub tick: u64,
    /// Seconds simulated by the last scene tick.
    pub dt: f32,
    /// Enabled emitters in stable ID order.
    pub emitters: Vec<ParticleEmitterInstance>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_round_trip() {
        let emitter = ParticleEmitter::default();
        emitter.validate().unwrap();
        let json = serde_json::to_value(&emitter).unwrap();
        let back: ParticleEmitter = serde_json::from_value(json).unwrap();
        assert_eq!(back, emitter);
        let sparse: ParticleEmitter = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(sparse, emitter);
    }

    #[test]
    fn invalid_values_are_rejected() {
        let bad: Vec<fn(&mut ParticleEmitter)> = vec![
            |e| e.version = 2,
            |e| e.capacity = 0,
            |e| e.capacity = MAX_EMITTER_CAPACITY + 1,
            |e| e.spawn_rate = -1.0,
            |e| e.spawn_rate = f32::NAN,
            |e| e.lifetime = [0.0, 1.0],
            |e| e.lifetime = [2.0, 1.0],
            |e| e.speed = [-1.0, 1.0],
            |e| e.spread = 2.0,
            |e| e.direction = [0.0; 3],
            |e| e.direction[0] = f32::INFINITY,
            |e| e.gravity[1] = f32::NAN,
            |e| e.drag = -0.5,
            |e| e.size = [0.1, -0.1],
            |e| e.start_color[3] = 2.0,
            |e| e.end_color[0] = -1.0,
        ];
        for (index, mutate) in bad.iter().enumerate() {
            let mut emitter = ParticleEmitter::default();
            mutate(&mut emitter);
            assert!(emitter.validate().is_err(), "case {index}");
        }
    }
}
