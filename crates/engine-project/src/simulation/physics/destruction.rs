//! Authored destruction (`engine.Destructible3D`): Voronoi pre-fracture of a rigid body when
//! contact load crosses a threshold, with debris spawning, pooling, sleeping and expiry.
//!
//! The body (a box, a convex hull, or any body with an authored closed `mesh`) is fractured
//! with `engine-destruction`. On fracture the intact body is removed from the simulation and
//! one dynamic convex-hull body per fragment is spawned as an owned entity; see
//! `docs/physics-destruction.md`. Runtime state lives in the [`DestructionState`] resource so
//! the native API and the feature hooks share it.
use super::{Bodies, Worlds};
use super::{BodyMaterial, Context, Feature, Motion, PhysicsBody3D, Resolved, Shape, emit};
use crate::runtime::{AuthoredId, EntityLookup, OwnedBy};
use crate::simulation::{Services, invalid};
use crate::{Result, Transform};
use bevy_ecs::prelude::*;
use engine_destruction::{Fracture, Limits, Mesh, Pattern};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

/// Most fragments one fracture may create.
pub const MAX_FRAGMENTS: u32 = 256;
/// Most debris bodies one pool may hold.
pub const MAX_POOL: u32 = 4096;
/// Seconds a fragment must stay below the sleep velocity before it is put to sleep.
pub const SLEEP_GRACE: f32 = 0.25;
/// Smallest fragment volume (m^3) engine-physics accepts as a dynamic convex hull.
const MIN_FRAGMENT_VOLUME: f32 = 1e-9;
const MAX_MESH_VERTICES: usize = 4096;
const MAX_MESH_TRIANGLES: usize = 8192;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FracturePattern {
    /// Seeds uniformly inside the solid.
    #[default]
    Uniform,
    /// Seeds clustered around the impact point, so fragments are smallest there.
    Impact,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdMode {
    /// Largest contact impulse (N*s) applied in one physics substep.
    #[default]
    Impulse,
    /// Largest contact force (N): that impulse divided by the substep length.
    Force,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecyclePolicy {
    /// Recycle the longest-lived debris first.
    #[default]
    Oldest,
    /// Recycle the smallest (by volume) debris first, oldest among equals.
    Smallest,
}
/// Closed triangle mesh in body-local, unscaled coordinates (outward counter-clockwise).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DestructibleMesh {
    pub vertices: Vec<[f32; 3]>,
    pub indices: Vec<[u32; 3]>,
}
/// How fragments behave after a fracture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DebrisSettings {
    /// Seconds a fragment lives; 0 keeps it until recycled.
    pub lifetime: f32,
    /// Linear (m/s) and angular (rad/s) speed below which a fragment is forced to sleep after
    /// [`SLEEP_GRACE`] seconds; 0 leaves sleeping to Rapier.
    pub sleep_velocity: f32,
    /// Most fragments alive in `pool` at once; the excess is recycled per `recycle`.
    pub max_active: u32,
    pub recycle: RecyclePolicy,
    /// Debris sharing a pool name share its `max_active` cap.
    pub pool: String,
    /// Fraction (0..=1) of the body's linear and angular velocity fragments inherit.
    pub inherit_velocity: f32,
    /// Outward speed (m/s) added to each fragment; the mass-weighted mean is removed so the
    /// scatter adds no net linear momentum.
    pub scatter_speed: f32,
    /// Fragment density in kg/m^3; omitted keeps the body's total mass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<f32>,
}
impl Default for DebrisSettings {
    fn default() -> Self {
        Self {
            lifetime: 10.,
            sleep_velocity: 0.1,
            max_active: 128,
            recycle: RecyclePolicy::Oldest,
            pool: "default".into(),
            inherit_velocity: 1.,
            scatter_speed: 1.,
            density: None,
        }
    }
}
/// Authored destructible 3D body. Requires `engine.PhysicsBody3D` (box or convex hull, or an
/// authored closed `mesh`). `threshold` and `health` decide when it fractures.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Destructible3D {
    pub pattern: FracturePattern,
    /// Seeds, and so Voronoi pieces, 2..=256.
    pub fragment_count: u32,
    pub seed: u64,
    /// Body-local (unscaled) impact point for the `impact` pattern when no contact point is
    /// known (pre-fracture, or no contact data).
    pub impact_point: [f32; 3],
    /// Radius in meters of the impact seed cluster.
    pub impact_radius: f32,
    /// Closed mesh to fracture instead of the body shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh: Option<DestructibleMesh>,
    /// Contact load that fractures the body, per `threshold_mode`.
    pub threshold: f32,
    pub threshold_mode: ThresholdMode,
    /// Accumulated damage that fractures the body; omitted disables damage accumulation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<f32>,
    /// Per-substep contact impulse (N*s) below which no damage accumulates (resting contact).
    pub damage_floor: f32,
    /// Compute and cache the fragments at scene load instead of at fracture time.
    pub prefracture: bool,
    pub debris: DebrisSettings,
}
impl Default for Destructible3D {
    fn default() -> Self {
        Self {
            pattern: FracturePattern::Uniform,
            fragment_count: 12,
            seed: 1,
            impact_point: [0.; 3],
            impact_radius: 0.5,
            mesh: None,
            threshold: 50.,
            threshold_mode: ThresholdMode::Impulse,
            health: None,
            damage_floor: 1.,
            prefracture: false,
            debris: DebrisSettings::default(),
        }
    }
}
fn finite_in(value: f32, min: f32, max: f32) -> bool {
    value.is_finite() && (min..=max).contains(&value)
}
impl Destructible3D {
    /// Bounds for every authored value.
    pub fn check(&self) -> Result<()> {
        let bad = |message: &str| Err(invalid(format!("engine.Destructible3D: {message}")));
        if !(2..=MAX_FRAGMENTS).contains(&self.fragment_count) {
            return bad("fragment_count must be 2..=256");
        }
        if self.impact_point.iter().any(|v| !finite_in(*v, -1e6, 1e6))
            || !finite_in(self.impact_radius, 1e-4, 1e6)
        {
            return bad("impact_point must be finite and impact_radius in 0.0001..=1e6");
        }
        if !finite_in(self.threshold, 1e-6, 1e9) {
            return bad("threshold must be in 0.000001..=1e9");
        }
        if self.health.is_some_and(|h| !finite_in(h, 1e-6, 1e12))
            || !finite_in(self.damage_floor, 0., 1e9)
        {
            return bad("health must be in 0.000001..=1e12 and damage_floor in 0..=1e9");
        }
        let d = &self.debris;
        if !finite_in(d.lifetime, 0., 86_400.)
            || !finite_in(d.sleep_velocity, 0., 100.)
            || !finite_in(d.inherit_velocity, 0., 1.)
            || !finite_in(d.scatter_speed, 0., 1000.)
            || d.density.is_some_and(|v| !finite_in(v, 0.000001, 10_000.))
        {
            return bad(
                "debris lifetime, sleep_velocity, inherit_velocity, scatter_speed or density out of range",
            );
        }
        if !(1..=MAX_POOL).contains(&d.max_active) || d.max_active < self.fragment_count {
            return bad("debris.max_active must be fragment_count..=4096");
        }
        if d.pool.is_empty() || d.pool.len() > 64 {
            return bad("debris.pool must be 1..=64 characters");
        }
        if let Some(mesh) = &self.mesh
            && (mesh.vertices.len() < 4
                || mesh.vertices.len() > MAX_MESH_VERTICES
                || mesh.indices.len() < 4
                || mesh.indices.len() > MAX_MESH_TRIANGLES
                || mesh
                    .vertices
                    .iter()
                    .flatten()
                    .any(|v| !finite_in(*v, -1e6, 1e6))
                || mesh
                    .indices
                    .iter()
                    .flatten()
                    .any(|&i| i as usize >= mesh.vertices.len()))
        {
            return bad("mesh needs 4..=4096 finite vertices and 4..=8192 in-range triangles");
        }
        Ok(())
    }
}
/// Runtime marker on a destructible entity that has fractured. Its `PhysicsBody3D` was
/// removed; renderers should stop drawing the intact mesh.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct Fractured {
    pub fragments: u32,
    pub impulse: f32,
    pub point: Vec3,
}
/// Runtime marker on a spawned fragment entity.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebrisFragment {
    pub owner: Entity,
    pub index: u32,
}
/// Render mesh of a fragment, relative to the fragment entity's origin (its centroid).
/// Triangles flagged in `interior` are fracture surface.
#[derive(Clone, Debug, PartialEq)]
pub struct FragmentRender {
    pub positions: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
    pub interior: Vec<bool>,
}
/// One live fragment of a fractured entity.
#[derive(Clone, Debug, PartialEq)]
pub struct FragmentInfo {
    pub entity: Entity,
    pub index: u32,
    pub volume: f32,
    pub age: f32,
}
/// What a fracture did.
#[derive(Clone, Debug, PartialEq)]
pub struct FractureReport {
    pub fragments: Vec<Entity>,
    pub impulse: f32,
    pub point: Vec3,
}
/// Damage and fracture state of one destructible.
#[derive(Clone, Debug, PartialEq)]
pub struct DestructibleStatus {
    pub fractured: bool,
    pub damage: f32,
    pub health: Option<f32>,
    pub threshold: f32,
    /// Fragments are cached from a pre-fracture.
    pub prefractured: bool,
    pub live_fragments: usize,
}

