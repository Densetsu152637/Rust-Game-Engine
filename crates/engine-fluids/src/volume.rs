//! One fluid volume: particle storage, emitters, kill volumes and the PBF step.
//!
//! Per substep (`dt / substeps`): apply gravity and predict positions, build the neighbour
//! lists once, then for each iteration compute densities and lambdas, apply position
//! corrections and resolve proxy and domain collisions; finally derive velocities from the
//! position change, apply wall restitution, vorticity confinement, XSPH viscosity and the speed
//! clamp. After the substeps particles age, expire, are removed by kill volumes, and the
//! diagnostic pass computes end-of-step densities, colour-field normals and surface flags.

use crate::emitter::{
    EmitterDesc, EmitterState, Region, Spawn, validate_lifetime, validate_velocity,
};
use crate::grid::NeighborGrid;
use crate::kernel::Derived;
use crate::output::{DensityGrid, FluidStats, RenderOutput};
use crate::params::{Domain, FluidParams};
use crate::proxy::{self, ColliderProxy, PreparedProxy, ProxyImpulse, ProxyPose};
use crate::{
    Error, MAX_EMITTERS, MAX_GRID_SAMPLES, MAX_KILL_VOLUMES, MAX_NEIGHBORS, MAX_PARTICLES,
    MAX_STEP_SECONDS, Result,
};
use glam::Vec3;
use std::collections::BTreeMap;

/// Everything needed to create a volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeDesc {
    pub params: FluidParams,
    pub domain: Domain,
    /// Most particles the volume may hold, `1..=MAX_PARTICLES`.
    pub capacity: usize,
}

/// What happened during one step of a volume.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepReport {
    /// Particles created by emitters this step.
    pub spawned: usize,
    /// Particles emitters produced that did not fit into the capacity.
    pub dropped_at_capacity: usize,
    /// The volume holds `capacity` particles after emission.
    pub capacity_reached: bool,
    /// Particles removed by kill volumes.
    pub killed: usize,
    /// Particles removed because their lifetime ended.
    pub expired: usize,
    /// Particles alive after the step.
    pub particle_count: usize,
    /// Neighbour entries dropped because a particle exceeded `MAX_NEIGHBORS` (summed over
    /// substeps). Non-zero values mean extreme compression.
    pub neighbor_overflow: usize,
    /// Impulse applied by this volume to every supplied proxy, sorted by proxy id.
    pub proxy_impulses: Vec<ProxyImpulse>,
}

#[derive(Clone, Debug, Default)]
struct Particles {
    id: Vec<u64>,
    position: Vec<Vec3>,
    velocity: Vec<Vec3>,
    density: Vec<f32>,
    normal: Vec<Vec3>,
    surface: Vec<bool>,
    age: Vec<f32>,
    lifetime: Vec<f32>,
}

impl Particles {
    fn len(&self) -> usize {
        self.position.len()
    }

    fn push(&mut self, id: u64, position: Vec3, velocity: Vec3, lifetime: f32, rest: f32) {
        self.id.push(id);
        self.position.push(position);
        self.velocity.push(velocity);
        self.density.push(rest);
        self.normal.push(Vec3::ZERO);
        self.surface.push(false);
        self.age.push(0.0);
        self.lifetime.push(lifetime);
    }

    fn retain(&mut self, keep: &[bool]) {
        fn compact<T: Copy>(values: &mut Vec<T>, keep: &[bool]) {
            let mut next = 0;
            for i in 0..values.len() {
                if keep[i] {
                    values[next] = values[i];
                    next += 1;
                }
            }
            values.truncate(next);
        }
        compact(&mut self.id, keep);
        compact(&mut self.position, keep);
        compact(&mut self.velocity, keep);
        compact(&mut self.density, keep);
        compact(&mut self.normal, keep);
        compact(&mut self.surface, keep);
        compact(&mut self.age, keep);
        compact(&mut self.lifetime, keep);
    }

    fn clear(&mut self) {
        self.retain(&[]);
    }
}

