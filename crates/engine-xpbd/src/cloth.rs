//! Triangle-mesh cloth: stretch, dihedral bending, long-range tethers, aerodynamics, pins.
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::f32::consts::{PI, TAU};

use glam::Vec3;

use crate::mesh::{grid_mesh, hinges, unique_edges, validate_triangles};
use crate::particles::{
    Contact, Distance, Particles, StepContext, apply_initial_pins, inverse_masses, solve_distances,
};
use crate::proxy::{ImpulseAccum, Prepared};
use crate::{Error, MAX_COMPLIANCE, MAX_DAMPING, MAX_EXTENT, MAX_FRICTION, Result, in_range};

/// Largest accepted aerodynamic drag or lift coefficient.
const MAX_AERO_COEFFICIENT: f32 = 100.0;
/// Largest accepted tether slack factor.
const MAX_TETHER_SLACK: f32 = 10.0;
/// Largest accepted areal density in kg/m^2.
const MAX_AREAL_DENSITY: f32 = 1.0e4;

/// Runtime-tunable cloth material. Compliance is the inverse of stiffness; zero is as
/// stiff as the substep count allows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClothParams {
    /// Edge stretch compliance in m/N per edge.
    pub stretch_compliance: f32,
    /// Dihedral bending compliance in 1/(N m) per interior edge (rad^2 / J).
    pub bend_compliance: f32,
    /// Long-range tethers: when `Some(s)`, every free particle stays within `s` times its
    /// rest geodesic distance of the nearest pinned particle (`s >= 1`). `None` disables.
    pub tether_slack: Option<f32>,
    /// Velocity damping rate in 1/s.
    pub damping: f32,
    /// Collision margin in meters: particles are kept this far above proxy surfaces.
    pub thickness: f32,
    /// Friction coefficient, averaged with the proxy's coefficient per contact.
    pub friction: f32,
    /// Aerodynamic drag coefficient (force along the relative air flow).
    pub drag: f32,
    /// Aerodynamic lift coefficient (force perpendicular to the flow).
    pub lift: f32,
}

impl Default for ClothParams {
    fn default() -> Self {
        Self {
            stretch_compliance: 0.0,
            bend_compliance: 1.0e3,
            tether_slack: None,
            damping: 0.5,
            thickness: 0.01,
            friction: 0.5,
            drag: 1.0,
            lift: 0.5,
        }
    }
}

impl ClothParams {
    pub fn validate(&self) -> Result<()> {
        let ok = in_range(self.stretch_compliance, 0.0, MAX_COMPLIANCE)
            && in_range(self.bend_compliance, 0.0, MAX_COMPLIANCE)
            && self
                .tether_slack
                .is_none_or(|s| in_range(s, 1.0, MAX_TETHER_SLACK))
            && in_range(self.damping, 0.0, MAX_DAMPING)
            && in_range(self.thickness, 0.0, MAX_EXTENT)
            && in_range(self.friction, 0.0, MAX_FRICTION)
            && in_range(self.drag, 0.0, MAX_AERO_COEFFICIENT)
            && in_range(self.lift, 0.0, MAX_AERO_COEFFICIENT);
        if ok {
            Ok(())
        } else {
            Err(Error("invalid cloth parameters"))
        }
    }
}

/// Everything needed to create a [`Cloth`]. Rest lengths and rest angles are taken from
/// `positions`.
#[derive(Clone, Debug, PartialEq)]
pub struct ClothDesc {
    pub positions: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
    /// Mass per area in kg/m^2, lumped to vertices (a third of each triangle's mass).
    /// Ignored when `inverse_masses` is set.
    pub areal_density: f32,
    /// Explicit per-particle inverse masses in 1/kg (0 = static particle).
    pub inverse_masses: Option<Vec<f32>>,
    /// Particles pinned at their rest position on creation.
    pub pinned: Vec<u32>,
    pub params: ClothParams,
}