struct Cached {
    config: Destructible3D,
    body: PhysicsBody3D,
    scale: Vec3,
    fracture: Arc<Fracture>,
}
#[derive(Default)]
struct Pending {
    peak_impulse: f32,
    peak_force: f32,
    point: Option<Vec3>,
    damage: f32,
}
struct Debris {
    owner: Entity,
    index: u32,
    id: String,
    pool: String,
    max_active: u32,
    recycle: RecyclePolicy,
    sequence: u64,
    volume: f32,
    age: f32,
    lifetime: f32,
    sleep_velocity: f32,
    slow_for: f32,
}
/// Shared runtime state of the destruction feature and its native API.
#[derive(Resource, Default)]
pub(crate) struct DestructionState {
    pending: BTreeMap<Entity, Pending>,
    damage: BTreeMap<Entity, f32>,
    debris: BTreeMap<Entity, Debris>,
    meshes: BTreeMap<Entity, Arc<FragmentRender>>,
    cache: Mutex<BTreeMap<Entity, Cached>>,
    sequence: u64,
    tick_time: f32,
}
struct Trigger {
    point: Option<Vec3>,
    impulse: f32,
    force: f32,
    cause: &'static str,
}
struct Event {
    name: &'static str,
    target: Entity,
    payload: Value,
}
struct Parts<'a> {
    worlds: &'a mut Worlds,
    bodies: &'a Bodies,
    settings: &'a Resolved,
}

