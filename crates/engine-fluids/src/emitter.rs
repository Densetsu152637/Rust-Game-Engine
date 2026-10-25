//! Deterministic emitters, kill regions and lattice fills.

use crate::{Error, Result, params::MAX_COORDINATE};
use glam::{Vec2, Vec3};

/// Most emitted particles per second accepted by a rate emitter.
const MAX_RATE: f32 = 1.0e7;
/// Largest nozzle radius in particle spacings.
const MAX_NOZZLE_SPACINGS: f32 = 64.0;

/// Axis-aligned box or sphere region used by fills, rate emitters and kill volumes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Region {
    Box { center: Vec3, half_extents: Vec3 },
    Sphere { center: Vec3, radius: f32 },
}

impl Region {
    /// Reject non-finite, non-positive or unbounded regions.
    pub fn validate(&self) -> Result<()> {
        let (center, extents) = match *self {
            Self::Box {
                center,
                half_extents,
            } => (center, half_extents),
            Self::Sphere { center, radius } => (center, Vec3::splat(radius)),
        };
        if !center.is_finite() || center.abs().max_element() > MAX_COORDINATE {
            return Err(Error::Invalid("region centre must be finite and bounded"));
        }
        if !extents.is_finite()
            || extents.min_element() <= 0.0
            || extents.max_element() > MAX_COORDINATE
        {
            return Err(Error::Invalid(
                "region extents must be finite, positive and bounded",
            ));
        }
        Ok(())
    }

    pub fn contains(&self, p: Vec3) -> bool {
        match *self {
            Self::Box {
                center,
                half_extents,
            } => (p - center).abs().cmple(half_extents).all(),
            Self::Sphere { center, radius } => p.distance_squared(center) <= radius * radius,
        }
    }

    /// Cubic lattice points at `spacing` inside the region, centred in it, in x-fastest order.
    pub fn lattice_points(&self, spacing: f32) -> Vec<Vec3> {
        let mut points = Vec::new();
        match *self {
            Self::Box {
                center,
                half_extents,
            } => {
                let counts = (half_extents * 2.0 / spacing + 1.0e-3).floor();
                let start = center - (counts - Vec3::ONE) * 0.5 * spacing;
                let [nx, ny, nz] = counts.to_array().map(|c| c as u32);
                for k in 0..nz {
                    for j in 0..ny {
                        for i in 0..nx {
                            points.push(start + Vec3::new(i as f32, j as f32, k as f32) * spacing);
                        }
                    }
                }
            }
            Self::Sphere { center, radius } => {
                let reach = (radius / spacing).floor() as i32;
                let limit = (radius - 0.5 * spacing).max(0.0);
                for k in -reach..=reach {
                    for j in -reach..=reach {
                        for i in -reach..=reach {
                            let offset = Vec3::new(i as f32, j as f32, k as f32) * spacing;
                            if offset.length() <= limit + 1.0e-6 {
                                points.push(center + offset);
                            }
                        }
                    }
                }
            }
        }
        points
    }

    fn sample(&self, rng: &mut SplitMix64) -> Vec3 {
        match *self {
            Self::Box {
                center,
                half_extents,
            } => center + half_extents * (rng.unit3() * 2.0 - Vec3::ONE),
            Self::Sphere { center, radius } => {
                for _ in 0..64 {
                    let p = rng.unit3() * 2.0 - Vec3::ONE;
                    if p.length_squared() <= 1.0 {
                        return center + p * radius;
                    }
                }
                center
            }
        }
    }
}

/// How an emitter produces particles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EmitterKind {
    /// `per_second` particles at seeded pseudo-random positions inside `region`, with
    /// `velocity`. Fractional particles carry over between steps.
    Rate {
        region: Region,
        per_second: f32,
        velocity: Vec3,
        seed: u64,
    },
    /// Disc layers of lattice points (one particle spacing apart) leaving `position` along
    /// `direction` at `speed`; the disc has `radius`.
    Nozzle {
        position: Vec3,
        direction: Vec3,
        radius: f32,
        speed: f32,
    },
}

/// One emitter of a volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmitterDesc {
    pub kind: EmitterKind,
    /// Seconds each emitted particle lives, or `None` for unlimited.
    pub lifetime: Option<f32>,
    pub enabled: bool,
}

impl EmitterDesc {
    pub(crate) fn validate(&self, spacing: f32) -> Result<()> {
        validate_lifetime(self.lifetime)?;
        match self.kind {
            EmitterKind::Rate {
                region,
                per_second,
                velocity,
                ..
            } => {
                region.validate()?;
                if !(per_second.is_finite() && (0.0..=MAX_RATE).contains(&per_second)) {
                    return Err(Error::Invalid(
                        "emitter rate must be finite and within 0..=1e7 per second",
                    ));
                }
                validate_velocity(velocity)
            }
            EmitterKind::Nozzle {
                position,
                direction,
                radius,
                speed,
            } => {
                if !position.is_finite() || position.abs().max_element() > MAX_COORDINATE {
                    return Err(Error::Invalid("nozzle position must be finite and bounded"));
                }
                if !direction.is_finite() || direction.length() < 1.0e-6 {
                    return Err(Error::Invalid(
                        "nozzle direction must be finite and non-zero",
                    ));
                }
                if !(radius.is_finite() && radius > 0.0 && radius <= MAX_NOZZLE_SPACINGS * spacing)
                {
                    return Err(Error::Invalid(
                        "nozzle radius must be positive and at most 64 particle spacings",
                    ));
                }
                if !(speed.is_finite() && speed > 0.0 && speed <= 1000.0) {
                    return Err(Error::Invalid("nozzle speed must be within (0, 1000] m/s"));
                }
                Ok(())
            }
        }
    }
}