#[derive(Clone, Debug, Default)]
struct Scratch {
    predicted: Vec<Vec3>,
    pre_velocity: Vec<Vec3>,
    lambda: Vec<f32>,
    delta: Vec<Vec3>,
    vorticity: Vec<Vec3>,
    walls: Vec<u8>,
    neighbor_start: Vec<u32>,
    neighbors: Vec<u32>,
    spawns: Vec<Spawn>,
    keep: Vec<bool>,
}

/// One particle fluid with its own parameters, domain and capacity.
#[derive(Clone, Debug)]
pub struct FluidVolume {
    params: FluidParams,
    derived: Derived,
    domain: Domain,
    capacity: usize,
    particles: Particles,
    scratch: Scratch,
    grid: NeighborGrid,
    grid_valid: bool,
    emitters: BTreeMap<u64, EmitterState>,
    kill_volumes: BTreeMap<u64, Region>,
    next_particle_id: u64,
    time: f64,
}

fn validate_capacity(capacity: usize) -> Result<()> {
    if (1..=MAX_PARTICLES).contains(&capacity) {
        Ok(())
    } else {
        Err(Error::Invalid("capacity must be within 1..=MAX_PARTICLES"))
    }
}

pub(crate) fn validate_dt(dt: f32) -> Result<()> {
    if dt.is_finite() && dt > 0.0 && dt <= MAX_STEP_SECONDS {
        Ok(())
    } else {
        Err(Error::Invalid(
            "step seconds must be finite and within (0, MAX_STEP_SECONDS]",
        ))
    }
}

impl FluidVolume {
    pub fn new(desc: VolumeDesc) -> Result<Self> {
        desc.params.validate()?;
        desc.domain.validate(&desc.params)?;
        validate_capacity(desc.capacity)?;
        Ok(Self {
            derived: Derived::new(&desc.params),
            params: desc.params,
            domain: desc.domain,
            capacity: desc.capacity,
            particles: Particles::default(),
            scratch: Scratch::default(),
            grid: NeighborGrid::default(),
            grid_valid: false,
            emitters: BTreeMap::new(),
            kill_volumes: BTreeMap::new(),
            next_particle_id: 0,
            time: 0.0,
        })
    }

    pub fn params(&self) -> &FluidParams {
        &self.params
    }

    /// Replace the parameters. Particles keep their state; mass, kernel and wall tables are
    /// recomputed. Rejected when the domain or an emitter becomes invalid for the new spacing.
    pub fn set_params(&mut self, params: FluidParams) -> Result<()> {
        params.validate()?;
        self.domain.validate(&params)?;
        for state in self.emitters.values() {
            state.desc.validate(params.spacing())?;
        }
        self.derived = Derived::new(&params);
        self.params = params;
        self.grid_valid = false;
        Ok(())
    }

    pub fn domain(&self) -> &Domain {
        &self.domain
    }