fn convert(error: engine_destruction::Error) -> crate::Error {
    invalid(format!("engine.Destructible3D fracture failed: {error}"))
}
fn source_mesh(config: &Destructible3D, body: &PhysicsBody3D, scale: Vec3) -> Result<Mesh> {
    if let Some(mesh) = &config.mesh {
        let positions = mesh
            .vertices
            .iter()
            .map(|v| Vec3::from(*v) * scale)
            .collect();
        return Ok(Mesh::new(positions, mesh.indices.clone()));
    }
    match body.shape {
        Shape::Box => Ok(Mesh::cuboid(Vec3::from(body.half_extents) * scale)),
        Shape::ConvexHull => {
            let points: Vec<Vec3> = body.points.iter().map(|p| Vec3::from(*p) * scale).collect();
            Mesh::from_convex_points(&points).map_err(convert)
        }
        _ => Err(invalid(
            "engine.Destructible3D needs a box or convex_hull body, or an authored mesh",
        )),
    }
}
fn compute(
    config: &Destructible3D,
    body: &PhysicsBody3D,
    scale: Vec3,
    impact: Option<Vec3>,
) -> Result<Fracture> {
    let mesh = source_mesh(config, body, scale)?;
    let count = config.fragment_count as usize;
    let pattern = match config.pattern {
        FracturePattern::Uniform => Pattern::Uniform {
            count,
            seed: config.seed,
        },
        FracturePattern::Impact => Pattern::Impact {
            count,
            seed: config.seed,
            point: impact.unwrap_or(Vec3::from(config.impact_point) * scale),
            radius: config.impact_radius,
        },
    };
    engine_destruction::fracture(&mesh, &pattern, &Limits::default()).map_err(convert)
}
fn combine(rule: engine_physics::CombineRule) -> super::CombineRule {
    match rule {
        engine_physics::CombineRule::Average => super::CombineRule::Average,
        engine_physics::CombineRule::Min => super::CombineRule::Min,
        engine_physics::CombineRule::Max => super::CombineRule::Max,
        engine_physics::CombineRule::Multiply => super::CombineRule::Multiply,
    }
}
impl DestructionState {
    fn cache(&self) -> std::sync::MutexGuard<'_, BTreeMap<Entity, Cached>> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn fresh(
        &self,
        entity: Entity,
        config: &Destructible3D,
        body: &PhysicsBody3D,
        scale: Vec3,
    ) -> bool {
        self.cache()
            .get(&entity)
            .is_some_and(|c| c.config == *config && c.body == *body && c.scale == scale)
    }
    fn debris_in(&self, pool: &str) -> usize {
        self.debris.values().filter(|d| d.pool == pool).count()
    }
    /// Removes `entity` from the world and every table; reports the expiry.
    fn retire(
        &mut self,
        world: &mut World,
        entity: Entity,
        reason: &'static str,
        events: &mut Vec<Event>,
    ) {
        let Some(debris) = self.debris.remove(&entity) else {
            return;
        };
        self.meshes.remove(&entity);
        if world.get_entity(debris.owner).is_ok() {
            events.push(Event {
                name: "debris_expired",
                target: debris.owner,
                payload: json!({
                    "fragment": debris.id,
                    "index": debris.index,
                    "reason": reason,
                    "age": debris.age,
                    "volume": debris.volume,
                    "pool": debris.pool,
                }),
            });
        }
        if let Some(mut lookup) = world.get_resource_mut::<EntityLookup>() {
            lookup.0.remove(&debris.id);
        }
        world.despawn(entity);
    }
    /// Recycles the excess of `pool` per the newest fracture's cap and policy.
    fn enforce_pool(&mut self, world: &mut World, pool: &str, events: &mut Vec<Event>) {
        let mut members: Vec<(Entity, u64, f32)> = self
            .debris
            .iter()
            .filter(|(_, d)| d.pool == pool)
            .map(|(e, d)| (*e, d.sequence, d.volume))
            .collect();
        let Some(newest) = self
            .debris
            .values()
            .filter(|d| d.pool == pool)
            .max_by_key(|d| d.sequence)
        else {
            return;
        };
        let (cap, policy) = (newest.max_active as usize, newest.recycle);
        if members.len() <= cap {
            return;
        }
        match policy {
            RecyclePolicy::Oldest => members.sort_by_key(|m| m.1),
            RecyclePolicy::Smallest => {
                members.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.1.cmp(&b.1)));
            }
        }
        let excess = members.len() - cap;
        for (entity, _, _) in members.into_iter().take(excess) {
            self.retire(world, entity, "recycled", events);
        }
    }
    /// Advances debris ages, expires, sleeps and recycles. Returns nothing; events are queued.
    fn tick_debris(&mut self, worlds: &mut Worlds, world: &mut World, events: &mut Vec<Event>) {
        let dt = std::mem::take(&mut self.tick_time);
        let dead: Vec<Entity> = self
            .debris
            .keys()
            .filter(|e| world.get_entity(**e).is_err())
            .copied()
            .collect();
        for entity in dead {
            self.debris.remove(&entity);
            self.meshes.remove(&entity);
        }
        let mut expired = Vec::new();
        for (entity, debris) in &mut self.debris {
            debris.age += dt;
            if debris.lifetime > 0. && debris.age >= debris.lifetime {
                expired.push(*entity);
                continue;
            }
            let id = entity.to_bits();
            if debris.sleep_velocity > 0.
                && let Some(velocity) = worlds.three.velocity(id)
            {
                let slow = Vec3::from(velocity.linear).length() <= debris.sleep_velocity
                    && Vec3::from(velocity.angular).length() <= debris.sleep_velocity;
                if slow {
                    debris.slow_for += dt;
                    if debris.slow_for >= SLEEP_GRACE && worlds.three.is_sleeping(id) == Ok(false) {
                        // Only dynamic bodies can sleep; anything else is left alone.
                        let _ = worlds.three.sleep(id);
                    }
                } else {
                    debris.slow_for = 0.;
                }
            }
        }
        for entity in expired {
            self.retire(world, entity, "lifetime", events);
        }
        let pools: BTreeSet<String> = self.debris.values().map(|d| d.pool.clone()).collect();
        for pool in pools {
            self.enforce_pool(world, &pool, events);
        }
    }
    /// Pending contact load that crossed a threshold or exhausted health.
    fn collect_triggers(&mut self, world: &World) -> Vec<(Entity, Trigger)> {
        let pending = std::mem::take(&mut self.pending);
        let mut triggers = Vec::new();
        for (entity, load) in pending {
            let Some(config) = world.get::<Destructible3D>(entity) else {
                continue;
            };
            if world.get::<Fractured>(entity).is_some() {
                continue;
            }
            let damage = self.damage.entry(entity).or_default();
            *damage += load.damage;
            let measure = match config.threshold_mode {
                ThresholdMode::Impulse => load.peak_impulse,
                ThresholdMode::Force => load.peak_force,
            };
            let cause = if measure >= config.threshold {
                "threshold"
            } else if config.health.is_some_and(|h| *damage >= h) {
                "damage"
            } else {
                continue;
            };
            triggers.push((
                entity,
                Trigger {
                    point: load.point,
                    impulse: load.peak_impulse,
                    force: load.peak_force,
                    cause,
                },
            ));
        }
        triggers
    }
}

