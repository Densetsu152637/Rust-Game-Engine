//! Direct path: distance, listener-frame direction and volumetric occlusion with
//! per-band transmission (section 5.4.1).

use crate::math::{self, Vec3};
use crate::rng::fibonacci_sphere;
use crate::world::ray_offset;
use crate::{AcousticWorld, AcousticsError, BANDS, DirectPath, ListenerPose, Result};

/// Most crossed surfaces whose transmission is multiplied; a ray crossing more is
/// fully blocked.
pub const MAX_TRANSMISSION_SURFACES: u32 = 4;

/// Orthonormal listener basis `(right, up, forward)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListenerFrame {
    pub position: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
}
impl ListenerFrame {
    /// Validates the pose (finite, non-zero, non-parallel forward and up).
    pub fn new(pose: &ListenerPose) -> Result<Self> {
        let bad = || AcousticsError::new("acoustics.request.invalid", "listener pose is invalid");
        if !math::finite3(pose.position) {
            return Err(bad());
        }
        let forward = math::normalize(pose.forward).ok_or_else(bad)?;
        let right = math::normalize(math::cross(forward, pose.up)).ok_or_else(bad)?;
        let up = math::cross(right, forward);
        Ok(Self {
            position: pose.position,
            right,
            up,
            forward,
        })
    }
    /// A world direction in the listener frame (`x` right, `y` up, `-z` forward,
    /// matching the mixer's default listener that faces `-Z` with `+Y` up).
    pub fn to_local(&self, v: Vec3) -> Vec3 {
        [
            math::dot(v, self.right),
            math::dot(v, self.up),
            -math::dot(v, self.forward),
        ]
    }
}

/// Distance, direction and occlusion of the straight path from `source` to the
/// listener. With `occlusion`, `rays` rays go from the listener to Fibonacci-sphere
/// points of radius `radius_m` around the source; a ray's energy factor is the
/// product of the transmissions of the surfaces it crosses (0 beyond
/// [`MAX_TRANSMISSION_SURFACES`]) and 1 when clear. `gains` is the square root of
/// the mean factor (`v + (1 - v) mean(T)` as energy, applied as amplitude).
pub fn direct_path(
    world: &AcousticWorld,
    listener: &ListenerFrame,
    source: Vec3,
    radius_m: f32,
    rays: u16,
    occlusion: bool,
) -> DirectPath {
    let to_source = math::sub(source, listener.position);
    let distance = math::length(to_source);
    let direction = math::normalize(to_source).map_or([0.0, 0.0, -1.0], |d| listener.to_local(d));
    if !occlusion || rays == 0 {
        return DirectPath {
            visible_fraction: 1.0,
            gains: [1.0; BANDS],
            distance_m: distance,
            direction,
        };
    }
    let mut visible = 0u32;
    let mut energy = [0.0f64; BANDS];
    for i in 0..u32::from(rays) {
        let target = if radius_m > 0.0 {
            math::add(
                source,
                math::scale(fibonacci_sphere(i, u32::from(rays)), radius_m),
            )
        } else {
            source
        };
        let t = transmission_to(world, listener.position, target);
        if t == [1.0; BANDS] {
            visible += 1;
        }
        for b in 0..BANDS {
            energy[b] += f64::from(t[b]);
        }
    }
    let n = f64::from(rays);
    DirectPath {
        visible_fraction: visible as f32 / f32::from(rays),
        gains: energy.map(|e| (e / n).sqrt() as f32),
        distance_m: distance,
        direction,
    }
}

/// Energy transmission factor along the segment `from -> to`.
fn transmission_to(world: &AcousticWorld, from: Vec3, to: Vec3) -> [f32; BANDS] {
    let Some(dir) = math::normalize(math::sub(to, from)) else {
        return [1.0; BANDS];
    };
    let mut remaining = math::length(math::sub(to, from));
    let mut origin = from;
    let mut t = [1.0f32; BANDS];
    for crossing in 0..=MAX_TRANSMISSION_SURFACES {
        let Some(hit) = world.nearest(origin, dir, 0.0, remaining, u8::MAX) else {
            return t;
        };
        if crossing == MAX_TRANSMISSION_SURFACES {
            return [0.0; BANDS];
        }
        let tr = world.transmission(world.hit_material(&hit));
        for b in 0..BANDS {
            t[b] *= tr[b];
        }
        if t.iter().all(|v| *v == 0.0) {
            return t;
        }
        let p = math::add(origin, math::scale(dir, hit.t));
        let step = ray_offset(p);
        origin = math::add(p, math::scale(dir, step));
        remaining -= hit.t + step;
        if remaining <= 0.0 {
            return t;
        }
    }
    t
}