    /// Replace the domain. Particles outside it are pushed inside on the next step.
    pub fn set_domain(&mut self, domain: Domain) -> Result<()> {
        domain.validate(&self.params)?;
        self.domain = domain;
        Ok(())
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Change the capacity; it may not drop below the current particle count.
    pub fn set_capacity(&mut self, capacity: usize) -> Result<()> {
        validate_capacity(capacity)?;
        if capacity < self.len() {
            return Err(Error::Invalid(
                "capacity may not be smaller than the current particle count",
            ));
        }
        self.capacity = capacity;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.particles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Simulated seconds since creation.
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Spawn particles at `positions` (all inside the domain) with one velocity and optional
    /// lifetime. All-or-nothing: fails with [`Error::CapacityExceeded`] when they do not fit.
    pub fn spawn(
        &mut self,
        positions: &[Vec3],
        velocity: Vec3,
        lifetime: Option<f32>,
    ) -> Result<()> {
        validate_velocity(velocity)?;
        validate_lifetime(lifetime)?;
        if positions
            .iter()
            .any(|&p| !p.is_finite() || !self.domain.contains(p))
        {
            return Err(Error::Invalid(
                "spawn positions must be finite and inside the domain",
            ));
        }
        let available = self.capacity - self.len();
        if positions.len() > available {
            return Err(Error::CapacityExceeded {
                requested: positions.len(),
                available,
            });
        }
        let lifetime = lifetime.unwrap_or(f32::INFINITY);
        for &p in positions {
            let p = self.clamp_inside(p);
            self.particles.push(
                self.next_particle_id,
                p,
                velocity,
                lifetime,
                self.params.rest_density,
            );
            self.next_particle_id += 1;
        }
        self.grid_valid = false;
        Ok(())
    }

    /// Fill `region` (clipped to the domain) with a cubic lattice at the rest spacing and
    /// return the number of particles created. All-or-nothing like [`Self::spawn`].
    pub fn fill(&mut self, region: Region, velocity: Vec3, lifetime: Option<f32>) -> Result<usize> {
        region.validate()?;
        let points: Vec<Vec3> = region
            .lattice_points(self.derived.spacing)
            .into_iter()
            .filter(|&p| self.domain.contains(p))
            .collect();
        self.spawn(&points, velocity, lifetime)?;
        Ok(points.len())
    }

    /// Remove every particle.
    pub fn clear(&mut self) {
        self.particles.clear();
        self.grid_valid = false;
    }

    /// Insert or update an emitter. Updating keeps the emission accumulator so rates stay
    /// exact across edits; the random stream restarts only when the seed changes.
    pub fn set_emitter(&mut self, id: u64, desc: EmitterDesc) -> Result<()> {
        desc.validate(self.derived.spacing)?;
        if let Some(state) = self.emitters.get_mut(&id) {
            state.update(desc);
            return Ok(());
        }
        if self.emitters.len() >= MAX_EMITTERS {
            return Err(Error::Invalid("too many emitters (MAX_EMITTERS)"));
        }
        self.emitters.insert(id, EmitterState::new(desc));
        Ok(())
    }

    pub fn remove_emitter(&mut self, id: u64) -> bool {
        self.emitters.remove(&id).is_some()
    }

    pub fn emitter(&self, id: u64) -> Option<&EmitterDesc> {
        self.emitters.get(&id).map(|state| &state.desc)
    }

    /// Insert or replace a kill volume; particles inside it are removed after each step.
    pub fn set_kill_volume(&mut self, id: u64, region: Region) -> Result<()> {
        region.validate()?;
        if !self.kill_volumes.contains_key(&id) && self.kill_volumes.len() >= MAX_KILL_VOLUMES {
            return Err(Error::Invalid("too many kill volumes (MAX_KILL_VOLUMES)"));
        }
        self.kill_volumes.insert(id, region);
        Ok(())
    }

    pub fn remove_kill_volume(&mut self, id: u64) -> bool {
        self.kill_volumes.remove(&id).is_some()
    }

    /// Advance by `dt` seconds against the supplied proxies.
    pub fn step(&mut self, dt: f32, proxies: &[ColliderProxy]) -> Result<StepReport> {
        validate_dt(dt)?;
        let prepared = proxy::prepare(proxies)?;
        Ok(self.step_prepared(dt, &prepared))
    }

    pub(crate) fn step_prepared(&mut self, dt: f32, proxies: &[PreparedProxy]) -> StepReport {
        let mut report = StepReport::default();
        self.emit(dt, &mut report);
        let substeps = self.params.substeps;
        let h = dt / substeps as f32;
        let mut impulses = vec![(Vec3::ZERO, Vec3::ZERO); proxies.len()];
        let mut poses = Vec::with_capacity(proxies.len());
        for substep in 0..substeps {
            poses.clear();
            let t = h * (substep + 1) as f32;
            poses.extend(proxies.iter().map(|p| p.pose_at(t)));
            report.neighbor_overflow += self.substep(h, proxies, &poses, &mut impulses);
        }
        self.retire(dt, &mut report);
        self.diagnose();
        self.time += f64::from(dt);
        report.particle_count = self.len();
        report.proxy_impulses = proxies
            .iter()
            .zip(&impulses)
            .map(|(p, &(linear, angular))| ProxyImpulse {
                id: p.id,
                linear,
                angular,
            })
            .collect();
        report
    }

    fn clamp_inside(&self, p: Vec3) -> Vec3 {
        let r = Vec3::splat(self.derived.radius);
        p.clamp(self.domain.min + r, self.domain.max - r)
    }

    fn emit(&mut self, dt: f32, report: &mut StepReport) {
        let mut spawns = std::mem::take(&mut self.scratch.spawns);
        let lower = self.domain.min + Vec3::splat(self.derived.radius);
        let upper = self.domain.max - Vec3::splat(self.derived.radius);
        for state in self.emitters.values_mut() {
            spawns.clear();
            let room = self.capacity - self.particles.len();
            report.dropped_at_capacity += state.emit(dt, self.derived.spacing, room, &mut spawns);
            let lifetime = state.desc.lifetime.unwrap_or(f32::INFINITY);
            for spawn in &spawns {
                self.particles.push(
                    self.next_particle_id,
                    spawn.position.clamp(lower, upper),
                    spawn.velocity,
                    lifetime,
                    self.params.rest_density,
                );
                self.next_particle_id += 1;
            }
            report.spawned += spawns.len();
        }
        spawns.clear();
        self.scratch.spawns = spawns;
        report.capacity_reached = self.particles.len() >= self.capacity;
        if report.spawned > 0 {
            self.grid_valid = false;
        }
    }

    /// One PBF substep. Returns the neighbour overflow count.
    fn substep(
        &mut self,
        h: f32,
        proxies: &[PreparedProxy],
        poses: &[ProxyPose],
        impulses: &mut [(Vec3, Vec3)],
    ) -> usize {
        let n = self.particles.len();
        if n == 0 {
            return 0;
        }
        let gravity = self.params.gravity;
        let s = &mut self.scratch;
        let p = &mut self.particles;
        s.predicted.resize(n, Vec3::ZERO);
        s.pre_velocity.resize(n, Vec3::ZERO);
        s.lambda.resize(n, 0.0);
        s.delta.resize(n, Vec3::ZERO);
        s.walls.clear();
        s.walls.resize(n, 0);
        for i in 0..n {
            let v = p.velocity[i] + gravity * h;
            s.pre_velocity[i] = v;
            s.predicted[i] = p.position[i] + v * h;
        }
        let overflow = build_neighbors(
            &mut self.grid,
            &s.predicted,
            self.derived.kernel.h,
            &mut s.neighbor_start,
            &mut s.neighbors,
        );
        self.grid_valid = false;
        let collide = Collide {
            derived: &self.derived,
            domain: &self.domain,
            proxies,
            poses,
            h,
        };
        for _ in 0..self.params.iterations {
            solve_lambda(&self.derived, &self.domain, s, &mut p.density);
            solve_delta(&self.derived, &self.domain, s);
            for i in 0..n {
                let x = s.predicted[i] + s.delta[i];
                s.predicted[i] = collide.resolve(p.position[i], x, &mut s.walls[i], impulses);
            }
        }
        let inv_h = 1.0 / h;
        let restitution = self.domain.restitution;
        for i in 0..n {
            let mut v = (s.predicted[i] - p.position[i]) * inv_h;
            if s.walls[i] != 0 {
                v = restitute(v, s.pre_velocity[i], s.walls[i], restitution);
            }
            p.velocity[i] = v;
        }
        if self.params.vorticity > 0.0 {
            confine_vorticity(&self.derived, self.params.vorticity * h, s, &mut p.velocity);
        }
        if self.params.viscosity > 0.0 {
            apply_xsph(&self.derived, self.params.viscosity, s, &mut p.velocity);
        }
        let max_speed = self.params.max_speed;
        for v in &mut p.velocity {
            let speed2 = v.length_squared();
            if speed2 > max_speed * max_speed {
                *v *= max_speed / speed2.sqrt();
            }
        }
        std::mem::swap(&mut p.position, &mut s.predicted);
        overflow
    }

    /// Age particles and remove expired ones and those inside kill volumes.
    fn retire(&mut self, dt: f32, report: &mut StepReport) {
        let p = &mut self.particles;
        let keep = &mut self.scratch.keep;
        keep.clear();
        for i in 0..p.len() {
            p.age[i] += dt;
            let expired = p.age[i] >= p.lifetime[i];
            let killed = !expired
                && self
                    .kill_volumes
                    .values()
                    .any(|r| r.contains(p.position[i]));
            report.expired += usize::from(expired);
            report.killed += usize::from(killed);
            keep.push(!expired && !killed);
        }
        if report.expired + report.killed > 0 {
            p.retain(keep);
        }
    }

    /// End-of-step densities, colour-field normals and surface flags on final positions.
    fn diagnose(&mut self) {
        let d = &self.derived;
        let p = &mut self.particles;
        self.grid.build(&p.position, d.kernel.h);
        self.grid_valid = true;
        let threshold = self.params.surface_threshold;
        for i in 0..p.len() {
            let xi = p.position[i];
            let mut rho = d.self_density;
            let mut colour = Vec3::ZERO;
            self.grid.for_each_candidate(xi, |j| {
                let j = j as usize;
                if j == i {
                    return;
                }
                let r = xi - p.position[j];
                let r2 = r.length_squared();
                if r2 < d.kernel.h2 {
                    rho += d.mass * d.kernel.w(r2);
                    colour += d.kernel.grad(r, r2.sqrt()) * d.volume;
                }
            });
            let (wall_rho, wall_grad) = wall_terms(d, &self.domain, xi);
            rho += wall_rho;
            colour += wall_grad * d.inv_rest;
            let scaled = colour * d.kernel.h;
            let magnitude = scaled.length();
            p.density[i] = rho;
            p.normal[i] = if magnitude > 1.0e-6 {
                -scaled / magnitude
            } else {
                Vec3::ZERO
            };
            p.surface[i] = magnitude > threshold;
        }
    }

    pub fn ids(&self) -> &[u64] {
        &self.particles.id
    }

    pub fn positions(&self) -> &[Vec3] {
        &self.particles.position
    }

    pub fn velocities(&self) -> &[Vec3] {
        &self.particles.velocity
    }

    /// End-of-step densities in kg/m^3.
    pub fn densities(&self) -> &[f32] {
        &self.particles.density
    }

    /// Outward colour-field normals.
    pub fn normals(&self) -> &[Vec3] {
        &self.particles.normal
    }

    pub fn surface_flags(&self) -> &[bool] {
        &self.particles.surface
    }

    /// Seconds each particle has lived.
    pub fn ages(&self) -> &[f32] {
        &self.particles.age
    }

    /// Mass of one particle in kilograms.
    pub fn particle_mass(&self) -> f32 {
        self.derived.mass
    }

    /// Owned renderer data.
    pub fn render_output(&self) -> RenderOutput {
        let p = &self.particles;
        RenderOutput {
            ids: p.id.clone(),
            positions: p.position.clone(),
            velocities: p.velocity.clone(),
            densities: p.density.clone(),
            normals: p.normal.clone(),
            surface: p.surface.clone(),
            particle_radius: self.params.particle_radius,
            rest_density: self.params.rest_density,
        }
    }

    pub fn stats(&self) -> FluidStats {
        let p = &self.particles;
        let n = p.len();
        if n == 0 {
            return FluidStats::default();
        }
        let mut stats = FluidStats {
            particle_count: n,
            ..FluidStats::default()
        };
        let (mut sum, mut interior_sum, mut energy) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..n {
            let rho = p.density[i];
            sum += f64::from(rho);
            stats.max_density = stats.max_density.max(rho);
            if p.surface[i] {
                stats.surface_count += 1;
            } else {
                interior_sum += f64::from(rho);
            }
            let speed2 = p.velocity[i].length_squared();
            energy += 0.5 * f64::from(self.derived.mass) * f64::from(speed2);
            stats.max_speed = stats.max_speed.max(speed2.sqrt());
        }
        stats.mean_density = (sum / n as f64) as f32;
        let interior = n - stats.surface_count;
        if interior > 0 {
            stats.mean_interior_density = (interior_sum / interior as f64) as f32;
        }
        stats.kinetic_energy = energy as f32;
        stats
    }

    /// Sample normalized density on a `dims` grid spanning `min..=max` (wall boundary
    /// density excluded), for marching cubes.
    pub fn sample_density_grid(&self, min: Vec3, max: Vec3, dims: [u32; 3]) -> Result<DensityGrid> {
        if !min.is_finite() || !max.is_finite() || max.cmple(min).any() {
            return Err(Error::Invalid(
                "density grid bounds must be finite with max > min",
            ));
        }
        if dims.iter().any(|&n| n < 2) {
            return Err(Error::Invalid(
                "density grid needs at least 2 samples per axis",
            ));
        }
        let total = dims.iter().map(|&n| u64::from(n)).product::<u64>();
        if total > MAX_GRID_SAMPLES as u64 {
            return Err(Error::Invalid("density grid exceeds MAX_GRID_SAMPLES"));
        }
        let d = &self.derived;
        let rebuilt;
        let grid = if self.grid_valid {
            &self.grid
        } else {
            let mut grid = NeighborGrid::default();
            grid.build(&self.particles.position, d.kernel.h);
            rebuilt = grid;
            &rebuilt
        };
        let [nx, ny, nz] = dims;
        let cell_size = (max - min) / Vec3::new((nx - 1) as f32, (ny - 1) as f32, (nz - 1) as f32);
        let positions = &self.particles.position;
        let mut values = Vec::with_capacity(total as usize);
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let point = min + Vec3::new(x as f32, y as f32, z as f32) * cell_size;
                    let mut rho = 0.0;
                    grid.for_each_candidate(point, |j| {
                        rho += d.mass * d.kernel.w(point.distance_squared(positions[j as usize]));
                    });
                    values.push(rho * d.inv_rest);
                }
            }
        }
        Ok(DensityGrid {
            origin: min,
            cell_size,
            dims,
            values,
        })
    }
}

