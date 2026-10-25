//! Shared particle state, integration, pins, distance constraints and proxy contacts.
use glam::Vec3;

use crate::proxy::{ImpulseAccum, Prepared};
use crate::{Error, MAX_COORDINATE, MAX_SPEED, Result, finite_bounded, in_range};

/// Largest accepted per-particle inverse mass (1/kg).
pub(crate) const MAX_INVERSE_MASS: f32 = 1.0e6;

/// Particle arrays in struct-of-arrays layout. `w` is the effective inverse mass used by
/// the solver: zero while a particle is pinned or authored as static.
#[derive(Clone, Debug)]
pub(crate) struct Particles {
    pub x: Vec<Vec3>,
    pub prev: Vec<Vec3>,
    pub v: Vec<Vec3>,
    pub w: Vec<f32>,
    authored_w: Vec<f32>,
    pin: Vec<Option<Vec3>>,
    pin_start: Vec<Vec3>,
    pins_changed: bool,
}

impl Particles {
    pub fn new(x: Vec<Vec3>, authored_w: Vec<f32>) -> Self {
        let n = x.len();
        Self {
            prev: x.clone(),
            x,
            v: vec![Vec3::ZERO; n],
            w: authored_w.clone(),
            authored_w,
            pin: vec![None; n],
            pin_start: vec![Vec3::ZERO; n],
            pins_changed: true,
        }
    }

    pub fn len(&self) -> usize {
        self.x.len()
    }

    pub fn inverse_masses(&self) -> &[f32] {
        &self.authored_w
    }

    pub fn pin_target(&self, particle: usize) -> Option<Vec3> {
        self.pin.get(particle).copied().flatten()
    }

    pub fn is_pinned(&self, particle: usize) -> bool {
        self.pin_target(particle).is_some()
    }

    /// True once since the last call when the pinned set changed.
    pub fn take_pins_changed(&mut self) -> bool {
        std::mem::take(&mut self.pins_changed)
    }

    pub fn set_pin_target(&mut self, particle: usize, target: Vec3) -> Result<()> {
        if particle >= self.len() {
            return Err(Error("particle index out of range"));
        }
        if !finite_bounded(target, MAX_COORDINATE) {
            return Err(Error("invalid pin target"));
        }
        if self.pin[particle].is_none() {
            self.pins_changed = true;
        }
        self.pin[particle] = Some(target);
        self.w[particle] = 0.0;
        Ok(())
    }

    pub fn unpin(&mut self, particle: usize) -> Result<()> {
        if particle >= self.len() {
            return Err(Error("particle index out of range"));
        }
        if self.pin[particle].take().is_some() {
            self.pins_changed = true;
        }
        self.w[particle] = self.authored_w[particle];
        Ok(())
    }

    pub fn set_inverse_mass(&mut self, particle: usize, inverse_mass: f32) -> Result<()> {
        if particle >= self.len() {
            return Err(Error("particle index out of range"));
        }
        if !in_range(inverse_mass, 0.0, MAX_INVERSE_MASS) {
            return Err(Error("invalid inverse mass"));
        }
        self.authored_w[particle] = inverse_mass;
        if self.pin[particle].is_none() {
            self.w[particle] = inverse_mass;
        }
        Ok(())
    }

    /// Records where pinned particles start so substeps can interpolate toward the target.
    pub fn begin_step(&mut self) {
        for ((start, x), pin) in self.pin_start.iter_mut().zip(&self.x).zip(&self.pin) {
            if pin.is_some() {
                *start = *x;
            }
        }
    }

    /// Explicit prediction: `v += h a`, `x += h v`. Pinned particles move linearly toward
    /// their target and land on it exactly at the last substep.
    pub fn predict(
        &mut self,
        h: f32,
        gravity: Vec3,
        forces: Option<&[Vec3]>,
        substep: u32,
        substeps: u32,
    ) {
        let t = (substep + 1) as f32 / substeps as f32;
        let last = substep + 1 == substeps;
        for i in 0..self.x.len() {
            self.prev[i] = self.x[i];
            if let Some(target) = self.pin[i] {
                self.x[i] = if last {
                    target
                } else {
                    self.pin_start[i].lerp(target, t)
                };
                continue;
            }
            let w = self.w[i];
            if w <= 0.0 {
                continue;
            }
            let mut acceleration = gravity;
            if let Some(forces) = forces {
                acceleration += forces[i] * w;
            }
            self.v[i] += acceleration * h;
            // Aerodynamic forces scale with the inverse mass (up to 1e6) and the squared air
            // speed; without this clamp a validated cloth in a strong wind overflows to
            // infinity within a few steps.
            let speed = self.v[i].length();
            if speed > MAX_SPEED {
                self.v[i] *= MAX_SPEED / speed;
            }
            self.x[i] += self.v[i] * h;
        }
    }

    /// Derives velocities from the substep displacement and applies velocity damping.
    pub fn finish_substep(&mut self, h: f32, damping: f32) {
        let inverse_h = 1.0 / h;
        let damp = 1.0 / (1.0 + damping * h);
        for ((v, x), (prev, w)) in self
            .v
            .iter_mut()
            .zip(&self.x)
            .zip(self.prev.iter().zip(&self.w))
        {
            let mut velocity = (*x - *prev) * inverse_h;
            if *w > 0.0 {
                velocity *= damp;
            }
            let speed = velocity.length();
            if speed > MAX_SPEED {
                velocity *= MAX_SPEED / speed;
            }
            *v = velocity;
        }
    }