/// Fractures `entity` now: removes its body, spawns the fragments and queues the event.
fn execute(
    parts: Parts<'_>,
    world: &mut World,
    state: &mut DestructionState,
    entity: Entity,
    trigger: &Trigger,
    events: &mut Vec<Event>,
) -> Result<Vec<Entity>> {
    let Parts {
        worlds,
        bodies,
        settings,
    } = parts;
    let config = world
        .get::<Destructible3D>(entity)
        .cloned()
        .ok_or_else(|| invalid("Entity has no engine.Destructible3D"))?;
    if world.get::<Fractured>(entity).is_some() {
        return Err(invalid("Entity has already fractured"));
    }
    config.check()?;
    let record = bodies
        .three
        .get(&entity)
        .ok_or_else(|| invalid("Entity has no synced physics body"))?;
    let id = entity.to_bits();
    let missing = || invalid("Entity has no synced physics body");
    let pose = worlds.three.pose(id).ok_or_else(missing)?;
    let velocity = worlds.three.velocity(id).ok_or_else(missing)?;
    let scale = record.pose.0;
    let position = Vec3::from(pose.position);
    let rotation = Quat::from_array(pose.rotation);
    let impact_local = trigger.point.map(|p| rotation.inverse() * (p - position));

    let fracture: Arc<Fracture> = if config.prefracture {
        let cached = state
            .cache()
            .get(&entity)
            .filter(|c| c.config == config && c.body == record.config && c.scale == scale)
            .map(|c| c.fracture.clone());
        match cached {
            Some(fracture) => fracture,
            None => {
                let fracture = Arc::new(compute(&config, &record.config, scale, None)?);
                state.cache().insert(
                    entity,
                    Cached {
                        config: config.clone(),
                        body: record.config.clone(),
                        scale,
                        fracture: fracture.clone(),
                    },
                );
                fracture
            }
        }
    } else {
        Arc::new(compute(&config, &record.config, scale, impact_local)?)
    };
    if fracture
        .fragments
        .iter()
        .any(|f| f.hull_volume < MIN_FRAGMENT_VOLUME)
    {
        return Err(invalid(
            "engine.Destructible3D fragments below 1e-9 m^3 cannot be physics bodies; use fewer fragments or a larger body",
        ));
    }
    let total_volume: f32 = fracture.fragments.iter().map(|f| f.volume).sum();
    let centre_local: Vec3 = fracture
        .fragments
        .iter()
        .map(|f| f.centroid * f.volume)
        .sum::<Vec3>()
        / total_volume;
    let base = settings.material(&record.config.material)?;
    let body_mass = worlds.three.mass(id).unwrap_or(0.);
    let target_mass = match (config.debris.density, body_mass > 0.) {
        (Some(density), _) => density * total_volume,
        (None, true) => body_mass,
        (None, false) => base.density * total_volume,
    };
    let (v, w) = (Vec3::from(velocity.linear), Vec3::from(velocity.angular));
    let inherit = config.debris.inherit_velocity;
    let offsets: Vec<Vec3> = fracture
        .fragments
        .iter()
        .map(|f| rotation * (f.centroid - centre_local))
        .collect();
    let masses: Vec<f32> = fracture
        .fragments
        .iter()
        .map(|f| target_mass * f.volume / total_volume)
        .collect();
    let mut scatter: Vec<Vec3> = offsets
        .iter()
        .map(|o| {
            let dir = if o.length() > 1e-6 {
                o.normalize()
            } else {
                Vec3::Y
            };
            dir * config.debris.scatter_speed
        })
        .collect();
    let mean = scatter
        .iter()
        .zip(&masses)
        .map(|(s, m)| *s * *m)
        .sum::<Vec3>()
        / target_mass;
    for s in &mut scatter {
        *s -= mean;
    }
    // Everything that can fail is checked before the first entity spawns: an error part-way
    // through would leave fragments next to the intact body.
    let densities: Vec<f32> = fracture
        .fragments
        .iter()
        .zip(&masses)
        .map(|(fragment, mass)| mass / fragment.hull_volume)
        .collect();
    if densities
        .iter()
        .any(|density| !(0.000001..=10_000.).contains(density))
    {
        return Err(invalid(
            "engine.Destructible3D fragment density is outside 0.000001..=10000 kg/m^3",
        ));
    }
    let owner_id = world
        .get::<AuthoredId>(entity)
        .map(|a| a.id.clone())
        .unwrap_or_else(|| format!("entity{}", entity.index()));
    state.sequence += 1;
    let sequence_base = state.sequence;
    let mut spawned = Vec::with_capacity(fracture.fragments.len());
    for (index, fragment) in fracture.fragments.iter().enumerate() {
        let local = fragment.local_hull_points();
        let density = densities[index];
        let material = BodyMaterial {
            density,
            friction: base.friction,
            restitution: base.restitution,
            friction_combine: combine(base.friction_combine),
            restitution_combine: combine(base.restitution_combine),
            sensor: base.sensor,
            layer: base.layer,
            mask: base.mask,
            preset: None,
            layer_name: None,
        };
        let linear = (v + w.cross(offsets[index])) * inherit + scatter[index];
        let frag_id = unique_id(world, &owner_id, index);
        let transform = Transform {
            position: (position + rotation * fragment.centroid).to_array(),
            rotation: rotation.to_array(),
            ..Transform::default()
        };
        let body = PhysicsBody3D {
            motion: Motion::Dynamic,
            shape: Shape::ConvexHull,
            points: local.iter().map(|p| p.to_array()).collect(),
            velocity: linear.to_array(),
            angular_velocity: (w * inherit).to_array(),
            ccd: record.config.ccd,
            material,
            ..PhysicsBody3D::default()
        };
        let fragment_entity = world
            .spawn((
                OwnedBy(entity),
                AuthoredId {
                    id: frag_id.clone(),
                    name: frag_id.clone(),
                },
                transform,
                body,
                DebrisFragment {
                    owner: entity,
                    index: index as u32,
                },
            ))
            .id();
        if let Some(mut lookup) = world.get_resource_mut::<EntityLookup>() {
            lookup.0.insert(frag_id.clone(), fragment_entity);
        }
        state.meshes.insert(
            fragment_entity,
            Arc::new(FragmentRender {
                positions: fragment
                    .mesh
                    .positions
                    .iter()
                    .map(|p| *p - fragment.centroid)
                    .collect(),
                triangles: fragment.mesh.triangles.clone(),
                interior: fragment.mesh.interior.clone(),
            }),
        );
        state.debris.insert(
            fragment_entity,
            Debris {
                owner: entity,
                index: index as u32,
                id: frag_id,
                pool: config.debris.pool.clone(),
                max_active: config.debris.max_active,
                recycle: config.debris.recycle,
                sequence: sequence_base + index as u64,
                volume: fragment.volume,
                age: 0.,
                lifetime: config.debris.lifetime,
                sleep_velocity: config.debris.sleep_velocity,
                slow_for: 0.,
            },
        );
        spawned.push(fragment_entity);
    }
    state.sequence = sequence_base + spawned.len() as u64;
    let point = trigger.point.unwrap_or(position);
    world
        .entity_mut(entity)
        .remove::<PhysicsBody3D>()
        .insert(Fractured {
            fragments: spawned.len() as u32,
            impulse: trigger.impulse,
            point,
        });
    state.cache().remove(&entity);
    state.pending.remove(&entity);
    state.damage.remove(&entity);
    events.push(Event {
        name: "fractured",
        target: entity,
        payload: json!({
            "dimension": 3,
            "cause": trigger.cause,
            "impulse": trigger.impulse,
            "force": trigger.force,
            "fragments": spawned.len(),
            "point": point.to_array(),
            "mass": body_mass,
            "linear_velocity": velocity.linear,
            "angular_velocity": velocity.angular,
        }),
    });
    state.enforce_pool(world, &config.debris.pool, events);
    // A pool smaller than the new fracture can recycle fragments of this very fracture.
    spawned.retain(|e| world.get_entity(*e).is_ok());
    Ok(spawned)
}
fn unique_id(world: &World, owner: &str, index: usize) -> String {
    let mut id = format!("{owner}#f{index}");
    let mut suffix = 1;
    while crate::services::find_entity(world, &id).is_some() {
        id = format!("{owner}#f{index}.{suffix}");
        suffix += 1;
    }
    id
}
fn deliver(world: &mut World, events: Vec<Event>) -> Result<()> {
    for event in events {
        if world.get_entity(event.target).is_ok() {
            emit(
                world,
                &Destruction,
                event.name,
                event.target,
                None,
                event.payload,
            )?;
        }
    }
    Ok(())
}