/// Build CSR neighbour lists on `positions`; returns the number of dropped entries.
fn build_neighbors(
    grid: &mut NeighborGrid,
    positions: &[Vec3],
    h: f32,
    start: &mut Vec<u32>,
    list: &mut Vec<u32>,
) -> usize {
    grid.build(positions, h);
    let h2 = h * h;
    start.clear();
    list.clear();
    start.push(0);
    let mut overflow = 0;
    for (i, &xi) in positions.iter().enumerate() {
        let begin = list.len();
        grid.for_each_candidate(xi, |j| {
            if j as usize != i && positions[j as usize].distance_squared(xi) < h2 {
                list.push(j);
            }
        });
        let count = list.len() - begin;
        if count > MAX_NEIGHBORS {
            overflow += count - MAX_NEIGHBORS;
            list.truncate(begin + MAX_NEIGHBORS);
        }
        start.push(list.len() as u32);
    }
    overflow
}

fn neighbors_of(s: &Scratch, i: usize) -> &[u32] {
    &s.neighbors[s.neighbor_start[i] as usize..s.neighbor_start[i + 1] as usize]
}

/// Wall boundary density at `x` and its gradient with respect to `x`.
#[inline]
fn wall_terms(d: &Derived, domain: &Domain, x: Vec3) -> (f32, Vec3) {
    let mut rho = 0.0;
    let mut grad = Vec3::ZERO;
    for axis in 0..3 {
        let (w, dw) = d.wall(x[axis] - domain.min[axis]);
        rho += w;
        grad[axis] += dw;
        let (w, dw) = d.wall(domain.max[axis] - x[axis]);
        rho += w;
        grad[axis] -= dw;
    }
    (rho, grad)
}

