//! Tetrahedral soft bodies: edge-length and tetrahedron-volume XPBD constraints.
use glam::Vec3;

use crate::mesh::{boundary, signed_volume, unique_edges, validate_tets};
use crate::particles::{
    Contact, Distance, Particles, StepContext, apply_initial_pins, inverse_masses, solve_distances,
};
use crate::proxy::{ImpulseAccum, Prepared};
use crate::{Error, MAX_COMPLIANCE, MAX_DAMPING, MAX_EXTENT, MAX_FRICTION, Result, in_range};

/// Largest accepted density in kg/m^3.
const MAX_DENSITY: f32 = 1.0e5;

/// Runtime-tunable soft-body material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoftBodyParams {
    /// Edge-length compliance in m/N per edge (0 = as stiff as the substeps allow).
    pub edge_compliance: f32,
    /// Volume compliance in 1/Pa: the inverse bulk modulus. Each tetrahedron uses
    /// `volume_compliance * rest_volume`, so the response is mesh independent.
    pub volume_compliance: f32,
    /// Velocity damping rate in 1/s.
    pub damping: f32,
    /// Collision margin in meters: vertices are kept this far above proxy surfaces.
    pub particle_radius: f32,
    /// Friction coefficient, averaged with the proxy's coefficient per contact.
    pub friction: f32,
}

impl Default for SoftBodyParams {
    fn default() -> Self {
        Self {
            edge_compliance: 0.0,
            volume_compliance: 0.0,
            damping: 0.5,
            particle_radius: 0.01,
            friction: 0.5,
        }
    }
}

impl SoftBodyParams {
    pub fn validate(&self) -> Result<()> {
        let ok = in_range(self.edge_compliance, 0.0, MAX_COMPLIANCE)
            && in_range(self.volume_compliance, 0.0, MAX_COMPLIANCE)
            && in_range(self.damping, 0.0, MAX_DAMPING)
            && in_range(self.particle_radius, 0.0, MAX_EXTENT)
            && in_range(self.friction, 0.0, MAX_FRICTION);
        if ok {
            Ok(())
        } else {
            Err(Error("invalid soft-body parameters"))
        }
    }
}

/// Everything needed to create a [`SoftBody`]. Tetrahedra of either orientation are
/// accepted (negative ones are reoriented); rest lengths and volumes come from `positions`.
#[derive(Clone, Debug, PartialEq)]
pub struct SoftBodyDesc {
    pub positions: Vec<Vec3>,
    pub tets: Vec<[u32; 4]>,
    /// Density in kg/m^3, lumped to vertices (a quarter of each tetrahedron's mass).
    /// Ignored when `inverse_masses` is set.
    pub density: f32,
    /// Explicit per-particle inverse masses in 1/kg (0 = static particle).
    pub inverse_masses: Option<Vec<f32>>,
    /// Particles pinned at their rest position on creation.
    pub pinned: Vec<u32>,
    pub params: SoftBodyParams,
}