pub(super) struct Destruction;
impl Feature for Destruction {
    fn name(&self) -> &'static str {
        "destruction"
    }
    fn descriptors(&self) -> Vec<crate::ComponentDescriptor> {
        vec![descriptor()]
    }
    fn register_components(&self, app: &mut engine_core::EngineApp) -> Result<()> {
        if !app.world().contains_resource::<DestructionState>() {
            app.world_mut().insert_resource(DestructionState::default());
        }
        crate::register_component::<Destructible3D>(app, "engine.Destructible3D", 1)
    }
    fn validate(&self, world: &World, _settings: &Resolved) -> Result<()> {
        for entity in world.iter_entities() {
            let Some(config) = entity.get::<Destructible3D>() else {
                continue;
            };
            config.check()?;
            if entity.contains::<Fractured>() {
                continue;
            }
            let Some(body) = entity.get::<PhysicsBody3D>() else {
                return Err(invalid(
                    "engine.Destructible3D requires engine.PhysicsBody3D",
                ));
            };
            if config.mesh.is_none() && !matches!(body.shape, Shape::Box | Shape::ConvexHull) {
                return Err(invalid(
                    "engine.Destructible3D needs a box or convex_hull body, or an authored mesh",
                ));
            }
        }
        Ok(())
    }
    fn sync(&mut self, cx: Context<'_>, world: &World) -> Result<()> {
        let Some(state) = world.get_resource::<DestructionState>() else {
            return Ok(());
        };
        let mut live = BTreeSet::new();
        for (entity, record) in &cx.bodies.three {
            let Some(config) = world.get::<Destructible3D>(*entity) else {
                continue;
            };
            if !config.prefracture || world.get::<Fractured>(*entity).is_some() {
                continue;
            }
            live.insert(*entity);
            let scale = record.pose.0;
            if !state.fresh(*entity, config, &record.config, scale) {
                let fracture = Arc::new(compute(config, &record.config, scale, None)?);
                state.cache().insert(
                    *entity,
                    Cached {
                        config: config.clone(),
                        body: record.config.clone(),
                        scale,
                        fracture,
                    },
                );
            }
        }
        state.cache().retain(|entity, _| live.contains(entity));
        Ok(())
    }
    fn after_substep(&mut self, cx: Context<'_>, world: &mut World, dt: f32) -> Result<()> {
        if !world.contains_resource::<DestructionState>() {
            return Ok(());
        }
        let mut loads = Vec::new();
        for entity in cx.bodies.three.keys() {
            let Some(config) = world.get::<Destructible3D>(*entity) else {
                continue;
            };
            if world.get::<Fractured>(*entity).is_some() {
                continue;
            }
            let id = entity.to_bits();
            if cx.worlds.three.is_sleeping(id) != Ok(false) {
                continue;
            }
            let Ok(contact) = cx.worlds.three.contact_impulse(id) else {
                continue;
            };
            if contact.impulse > 0. {
                loads.push((
                    *entity,
                    contact,
                    config.health.is_some(),
                    config.damage_floor,
                ));
            }
        }
        let mut state = world.resource_mut::<DestructionState>();
        state.tick_time += dt;
        for (entity, contact, tracks_damage, floor) in loads {
            let load = state.pending.entry(entity).or_default();
            if contact.impulse > load.peak_impulse {
                load.peak_impulse = contact.impulse;
                load.peak_force = contact.impulse / dt;
                load.point = Some(Vec3::from(contact.point));
            }
            if tracks_damage && contact.impulse > floor {
                load.damage += contact.impulse - floor;
            }
        }
        Ok(())
    }
    fn after_step(&mut self, cx: Context<'_>, world: &mut World) -> Result<()> {
        if !world.contains_resource::<DestructionState>() {
            return Ok(());
        }
        let Context {
            worlds,
            bodies,
            settings,
            ..
        } = cx;
        let events = world.resource_scope(|world, mut state: Mut<DestructionState>| {
            let mut events = Vec::new();
            for (entity, trigger) in state.collect_triggers(world) {
                let parts = Parts {
                    worlds: &mut *worlds,
                    bodies,
                    settings,
                };
                execute(parts, world, &mut state, entity, &trigger, &mut events)?;
            }
            state.tick_debris(worlds, world, &mut events);
            Ok::<_, crate::Error>(events)
        })?;
        deliver(world, events)
    }
}