    pub fn aabb(&self) -> (Vec3, Vec3) {
        self.x.iter().fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(min, max), p| (min.min(*p), max.max(*p)),
        )
    }

    pub fn all_finite(&self) -> bool {
        self.x.iter().all(|p| p.is_finite()) && self.v.iter().all(|v| v.is_finite())
    }

    /// Projects particles out of every proxy (in id order) to `margin` above its surface,
    /// applies Coulomb friction relative to the moving proxy surface and accumulates the
    /// reaction impulse `-m dx / h` on the proxy.
    pub fn collide(
        &mut self,
        proxies: &[Prepared],
        accum: &mut [ImpulseAccum],
        contact: Contact,
        h: f32,
    ) {
        if proxies.is_empty() {
            return;
        }
        let (min, max) = self.aabb();
        for (proxy, sum) in proxies.iter().zip(accum.iter_mut()) {
            if !proxy.may_touch(min, max, contact.margin) {
                continue;
            }
            let mu = 0.5 * (contact.friction + proxy.friction);
            for i in 0..self.x.len() {
                let w = self.w[i];
                if w <= 0.0 {
                    continue;
                }
                let p = self.x[i];
                let (distance, normal) = proxy.query(p);
                let penetration = contact.margin - distance;
                if penetration <= 0.0 {
                    continue;
                }
                let mut corrected = p + normal * penetration;
                let relative = (corrected - self.prev[i]) - proxy.surface_velocity(corrected) * h;
                let tangential = relative - normal * relative.dot(normal);
                let slip = tangential.length();
                if slip > 0.0 {
                    corrected -= tangential * (mu * penetration / slip).min(1.0);
                }
                self.x[i] = corrected;
                let impulse = (p - corrected) / (w * h);
                sum.add(impulse, corrected - normal * contact.margin, proxy.position);
            }
        }
    }
}

/// World-level inputs of one step, shared by every object.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StepContext {
    pub dt: f32,
    pub substeps: u32,
    pub iterations: u32,
    pub gravity: Vec3,
    pub wind: Vec3,
    pub air_density: f32,
}

impl StepContext {
    pub fn substep(&self) -> f32 {
        self.dt / self.substeps as f32
    }
}

/// Contact parameters of one object.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Contact {
    pub margin: f32,
    pub friction: f32,
}

/// Distance constraint between particles `a` and `b`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Distance {
    pub a: u32,
    pub b: u32,
    pub rest: f32,
}

/// One Gauss-Seidel XPBD pass over distance constraints with scaled compliance
/// `alpha_tilde = compliance / h^2`.
pub(crate) fn solve_distances(
    x: &mut [Vec3],
    w: &[f32],
    constraints: &[Distance],
    lambdas: &mut [f32],
    alpha_tilde: f32,
) {
    for (constraint, lambda) in constraints.iter().zip(lambdas.iter_mut()) {
        let (a, b) = (constraint.a as usize, constraint.b as usize);
        let weight = w[a] + w[b];
        if weight <= 0.0 {
            continue;
        }
        let delta = x[a] - x[b];
        let length = delta.length();
        if length < 1.0e-9 {
            continue;
        }
        let normal = delta / length;
        let c = length - constraint.rest;
        let d_lambda = (-c - alpha_tilde * *lambda) / (weight + alpha_tilde);
        *lambda += d_lambda;
        x[a] += normal * (d_lambda * w[a]);
        x[b] -= normal * (d_lambda * w[b]);
    }
}

/// Validates the particle count and that every position is finite and bounded.
pub(crate) fn validate_positions(positions: &[Vec3], max: usize, min: usize) -> Result<()> {
    if positions.len() < min || positions.len() > max {
        return Err(Error("particle count out of range"));
    }
    if !positions.iter().all(|p| finite_bounded(*p, MAX_COORDINATE)) {
        return Err(Error("invalid particle positions"));
    }
    Ok(())
}

/// Validates explicit inverse masses or derives them from per-particle masses.
pub(crate) fn inverse_masses(explicit: Option<&[f32]>, derived_masses: &[f32]) -> Result<Vec<f32>> {
    match explicit {
        Some(values) => {
            if values.len() != derived_masses.len() {
                return Err(Error("inverse mass count must match particle count"));
            }
            if !values.iter().all(|w| in_range(*w, 0.0, MAX_INVERSE_MASS)) {
                return Err(Error("invalid inverse mass"));
            }
            Ok(values.to_vec())
        }
        None => derived_masses
            .iter()
            .map(|m| {
                let w = 1.0 / m;
                if m.is_finite() && *m > 0.0 && w <= MAX_INVERSE_MASS {
                    Ok(w)
                } else {
                    Err(Error("derived particle mass out of range"))
                }
            })
            .collect(),
    }
}

/// Validates pin indices and pins each particle at its rest position.
pub(crate) fn apply_initial_pins(particles: &mut Particles, pinned: &[u32]) -> Result<()> {
    for &index in pinned {
        let index = index as usize;
        let Some(&position) = particles.x.get(index) else {
            return Err(Error("pinned particle index out of range"));
        };
        particles.set_pin_target(index, position)?;
    }
    Ok(())
}