/// Densities and lambdas for the unilateral density constraint `max(rho / rho0 - 1, 0)`.
fn solve_lambda(d: &Derived, domain: &Domain, s: &mut Scratch, density: &mut [f32]) {
    let k = &d.kernel;
    for (i, density) in density.iter_mut().enumerate() {
        let xi = s.predicted[i];
        let mut rho = d.self_density;
        let mut grad_i = Vec3::ZERO;
        let mut grad_sq = 0.0;
        for &j in neighbors_of(s, i) {
            let r = xi - s.predicted[j as usize];
            let r2 = r.length_squared();
            if r2 >= k.h2 {
                continue;
            }
            rho += d.mass * k.w(r2);
            let g = k.grad(r, r2.sqrt()) * d.volume;
            grad_i += g;
            grad_sq += g.length_squared();
        }
        let (wall_rho, wall_grad) = wall_terms(d, domain, xi);
        rho += wall_rho;
        grad_i += wall_grad * d.inv_rest;
        *density = rho;
        let c = (rho * d.inv_rest - 1.0).max(0.0);
        s.lambda[i] = -c / (grad_i.length_squared() + grad_sq + d.epsilon);
    }
}

/// Position corrections from lambdas, tensile correction and wall boundary terms.
fn solve_delta(d: &Derived, domain: &Domain, s: &mut Scratch) {
    let k = &d.kernel;
    for i in 0..s.predicted.len() {
        let xi = s.predicted[i];
        let li = s.lambda[i];
        let mut dp = Vec3::ZERO;
        for &j in neighbors_of(s, i) {
            let j = j as usize;
            let r = xi - s.predicted[j];
            let r2 = r.length_squared();
            if r2 >= k.h2 {
                continue;
            }
            let mut pair = li + s.lambda[j];
            if d.tensile_scale > 0.0 {
                let ratio = k.w(r2) * d.inv_w_dq;
                let ratio2 = ratio * ratio;
                pair -= d.tensile_scale * ratio2 * ratio2;
            }
            dp += k.grad(r, r2.sqrt()) * pair;
        }
        dp *= d.volume;
        if li != 0.0 {
            let (_, wall_grad) = wall_terms(d, domain, xi);
            dp += wall_grad * (li * d.inv_rest);
        }
        s.delta[i] = dp;
    }
}