pub(crate) fn validate_lifetime(lifetime: Option<f32>) -> Result<()> {
    match lifetime {
        Some(t) if !(t.is_finite() && t > 0.0) => Err(Error::Invalid(
            "particle lifetime must be finite and positive",
        )),
        _ => Ok(()),
    }
}

pub(crate) fn validate_velocity(velocity: Vec3) -> Result<()> {
    if velocity.is_finite() && velocity.length() <= 1000.0 {
        Ok(())
    } else {
        Err(Error::Invalid(
            "particle velocity must be finite and at most 1000 m/s",
        ))
    }
}

/// Runtime state of an emitter: fractional accumulator and random stream.
#[derive(Clone, Debug)]
pub(crate) struct EmitterState {
    pub desc: EmitterDesc,
    accumulator: f64,
    rng: SplitMix64,
}

/// One particle to spawn.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Spawn {
    pub position: Vec3,
    pub velocity: Vec3,
}

impl EmitterState {
    pub fn new(desc: EmitterDesc) -> Self {
        let seed = match desc.kind {
            EmitterKind::Rate { seed, .. } => seed,
            EmitterKind::Nozzle { .. } => 0,
        };
        Self {
            desc,
            accumulator: 0.0,
            rng: SplitMix64(seed),
        }
    }

    /// Replace the description, keeping the accumulator; the random stream restarts only
    /// when the seed changes.
    pub fn update(&mut self, desc: EmitterDesc) {
        let seed_changed = match (self.desc.kind, desc.kind) {
            (EmitterKind::Rate { seed: a, .. }, EmitterKind::Rate { seed: b, .. }) => a != b,
            _ => true,
        };
        if seed_changed {
            *self = Self::new(desc);
        } else {
            self.desc = desc;
        }
    }

    /// Advance by `dt` and append at most `room` spawns. Returns the number of particles the
    /// emitter produced but that did not fit.
    pub fn emit(&mut self, dt: f32, spacing: f32, room: usize, out: &mut Vec<Spawn>) -> usize {
        if !self.desc.enabled {
            return 0;
        }
        let start = out.len();
        let mut dropped = 0;
        match self.desc.kind {
            EmitterKind::Rate {
                region,
                per_second,
                velocity,
                ..
            } => {
                self.accumulator += f64::from(per_second) * f64::from(dt);
                let count = self.accumulator.floor();
                self.accumulator -= count;
                let count = count as usize;
                let fit = count.min(room);
                for _ in 0..fit {
                    out.push(Spawn {
                        position: region.sample(&mut self.rng),
                        velocity,
                    });
                }
                dropped += count - fit;
            }
            EmitterKind::Nozzle {
                position,
                direction,
                radius,
                speed,
            } => {
                let direction = direction.normalize();
                let step = f64::from(spacing);
                self.accumulator += f64::from(speed) * f64::from(dt);
                let layers = (self.accumulator / step).floor();
                self.accumulator -= layers * step;
                let layers = layers as usize;
                if layers == 0 {
                    return 0;
                }
                let disc = nozzle_disc(direction, radius, spacing);
                for layer in 0..layers {
                    // Older layers have travelled further from the nozzle.
                    let travelled = (self.accumulator + (layers - 1 - layer) as f64 * step) as f32;
                    let fit = disc.len().min(room - (out.len() - start));
                    out.extend(disc[..fit].iter().map(|&offset| Spawn {
                        position: position + offset + direction * travelled,
                        velocity: direction * speed,
                    }));
                    if fit < disc.len() {
                        dropped += disc.len() - fit + (layers - 1 - layer) * disc.len();
                        break;
                    }
                }
            }
        }
        dropped
    }
}

/// Lattice points of a nozzle disc perpendicular to `direction`.
fn nozzle_disc(direction: Vec3, radius: f32, spacing: f32) -> Vec<Vec3> {
    let (u, v) = direction.any_orthonormal_pair();
    let reach = (radius / spacing).floor() as i32;
    let mut points = Vec::new();
    for b in -reach..=reach {
        for a in -reach..=reach {
            let offset = Vec2::new(a as f32, b as f32) * spacing;
            if offset.length_squared() <= radius * radius * (1.0 + 1.0e-6) {
                points.push(u * offset.x + v * offset.y);
            }
        }
    }
    points
}

/// SplitMix64 pseudo-random generator: tiny, seedable and identical on every platform.
#[derive(Clone, Copy, Debug)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn unit3(&mut self) -> Vec3 {
        let x = self.unit();
        let y = self.unit();
        let z = self.unit();
        Vec3::new(x, y, z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_lattice_is_centred_and_counted() {
        let region = Region::Box {
            center: Vec3::new(0.5, 0.25, 0.5),
            half_extents: Vec3::new(0.5, 0.25, 0.5),
        };
        let points = region.lattice_points(0.1);
        assert_eq!(points.len(), 10 * 5 * 10);
        assert!(points[0].distance(Vec3::splat(0.05)) < 1e-6);
        assert!(points.iter().all(|&p| region.contains(p)));
    }

    #[test]
    fn nozzle_disc_counts_lattice_points_inside_radius() {
        let disc = nozzle_disc(Vec3::Y, 0.1, 0.05);
        // Integer points within radius 2 of the origin: 13.
        assert_eq!(disc.len(), 13);
        assert!(disc.iter().all(|p| p.y.abs() < 1e-6));
    }
}