// ---- Native API ----------------------------------------------------------------------

/// Fractures `entity` immediately, as if `impulse` (N*s) had hit it at world-space
/// `impact_point`. The body must be synced (it exists after the first simulation tick).
/// The intact body is removed on the next tick; fragments become bodies on the next tick too.
pub fn fracture_now(
    world: &mut World,
    entity: Entity,
    impact_point: Vec3,
    impulse: f32,
) -> Result<FractureReport> {
    if !impact_point.is_finite() || !impulse.is_finite() || impulse < 0. {
        return Err(invalid(
            "fracture_now needs a finite impact point and a non-negative impulse",
        ));
    }
    if !world.contains_resource::<Services>() {
        return Err(invalid("Physics services are disabled"));
    }
    if !world.contains_resource::<DestructionState>() {
        world.insert_resource(DestructionState::default());
    }
    let trigger = Trigger {
        point: Some(impact_point),
        impulse,
        force: 0.,
        cause: "api",
    };
    let (fragments, events) = world.resource_scope(|world, mut services: Mut<Services>| {
        world.resource_scope(|world, mut state: Mut<DestructionState>| {
            let physics = &mut services.physics;
            let parts = Parts {
                worlds: &mut physics.worlds,
                bodies: &physics.bodies,
                settings: &physics.settings,
            };
            let mut events = Vec::new();
            let fragments = execute(parts, world, &mut state, entity, &trigger, &mut events)?;
            Ok::<_, crate::Error>((fragments, events))
        })
    })?;
    deliver(world, events)?;
    Ok(FractureReport {
        fragments,
        impulse,
        point: impact_point,
    })
}
/// Live fragments of a fractured `entity`, ordered by index.
pub fn fragments(world: &World, entity: Entity) -> Vec<FragmentInfo> {
    let Some(state) = world.get_resource::<DestructionState>() else {
        return vec![];
    };
    let mut result: Vec<FragmentInfo> = state
        .debris
        .iter()
        .filter(|(_, d)| d.owner == entity)
        .map(|(e, d)| FragmentInfo {
            entity: *e,
            index: d.index,
            volume: d.volume,
            age: d.age,
        })
        .collect();
    result.sort_by_key(|f| f.index);
    result
}
/// Render mesh of a fragment entity, relative to the entity's origin. Present while the
/// fragment is alive. The renderer hookup draws these through a `PipelineFeature`; until then
/// the physics debug view draws the hulls.
pub fn fragment_mesh(world: &World, fragment: Entity) -> Option<Arc<FragmentRender>> {
    world
        .get_resource::<DestructionState>()?
        .meshes
        .get(&fragment)
        .cloned()
}
/// Fracture threshold and damage state of `entity`.
pub fn destructible_status(world: &World, entity: Entity) -> Result<DestructibleStatus> {
    let config = world
        .get::<Destructible3D>(entity)
        .ok_or_else(|| invalid("Entity has no engine.Destructible3D"))?;
    let state = world.get_resource::<DestructionState>();
    Ok(DestructibleStatus {
        fractured: world.get::<Fractured>(entity).is_some(),
        damage: state
            .and_then(|s| s.damage.get(&entity).copied())
            .unwrap_or(0.),
        health: config.health,
        threshold: config.threshold,
        prefractured: state.is_some_and(|s| s.cache().contains_key(&entity)),
        live_fragments: state.map_or(0, |s| {
            s.debris.values().filter(|d| d.owner == entity).count()
        }),
    })
}
/// Sets `engine.Destructible3D.threshold` (in the unit of its `threshold_mode`).
pub fn set_fracture_threshold(world: &mut World, entity: Entity, threshold: f32) -> Result<()> {
    let mut config = world
        .get_mut::<Destructible3D>(entity)
        .ok_or_else(|| invalid("Entity has no engine.Destructible3D"))?;
    let mut updated = config.clone();
    updated.threshold = threshold;
    updated.check()?;
    config.threshold = threshold;
    Ok(())
}
/// Live debris bodies, in one pool or all.
pub fn debris_count(world: &World, pool: Option<&str>) -> usize {
    let Some(state) = world.get_resource::<DestructionState>() else {
        return 0;
    };
    match pool {
        Some(pool) => state.debris_in(pool),
        None => state.debris.len(),
    }
}
/// What a fracture of `config` on `body` would produce, without touching the world.
/// `scale` is the body's world scale.
pub fn preview_fracture(
    config: &Destructible3D,
    body: &PhysicsBody3D,
    scale: Vec3,
) -> Result<Vec<f32>> {
    config.check()?;
    let fracture = compute(config, body, scale, None)?;
    Ok(fracture.fragments.iter().map(|f| f.volume).collect())
}