/// Proxy and domain collision for one particle position.
struct Collide<'a> {
    derived: &'a Derived,
    domain: &'a Domain,
    proxies: &'a [PreparedProxy],
    poses: &'a [ProxyPose],
    h: f32,
}

impl Collide<'_> {
    /// Push `x` out of proxies and into the domain with Coulomb position friction relative to
    /// the substep start `x0`. Proxy corrections are accumulated as impulses on the proxies.
    #[inline]
    fn resolve(
        &self,
        x0: Vec3,
        mut x: Vec3,
        walls: &mut u8,
        impulses: &mut [(Vec3, Vec3)],
    ) -> Vec3 {
        let radius = self.derived.radius;
        let impulse_scale = self.derived.mass / self.h;
        for (k, proxy) in self.proxies.iter().enumerate() {
            let pose = &self.poses[k];
            let Some(contact) = proxy.contact(pose, x, radius) else {
                continue;
            };
            let before = x;
            let depth = radius - contact.distance;
            x += contact.normal * depth;
            if proxy.friction > 0.0 {
                let surface = proxy.surface_velocity(pose, x) * self.h;
                let relative = (x - x0) - surface;
                let tangential = relative - contact.normal * relative.dot(contact.normal);
                x -= friction_correction(tangential, proxy.friction * depth);
            }
            let impulse = (before - x) * impulse_scale;
            impulses[k].0 += impulse;
            if !proxy.is_plane() {
                let point = x - contact.normal * radius;
                impulses[k].1 += (point - pose.position).cross(impulse);
            }
        }
        let lower = self.domain.min + Vec3::splat(radius);
        let upper = self.domain.max - Vec3::splat(radius);
        let friction = self.domain.friction;
        for axis in 0..3 {
            let depth = if x[axis] < lower[axis] {
                *walls |= 1 << (2 * axis);
                lower[axis] - x[axis]
            } else if x[axis] > upper[axis] {
                *walls |= 1 << (2 * axis + 1);
                x[axis] - upper[axis]
            } else {
                continue;
            };
            x[axis] = x[axis].clamp(lower[axis], upper[axis]);
            if friction > 0.0 {
                let mut tangential = x - x0;
                tangential[axis] = 0.0;
                x -= friction_correction(tangential, friction * depth);
            }
        }
        x
    }
}