impl SoftBodyDesc {
    /// A box from [`crate::box_tet_mesh`] with default material and no pins.
    pub fn cuboid(min: Vec3, max: Vec3, segments: [u32; 3], density: f32) -> Result<Self> {
        let (positions, tets) = crate::box_tet_mesh(min, max, segments)?;
        Ok(Self {
            positions,
            tets,
            density,
            inverse_masses: None,
            pinned: Vec::new(),
            params: SoftBodyParams::default(),
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct Volume {
    particles: [u32; 4],
    rest: f32,
}

/// A simulated soft body. Create it through [`crate::XpbdWorld::insert_soft_body`].
#[derive(Clone, Debug)]
pub struct SoftBody {
    particles: Particles,
    tets: Vec<[u32; 4]>,
    surface: Vec<[u32; 3]>,
    edges: Vec<Distance>,
    edge_lambda: Vec<f32>,
    volumes: Vec<Volume>,
    volume_lambda: Vec<f32>,
    rest_volume: f32,
    params: SoftBodyParams,
}

impl SoftBody {
    pub(crate) fn new(desc: SoftBodyDesc) -> Result<Self> {
        let tets = validate_tets(&desc.positions, &desc.tets)?;
        desc.params.validate()?;
        if desc.inverse_masses.is_none() && !in_range(desc.density, f32::MIN_POSITIVE, MAX_DENSITY)
        {
            return Err(Error("invalid soft-body density"));
        }
        let mut masses = vec![0.0f32; desc.positions.len()];
        let volumes: Vec<Volume> = tets
            .iter()
            .map(|&tet| Volume {
                particles: tet,
                rest: signed_volume(&desc.positions, tet),
            })
            .collect();
        for volume in &volumes {
            for i in volume.particles {
                masses[i as usize] += desc.density * volume.rest / 4.0;
            }
        }
        let w = inverse_masses(desc.inverse_masses.as_deref(), &masses)?;
        let mut particles = Particles::new(desc.positions, w);
        apply_initial_pins(&mut particles, &desc.pinned)?;
        let edges: Vec<Distance> = unique_edges(&tets)
            .into_iter()
            .map(|(a, b)| Distance {
                a,
                b,
                rest: particles.x[a as usize].distance(particles.x[b as usize]),
            })
            .collect();
        Ok(Self {
            surface: boundary(&tets),
            edge_lambda: vec![0.0; edges.len()],
            volume_lambda: vec![0.0; volumes.len()],
            rest_volume: volumes.iter().map(|v| v.rest).sum(),
            edges,
            volumes,
            tets,
            particles,
            params: desc.params,
        })
    }

    /// Current particle positions in meters.
    pub fn positions(&self) -> &[Vec3] {
        &self.particles.x
    }

    /// Current particle velocities in m/s.
    pub fn velocities(&self) -> &[Vec3] {
        &self.particles.v
    }

    /// Positively oriented tetrahedra (as validated, possibly reoriented).
    pub fn tets(&self) -> &[[u32; 4]] {
        &self.tets
    }

    /// Outward-wound boundary triangles indexing [`Self::positions`], for rendering.
    pub fn surface_triangles(&self) -> &[[u32; 3]] {
        &self.surface
    }

    /// Authored inverse masses in 1/kg (pinning does not change them).
    pub fn inverse_masses(&self) -> &[f32] {
        self.particles.inverse_masses()
    }

    pub fn particle_count(&self) -> usize {
        self.particles.len()
    }

    /// Sum of rest tetrahedron volumes in m^3.
    pub fn rest_volume(&self) -> f32 {
        self.rest_volume
    }

    /// Sum of current signed tetrahedron volumes in m^3.
    pub fn volume(&self) -> f32 {
        self.tets
            .iter()
            .map(|&tet| signed_volume(&self.particles.x, tet))
            .sum()
    }

    pub fn params(&self) -> SoftBodyParams {
        self.params
    }

    pub fn set_params(&mut self, params: SoftBodyParams) -> Result<()> {
        params.validate()?;
        self.params = params;
        Ok(())
    }

    /// Pins `particle` (if needed) and sets the world-space position it reaches at the end
    /// of the next step.
    pub fn set_pin_target(&mut self, particle: usize, target: Vec3) -> Result<()> {
        self.particles.set_pin_target(particle, target)
    }

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
        let h = ctx.substep();
        let edge_alpha = self.params.edge_compliance / (h * h);
        let volume_alpha = self.params.volume_compliance / (h * h);
        let contact = Contact {
            margin: self.params.particle_radius,
            friction: self.params.friction,
        };
        self.particles.begin_step();
        for substep in 0..ctx.substeps {
            self.particles
                .predict(h, ctx.gravity, None, substep, ctx.substeps);
            self.edge_lambda.fill(0.0);
            self.volume_lambda.fill(0.0);
            for _ in 0..ctx.iterations {
                let Particles { x, w, .. } = &mut self.particles;
                solve_distances(x, w, &self.edges, &mut self.edge_lambda, edge_alpha);
                solve_volumes(x, w, &self.volumes, &mut self.volume_lambda, volume_alpha);
            }
            self.particles.collide(proxies, accum, contact, h);
            self.particles.finish_substep(h, self.params.damping);
        }
    }
}

/// One XPBD pass over `C = V - V0` per tetrahedron with compliance `alpha * V0 / h^2`.
fn solve_volumes(x: &mut [Vec3], w: &[f32], volumes: &[Volume], lambdas: &mut [f32], alpha: f32) {
    for (volume, lambda) in volumes.iter().zip(lambdas.iter_mut()) {
        let ids = volume.particles.map(|i| i as usize);
        let [p1, p2, p3, p4] = ids.map(|i| x[i]);
        let g2 = (p3 - p1).cross(p4 - p1) / 6.0;
        let g3 = (p4 - p1).cross(p2 - p1) / 6.0;
        let g4 = (p2 - p1).cross(p3 - p1) / 6.0;
        let g1 = -(g2 + g3 + g4);
        let gradients = [g1, g2, g3, g4];
        let c = (p2 - p1).dot(g2) - volume.rest;
        let alpha_tilde = alpha * volume.rest;
        let denominator: f32 = ids
            .iter()
            .zip(&gradients)
            .map(|(&i, g)| w[i] * g.length_squared())
            .sum::<f32>()
            + alpha_tilde;
        if denominator <= 1.0e-20 {
            continue;
        }
        let d_lambda = (-c - alpha_tilde * *lambda) / denominator;
        *lambda += d_lambda;
        for (&i, g) in ids.iter().zip(&gradients) {
            x[i] += *g * (w[i] * d_lambda);
        }
    }
}