impl ClothDesc {
    /// A rectangular cloth from [`grid_mesh`] with default material and no pins.
    pub fn grid(origin: Vec3, u: Vec3, v: Vec3, segments_u: u32, segments_v: u32) -> Result<Self> {
        let (positions, triangles) = grid_mesh(origin, u, v, segments_u, segments_v)?;
        Ok(Self {
            positions,
            triangles,
            areal_density: 0.2,
            inverse_masses: None,
            pinned: Vec::new(),
            params: ClothParams::default(),
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct Hinge {
    /// `[wing0, wing1, edge0, edge1]`.
    particles: [u32; 4],
    rest: f32,
}

#[derive(Clone, Copy, Debug)]
struct Tether {
    particle: u32,
    anchor: u32,
    rest: f32,
}

/// A simulated cloth. Create it through [`crate::XpbdWorld::insert_cloth`].
#[derive(Clone, Debug)]
pub struct Cloth {
    particles: Particles,
    triangles: Vec<[u32; 3]>,
    stretch: Vec<Distance>,
    stretch_lambda: Vec<f32>,
    hinges: Vec<Hinge>,
    hinge_lambda: Vec<f32>,
    tethers: Vec<Tether>,
    /// Compressed adjacency over stretch edges for geodesic tether distances.
    adjacency_offsets: Vec<u32>,
    adjacency: Vec<(u32, f32)>,
    forces: Vec<Vec3>,
    normals: Vec<Vec3>,
    params: ClothParams,
}

impl Cloth {
    pub(crate) fn new(desc: ClothDesc) -> Result<Self> {
        validate_triangles(&desc.positions, &desc.triangles)?;
        desc.params.validate()?;
        if desc.inverse_masses.is_none()
            && !in_range(desc.areal_density, f32::MIN_POSITIVE, MAX_AREAL_DENSITY)
        {
            return Err(Error("invalid cloth areal density"));
        }
        let mut masses = vec![0.0f32; desc.positions.len()];
        for &[a, b, c] in &desc.triangles {
            let [pa, pb, pc] = [a, b, c].map(|i| desc.positions[i as usize]);
            let third = desc.areal_density * 0.5 * (pb - pa).cross(pc - pa).length() / 3.0;
            for i in [a, b, c] {
                masses[i as usize] += third;
            }
        }
        let w = inverse_masses(desc.inverse_masses.as_deref(), &masses)?;
        let mut particles = Particles::new(desc.positions, w);
        apply_initial_pins(&mut particles, &desc.pinned)?;

        let edges = unique_edges(&desc.triangles);
        let stretch: Vec<Distance> = edges
            .iter()
            .map(|&(a, b)| Distance {
                a,
                b,
                rest: particles.x[a as usize].distance(particles.x[b as usize]),
            })
            .collect();
        let hinges: Vec<Hinge> = hinges(&desc.triangles)
            .into_iter()
            .filter_map(|particles_of| {
                let [p0, p1, p2, p3] = particles_of.map(|i| particles.x[i as usize]);
                dihedral(p0, p1, p2, p3).map(|(rest, _)| Hinge {
                    particles: particles_of,
                    rest,
                })
            })
            .collect();

        let n = particles.len();
        let mut degree = vec![0u32; n + 1];
        for constraint in &stretch {
            degree[constraint.a as usize + 1] += 1;
            degree[constraint.b as usize + 1] += 1;
        }
        for i in 0..n {
            degree[i + 1] += degree[i];
        }
        let mut fill = degree.clone();
        let mut adjacency = vec![(0u32, 0.0f32); stretch.len() * 2];
        for constraint in &stretch {
            for (from, to) in [(constraint.a, constraint.b), (constraint.b, constraint.a)] {
                let slot = &mut fill[from as usize];
                adjacency[*slot as usize] = (to, constraint.rest);
                *slot += 1;
            }
        }

        let mut cloth = Self {
            stretch_lambda: vec![0.0; stretch.len()],
            hinge_lambda: vec![0.0; hinges.len()],
            stretch,
            hinges,
            tethers: Vec::new(),
            adjacency_offsets: degree,
            adjacency,
            forces: vec![Vec3::ZERO; n],
            normals: vec![Vec3::Y; n],
            triangles: desc.triangles,
            particles,
            params: desc.params,
        };
        cloth.particles.take_pins_changed();
        cloth.rebuild_tethers();
        cloth.update_normals();
        Ok(cloth)
    }

    /// Current particle positions in meters.
    pub fn positions(&self) -> &[Vec3] {
        &self.particles.x
    }

    /// Current particle velocities in m/s.
    pub fn velocities(&self) -> &[Vec3] {
        &self.particles.v
    }

    /// Unit area-weighted vertex normals (`u x v` side of the triangle winding), updated
    /// after every step.
    pub fn normals(&self) -> &[Vec3] {
        &self.normals
    }

    pub fn triangles(&self) -> &[[u32; 3]] {
        &self.triangles
    }

    /// Authored inverse masses in 1/kg (pinning does not change them).
    pub fn inverse_masses(&self) -> &[f32] {
        self.particles.inverse_masses()
    }

    pub fn particle_count(&self) -> usize {
        self.particles.len()
    }

    /// Number of stretch, bending and tether constraints.
    pub fn constraint_counts(&self) -> (usize, usize, usize) {
        (self.stretch.len(), self.hinges.len(), self.tethers.len())
    }

    pub fn params(&self) -> ClothParams {
        self.params
    }

    pub fn set_params(&mut self, params: ClothParams) -> Result<()> {
        params.validate()?;
        let tethers_toggled = params.tether_slack.is_some() != self.params.tether_slack.is_some();
        self.params = params;
        if tethers_toggled {
            self.rebuild_tethers();
        }
        Ok(())
    }

    /// Pins `particle` (if needed) and sets the world-space position it reaches at the end
    /// of the next step. Call it before every step to follow a skinned bone.
    pub fn set_pin_target(&mut self, particle: usize, target: Vec3) -> Result<()> {
        self.particles.set_pin_target(particle, target)
    }

    /// Releases a pinned particle; it keeps the velocity of its last pinned motion.
    pub fn unpin(&mut self, particle: usize) -> Result<()> {
        self.particles.unpin(particle)
    }

    pub fn pin_target(&self, particle: usize) -> Option<Vec3> {
        self.particles.pin_target(particle)
    }

    pub fn set_inverse_mass(&mut self, particle: usize, inverse_mass: f32) -> Result<()> {
        self.particles.set_inverse_mass(particle, inverse_mass)
    }

    pub(crate) fn all_finite(&self) -> bool {
        self.particles.all_finite()
    }

    pub(crate) fn step(
        &mut self,
        ctx: &StepContext,
        proxies: &[Prepared],
        accum: &mut [ImpulseAccum],
    ) {
        if self.particles.take_pins_changed() {
            self.rebuild_tethers();
        }
        let h = ctx.substep();
        let stretch_alpha = self.params.stretch_compliance / (h * h);
        let bend_alpha = self.params.bend_compliance / (h * h);
        let contact = Contact {
            margin: self.params.thickness,
            friction: self.params.friction,
        };
        let aero = ctx.air_density > 0.0 && (self.params.drag > 0.0 || self.params.lift > 0.0);
        self.particles.begin_step();
        for substep in 0..ctx.substeps {
            if aero {
                self.accumulate_aero_forces(ctx.wind, ctx.air_density);
            }
            let forces = aero.then_some(self.forces.as_slice());
            self.particles
                .predict(h, ctx.gravity, forces, substep, ctx.substeps);
            self.stretch_lambda.fill(0.0);
            self.hinge_lambda.fill(0.0);
            for _ in 0..ctx.iterations {
                let Particles { x, w, .. } = &mut self.particles;
                solve_distances(x, w, &self.stretch, &mut self.stretch_lambda, stretch_alpha);
                if let Some(slack) = self.params.tether_slack {
                    solve_tethers(x, w, &self.tethers, slack);
                }
                solve_bending(x, w, &self.hinges, &mut self.hinge_lambda, bend_alpha);
            }
            self.particles.collide(proxies, accum, contact, h);
            self.particles.finish_substep(h, self.params.damping);
        }
        self.update_normals();
    }

    /// Per-triangle thin-plate aerodynamics against the uniform wind, lumped to vertices.
    fn accumulate_aero_forces(&mut self, wind: Vec3, air_density: f32) {
        self.forces.fill(Vec3::ZERO);
        let x = &self.particles.x;
        let v = &self.particles.v;
        for &[a, b, c] in &self.triangles {
            let (a, b, c) = (a as usize, b as usize, c as usize);
            let cross = (x[b] - x[a]).cross(x[c] - x[a]);
            let double_area = cross.length();
            if double_area <= 1.0e-12 {
                continue;
            }
            let normal = cross / double_area;
            let relative = wind - (v[a] + v[b] + v[c]) / 3.0;
            let speed = relative.length();
            if speed <= 1.0e-6 {
                continue;
            }
            let flow = relative / speed;
            let cos = normal.dot(flow);
            let facing = normal * cos.signum();
            let cos = cos.abs();
            // Dynamic pressure times area.
            let q = 0.25 * air_density * double_area * speed * speed;
            let mut force = flow * (self.params.drag * q * cos);
            // Lift acts along the plate normal's component perpendicular to the flow, with
            // magnitude proportional to cos * sin of the angle of attack.
            let perpendicular = facing - flow * cos;
            force += perpendicular * (self.params.lift * q * cos);
            let share = force / 3.0;
            for i in [a, b, c] {
                self.forces[i] += share;
            }
        }
    }

    fn update_normals(&mut self) {
        self.normals.fill(Vec3::ZERO);
        let x = &self.particles.x;
        for &[a, b, c] in &self.triangles {
            let (a, b, c) = (a as usize, b as usize, c as usize);
            let cross = (x[b] - x[a]).cross(x[c] - x[a]);
            for i in [a, b, c] {
                self.normals[i] += cross;
            }
        }
        for normal in &mut self.normals {
            *normal = normal.try_normalize().unwrap_or(Vec3::Y);
        }
    }

    /// Multi-source Dijkstra over rest edge lengths from every pinned particle; each free
    /// reachable particle gets one tether to its geodesically nearest pin.
    fn rebuild_tethers(&mut self) {
        self.tethers.clear();
        if self.params.tether_slack.is_none() {
            return;
        }
        let n = self.particles.len();
        let mut best = vec![(f32::INFINITY, u32::MAX); n];
        let mut heap = BinaryHeap::new();
        for (i, entry) in best.iter_mut().enumerate() {
            if self.particles.is_pinned(i) {
                *entry = (0.0, i as u32);
                heap.push(Reverse((0u32, i as u32)));
            }
        }
        while let Some(Reverse((distance_bits, node))) = heap.pop() {
            let distance = f32::from_bits(distance_bits);
            let (current, anchor) = best[node as usize];
            if distance > current {
                continue;
            }
            let start = self.adjacency_offsets[node as usize] as usize;
            let end = self.adjacency_offsets[node as usize + 1] as usize;
            for &(next, length) in &self.adjacency[start..end] {
                let candidate = distance + length;
                let slot = &mut best[next as usize];
                if candidate < slot.0 || (candidate == slot.0 && anchor < slot.1) {
                    *slot = (candidate, anchor);
                    heap.push(Reverse((candidate.to_bits(), next)));
                }
            }
        }
        for (i, &(rest, anchor)) in best.iter().enumerate() {
            if anchor != u32::MAX && anchor as usize != i && !self.particles.is_pinned(i) {
                self.tethers.push(Tether {
                    particle: i as u32,
                    anchor,
                    rest,
                });
            }
        }
    }
}

/// Unilateral inextensible tethers to pinned anchors (which have zero inverse mass).
fn solve_tethers(x: &mut [Vec3], w: &[f32], tethers: &[Tether], slack: f32) {
    for tether in tethers {
        let (p, anchor) = (tether.particle as usize, tether.anchor as usize);
        if w[p] <= 0.0 {
            continue;
        }
        let delta = x[p] - x[anchor];
        let length = delta.length();
        let limit = tether.rest * slack;
        if length > limit && length > 0.0 {
            x[p] = x[anchor] + delta * (limit / length);
        }
    }
}

fn solve_bending(
    x: &mut [Vec3],
    w: &[f32],
    hinges: &[Hinge],
    lambdas: &mut [f32],
    alpha_tilde: f32,
) {
    for (hinge, lambda) in hinges.iter().zip(lambdas.iter_mut()) {
        let ids = hinge.particles.map(|i| i as usize);
        let weights = ids.map(|i| w[i]);
        if weights.iter().all(|&wi| wi <= 0.0) {
            continue;
        }
        let Some((angle, gradients)) = dihedral(x[ids[0]], x[ids[1]], x[ids[2]], x[ids[3]]) else {
            continue;
        };
        let mut c = angle - hinge.rest;
        if c > PI {
            c -= TAU;
        } else if c < -PI {
            c += TAU;
        }
        let denominator: f32 = weights
            .iter()
            .zip(&gradients)
            .map(|(wi, g)| wi * g.length_squared())
            .sum::<f32>()
            + alpha_tilde;
        if denominator <= 1.0e-12 {
            continue;
        }
        let d_lambda = (-c - alpha_tilde * *lambda) / denominator;
        *lambda += d_lambda;
        for ((&i, &wi), g) in ids.iter().zip(&weights).zip(&gradients) {
            x[i] += *g * (wi * d_lambda);
        }
    }
}

/// Signed dihedral angle of the hinge with wings `x1`, `x2` and shared edge `x3 -> x4`
/// (zero when flat), with its gradient for each of the four particles (Bridson et al.
/// 2003 bending modes). `None` when the edge or a wing triangle is degenerate.
fn dihedral(x1: Vec3, x2: Vec3, x3: Vec3, x4: Vec3) -> Option<(f32, [Vec3; 4])> {
    let edge = x4 - x3;
    let edge_length = edge.length();
    let n1 = (x1 - x3).cross(x1 - x4);
    let n2 = (x2 - x4).cross(x2 - x3);
    let l1 = n1.length_squared();
    let l2 = n2.length_squared();
    if edge_length <= 1.0e-9 || l1 <= 1.0e-24 || l2 <= 1.0e-24 {
        return None;
    }
    let n1_hat = n1 / l1.sqrt();
    let n2_hat = n2 / l2.sqrt();
    let sin = n1_hat.cross(n2_hat).dot(edge / edge_length);
    let angle = sin.atan2(n1_hat.dot(n2_hat));
    let a1 = n1 / l1;
    let a2 = n2 / l2;
    let g1 = -a1 * edge_length;
    let g2 = -a2 * edge_length;
    let g3 = -(a1 * ((x1 - x4).dot(edge) / edge_length) + a2 * ((x2 - x4).dot(edge) / edge_length));
    let g4 = a1 * ((x1 - x3).dot(edge) / edge_length) + a2 * ((x2 - x3).dot(edge) / edge_length);
    Some((angle, [g1, g2, g3, g4]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dihedral_gradient_matches_finite_differences() {
        let base = [
            Vec3::new(0.3, 0.9, 0.2),
            Vec3::new(0.6, -0.8, -0.3),
            Vec3::new(0.0, 0.05, 0.0),
            Vec3::new(1.0, -0.1, 0.1),
        ];
        let (angle, gradients) = dihedral(base[0], base[1], base[2], base[3]).unwrap();
        assert!(angle.abs() > 0.1, "test hinge should be bent: {angle}");
        let eps = 1.0e-3;
        for particle in 0..4 {
            for axis in 0..3 {
                let mut plus = base;
                let mut minus = base;
                plus[particle][axis] += eps;
                minus[particle][axis] -= eps;
                let (a_plus, _) = dihedral(plus[0], plus[1], plus[2], plus[3]).unwrap();
                let (a_minus, _) = dihedral(minus[0], minus[1], minus[2], minus[3]).unwrap();
                let numeric = (a_plus - a_minus) / (2.0 * eps);
                let analytic = gradients[particle][axis];
                assert!(
                    (numeric - analytic).abs() < 2.0e-3,
                    "particle {particle} axis {axis}: numeric {numeric} analytic {analytic}"
                );
            }
        }
        let flat = dihedral(
            Vec3::new(0.5, 1.0, 0.0),
            Vec3::new(0.5, -1.0, 0.0),
            Vec3::ZERO,
            Vec3::X,
        )
        .unwrap();
        assert!(flat.0.abs() < 1e-6);
    }
}