/// Coulomb position friction: cancel the tangential displacement up to `limit`.
#[inline]
fn friction_correction(tangential: Vec3, limit: f32) -> Vec3 {
    let length = tangential.length();
    if length <= 1.0e-12 {
        return Vec3::ZERO;
    }
    tangential * (limit / length).min(1.0)
}

/// Restitution against touched domain walls (bit `2a` = min wall, `2a + 1` = max wall).
fn restitute(mut v: Vec3, incoming: Vec3, walls: u8, restitution: f32) -> Vec3 {
    for axis in 0..3 {
        if walls & (1 << (2 * axis)) != 0 {
            let target = restitution * (-incoming[axis]).max(0.0);
            v[axis] = v[axis].max(target);
        }
        if walls & (1 << (2 * axis + 1)) != 0 {
            let target = restitution * incoming[axis].max(0.0);
            v[axis] = v[axis].min(-target);
        }
    }
    v
}

/// Vorticity confinement (Fedkiw et al. 2001 as used by PBF): `v += strength_dt * N x w`.
fn confine_vorticity(d: &Derived, strength_dt: f32, s: &mut Scratch, velocity: &mut [Vec3]) {
    let k = &d.kernel;
    let n = velocity.len();
    s.vorticity.resize(n, Vec3::ZERO);
    for i in 0..n {
        let xi = s.predicted[i];
        let mut w = Vec3::ZERO;
        for &j in neighbors_of(s, i) {
            let j = j as usize;
            let r = xi - s.predicted[j];
            let len = r.length();
            w += k.grad(r, len).cross(velocity[j] - velocity[i]);
        }
        s.vorticity[i] = w * d.volume;
    }
    for i in 0..n {
        let xi = s.predicted[i];
        let wi = s.vorticity[i].length();
        let mut eta = Vec3::ZERO;
        for &j in neighbors_of(s, i) {
            let j = j as usize;
            let r = xi - s.predicted[j];
            eta += k.grad(r, r.length()) * (s.vorticity[j].length() - wi);
        }
        let length = eta.length();
        s.delta[i] = if length > 1.0e-9 {
            (eta / length).cross(s.vorticity[i]) * strength_dt
        } else {
            Vec3::ZERO
        };
    }
    for (v, dv) in velocity.iter_mut().zip(&s.delta) {
        *v += *dv;
    }
}

/// Symmetric (momentum-conserving) XSPH viscosity: `v_i += c * sum V W_ij (v_j - v_i)`.
fn apply_xsph(d: &Derived, c: f32, s: &mut Scratch, velocity: &mut [Vec3]) {
    let k = &d.kernel;
    for i in 0..velocity.len() {
        let xi = s.predicted[i];
        let mut dv = Vec3::ZERO;
        for &j in neighbors_of(s, i) {
            let j = j as usize;
            let w = k.w(xi.distance_squared(s.predicted[j]));
            dv += (velocity[j] - velocity[i]) * w;
        }
        s.delta[i] = dv * (c * d.volume);
    }
    for (v, dv) in velocity.iter_mut().zip(&s.delta) {
        *v += *dv;
    }
}