// ---- Schema --------------------------------------------------------------------------

fn descriptor() -> crate::ComponentDescriptor {
    let vector3 = json!({"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3});
    let number = |min: f64, max: f64, doc: &str| json!({"type":"number","minimum":min,"maximum":max,"description":doc});
    let debris = json!({"type":"object","additionalProperties":false,"properties":{
        "lifetime":number(0., 86400., "Seconds a fragment lives; 0 keeps it until recycled."),
        "sleep_velocity":number(0., 100., "Speed (m/s, rad/s) below which fragments are forced to sleep; 0 disables."),
        "max_active":{"type":"integer","minimum":1,"maximum":MAX_POOL,"description":"Most fragments alive in the pool; must be at least fragment_count."},
        "recycle":{"type":"string","enum":["oldest","smallest"]},
        "pool":{"type":"string","minLength":1,"maxLength":64,"description":"Debris pool name; pools share max_active."},
        "inherit_velocity":number(0., 1., "Fraction of the body's velocity the fragments inherit."),
        "scatter_speed":number(0., 1000., "Outward speed added to fragments (m/s); adds no net momentum."),
        "density":{"type":"number","minimum":0.000001,"maximum":10000,"description":"Fragment density in kg/m^3; omitted keeps the body mass."}
    }});
    let mesh = json!({"type":"object","additionalProperties":false,"properties":{
        "vertices":{"type":"array","maxItems":MAX_MESH_VERTICES,"items":vector3},
        "indices":{"type":"array","maxItems":MAX_MESH_TRIANGLES,"items":{"type":"array","minItems":3,"maxItems":3,
            "items":{"type":"integer","minimum":0,"maximum":MAX_MESH_VERTICES - 1}}}
    },"description":"Closed outward-wound triangle mesh in body-local unscaled coordinates, fractured instead of the body shape."});
    crate::ComponentDescriptor {
        name: "engine.Destructible3D".into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "pattern":{"type":"string","enum":["uniform","impact"]},
            "fragment_count":{"type":"integer","minimum":2,"maximum":MAX_FRAGMENTS,"description":"Voronoi seeds, and so pieces."},
            "seed":{"type":"integer","minimum":0,"description":"Deterministic seed generator seed."},
            "impact_point":vector3,
            "impact_radius":number(0.0001, 1e6, "Radius of the impact seed cluster in meters."),
            "mesh":mesh,
            "threshold":number(0.000001, 1e9, "Contact load that fractures the body (N*s per substep, or N in force mode)."),
            "threshold_mode":{"type":"string","enum":["impulse","force"]},
            "health":number(0.000001, 1e12, "Accumulated damage that fractures the body; omitted disables damage."),
            "damage_floor":number(0., 1e9, "Per-substep contact impulse below which no damage accumulates."),
            "prefracture":{"type":"boolean","description":"Compute and cache fragments at load instead of at fracture time."},
            "debris":debris
        }}),
        defaults: serde_json::to_value(Destructible3D::default()).expect("finite defaults"),
        requires: vec!["engine.PhysicsBody3D".into()],
        entity_references: vec![],
        data_only: false,
    }
}
