//! Deformable physics: cloth (`engine.Cloth`), soft bodies (`engine.SoftBody`) and particle
//! fluids (`engine.FluidVolume`) on top of `engine-xpbd` and `engine-fluids`.
//!
//! Every authored object owns a private solver world (so its gravity, wind, substeps and
//! collision mask are its own). After each engine-physics substep the feature builds collider
//! proxies from the rigid bodies, steps the solver with the same fixed `dt`, and applies the
//! reported reaction impulses back to dynamic bodies (two-way coupling). Particles live in
//! world space; the entity `Transform` is only the creation anchor, except that fluid domains,
//! emitters and kill volumes follow the entity translation. After the step the surfaces are
//! published in the [`PhysicsRenderData`] world resource. See `docs/physics-cloth.md`,
//! `docs/physics-softbody.md` and `docs/physics-fluids.md`.
use super::{Context, Feature, Resolved};
use crate::Result;
use crate::simulation::{invalid, world_pose};
use bevy_ecs::prelude::*;
use engine_fluids::{FluidWorld, VolumeDesc};
use engine_xpbd::{XpbdSettings, XpbdWorld};
use glam::Vec3;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

mod cloth;
mod colliders;
mod fluid;
mod render;
mod softbody;

pub use cloth::{Cloth, ClothAttachment, ClothEdge, ClothSource};
pub use fluid::{
    EmitterShape, FluidEmitter, FluidRegion, FluidVolume, MAX_FLUID_CAPACITY, RegionShape,
};
pub use render::{
    ClothRender, FluidRender, PhysicsRenderData, SoftBodyRender, deformable_debug_segments,
};
pub use softbody::{SoftBody, SoftBodySource};

use colliders::Collider;

/// Most objects of each kind in one scene.
pub const MAX_CLOTHS: usize = 256;
pub const MAX_SOFT_BODIES: usize = 256;
pub const MAX_FLUID_VOLUMES: usize = 32;
/// Id of the only object in each private solver world.
const OBJECT: u64 = 1;
/// Extra distance around a deformable within which rigid bodies become collider proxies.
const PROXY_MARGIN: f32 = 0.25;

/// Rigid coupling controls shared by the three components.
#[derive(Clone, Copy, Debug)]
pub(super) struct Coupling {
    pub two_way: bool,
    pub scale: f32,
    pub mask: u32,
}
impl Coupling {
    fn validate(&self, name: &str) -> Result<()> {
        check(
            name,
            &[(
                self.scale.is_finite() && (0. ..=100.).contains(&self.scale),
                "coupling_scale (0..=100)",
            )],
        )
    }
}
fn bad<T>(name: &str, what: &str) -> Result<T> {
    Err(invalid(format!("{name}: {what}")))
}
/// Fails naming the first violated bound.
fn check(name: &str, checks: &[(bool, &str)]) -> Result<()> {
    match checks.iter().find(|(ok, _)| !ok) {
        Some((_, what)) => bad(name, &format!("invalid {what}")),
        None => Ok(()),
    }
}
fn number(min: f64, max: f64, doc: &str) -> Value {
    json!({"type":"number","minimum":min,"maximum":max,"description":doc})
}
fn vector(n: usize) -> Value {
    json!({"type":"array","items":{"type":"number"},"minItems":n,"maxItems":n})
}
fn coupling_schema(properties: &mut Value) {
    properties["two_way"] = json!({"type":"boolean",
        "description":"Apply reaction impulses to dynamic rigid bodies; off keeps one-way collision."});
    properties["coupling_scale"] = number(0., 100., "Multiplier on the reaction impulses.");
    properties["collision_mask"] = json!({"type":"integer","minimum":0,"maximum":4294967295u64,
        "description":"Rigid bodies whose material layer shares a bit with this mask collide."});
}

struct Attached {
    particle: usize,
    entity: Entity,
    offset: Vec3,
}
struct ClothRt {
    structure: Cloth,
    config: Cloth,
    world: XpbdWorld,
    attached: Vec<Attached>,
    triangles: Arc<[[u32; 3]]>,
}
struct SoftRt {
    structure: SoftBody,
    config: SoftBody,
    world: XpbdWorld,
    triangles: Arc<[[u32; 3]]>,
}
struct FluidRt {
    structure: Vec<FluidRegion>,
    config: FluidVolume,
    world: FluidWorld,
    full: bool,
}

/// The deformables feature.
pub(super) struct Deformables {
    cloths: BTreeMap<Entity, ClothRt>,
    softs: BTreeMap<Entity, SoftRt>,
    fluids: BTreeMap<Entity, FluidRt>,
    frame: u64,
}
impl Deformables {
    pub(super) fn new() -> Self {
        Self {
            cloths: BTreeMap::new(),
            softs: BTreeMap::new(),
            fluids: BTreeMap::new(),
            frame: 0,
        }
    }
    fn is_empty(&self) -> bool {
        self.cloths.is_empty() && self.softs.is_empty() && self.fluids.is_empty()
    }
}

fn xpbd_settings(
    gravity: Vec3,
    scale: f32,
    wind: [f32; 3],
    air_density: f32,
    substeps: u32,
    iterations: u32,
) -> XpbdSettings {
    XpbdSettings {
        gravity: gravity * scale,
        wind: Vec3::from(wind),
        air_density,
        substeps,
        iterations,
    }
}
fn solver_world(settings: XpbdSettings, name: &str) -> Result<XpbdWorld> {
    XpbdWorld::new(settings, engine_xpbd::MAX_OBJECT_PARTICLES)
        .map_err(|e| invalid(format!("{name}: {e}")))
}

impl Deformables {
    fn sync_cloth(
        &mut self,
        entity: Entity,
        config: &Cloth,
        world: &World,
        gravity: Vec3,
    ) -> Result<()> {
        const NAME: &str = "engine.Cloth";
        let settings = xpbd_settings(
            gravity,
            config.gravity_scale,
            config.wind,
            config.air_density,
            config.substeps,
            config.iterations,
        );
        let structure = config.structure();
        let convert = |e: engine_xpbd::Error| invalid(format!("{NAME}: {e}"));
        match self.cloths.get_mut(&entity) {
            Some(rt) if rt.structure == structure => {
                if rt.world.settings() != settings {
                    rt.world.set_settings(settings).map_err(convert)?;
                }
                let cloth = rt.world.cloth_mut(OBJECT).expect("cloth exists");
                if cloth.params() != config.params() {
                    cloth.set_params(config.params()).map_err(convert)?;
                }
                rt.config = config.clone();
            }
            _ => {
                let pose = world_pose(world, entity)?;
                let desc = config.describe(pose)?;
                let mut attached = Vec::new();
                for attachment in &config.attachments {
                    let target = crate::services::find_entity(world, &attachment.entity)
                        .ok_or_else(|| {
                            invalid(format!(
                                "{NAME}: attachment entity {} does not exist",
                                attachment.entity
                            ))
                        })?;
                    let offset = match attachment.offset {
                        Some(offset) => Vec3::from(offset),
                        None => {
                            let (scale, rotation, position) = world_pose(world, target)?;
                            let rest = desc.positions[attachment.particle as usize];
                            (rotation.inverse() * (rest - position)) / scale
                        }
                    };
                    attached.push(Attached {
                        particle: attachment.particle as usize,
                        entity: target,
                        offset,
                    });
                }
                let mut solver = solver_world(settings, NAME)?;
                solver.insert_cloth(OBJECT, desc).map_err(convert)?;
                let triangles = solver
                    .cloth(OBJECT)
                    .expect("cloth exists")
                    .triangles()
                    .to_vec()
                    .into();
                self.cloths.insert(
                    entity,
                    ClothRt {
                        structure,
                        config: config.clone(),
                        world: solver,
                        attached,
                        triangles,
                    },
                );
            }
        }
        // Attachments: the particle target is the entity pose at this tick.
        let rt = self.cloths.get_mut(&entity).expect("cloth exists");
        for attachment in &rt.attached {
            let (scale, rotation, position) = world_pose(world, attachment.entity)?;
            let target = position + rotation * (attachment.offset * scale);
            rt.world
                .cloth_mut(OBJECT)
                .expect("cloth exists")
                .set_pin_target(attachment.particle, target)
                .map_err(convert)?;
        }
        Ok(())
    }

    fn sync_soft(
        &mut self,
        entity: Entity,
        config: &SoftBody,
        world: &World,
        gravity: Vec3,
    ) -> Result<()> {
        const NAME: &str = "engine.SoftBody";
        let settings = xpbd_settings(
            gravity,
            config.gravity_scale,
            [0.; 3],
            0.,
            config.substeps,
            config.iterations,
        );
        let structure = config.structure();
        let convert = |e: engine_xpbd::Error| invalid(format!("{NAME}: {e}"));
        match self.softs.get_mut(&entity) {
            Some(rt) if rt.structure == structure => {
                if rt.world.settings() != settings {
                    rt.world.set_settings(settings).map_err(convert)?;
                }
                let body = rt.world.soft_body_mut(OBJECT).expect("soft body exists");
                if body.params() != config.params() {
                    body.set_params(config.params()).map_err(convert)?;
                }
                rt.config = config.clone();
            }
            _ => {
                let desc = config.describe(world_pose(world, entity)?)?;
                let mut solver = solver_world(settings, NAME)?;
                solver.insert_soft_body(OBJECT, desc).map_err(convert)?;
                let triangles = solver
                    .soft_body(OBJECT)
                    .expect("soft body exists")
                    .surface_triangles()
                    .to_vec()
                    .into();
                self.softs.insert(
                    entity,
                    SoftRt {
                        structure,
                        config: config.clone(),
                        world: solver,
                        triangles,
                    },
                );
            }
        }
        Ok(())
    }

    fn sync_fluid(
        &mut self,
        entity: Entity,
        config: &FluidVolume,
        world: &World,
        gravity: Vec3,
    ) -> Result<()> {
        const NAME: &str = "engine.FluidVolume";
        let (scale, _, position) = world_pose(world, entity)?;
        let convert = |e: engine_fluids::Error| invalid(format!("{NAME}: {e}"));
        let params = config.params(gravity);
        let domain = config.domain(position, scale);
        let rt = match self.fluids.entry(entity) {
            std::collections::btree_map::Entry::Occupied(slot) => slot.into_mut(),
            std::collections::btree_map::Entry::Vacant(slot) => {
                let mut fluid = FluidWorld::new();
                fluid
                    .create_volume(
                        OBJECT,
                        VolumeDesc {
                            params,
                            domain,
                            capacity: config.capacity as usize,
                        },
                    )
                    .map_err(convert)?;
                slot.insert(FluidRt {
                    structure: vec![],
                    config: config.clone(),
                    world: fluid,
                    full: false,
                })
            }
        };
        let volume = rt.world.volume_mut(OBJECT).map_err(convert)?;
        if *volume.params() != params {
            volume.set_params(params).map_err(convert)?;
        }
        if *volume.domain() != domain {
            volume.set_domain(domain).map_err(convert)?;
        }
        if volume.capacity() != config.capacity as usize {
            volume
                .set_capacity(config.capacity as usize)
                .map_err(convert)?;
        }
        for (index, emitter) in config.emitters.iter().enumerate() {
            let desc = emitter.world(position, scale);
            if volume.emitter(index as u64) != Some(&desc) {
                volume.set_emitter(index as u64, desc).map_err(convert)?;
            }
        }
        for index in config.emitters.len()..fluid::MAX_EMITTERS {
            volume.remove_emitter(index as u64);
        }
        for (index, region) in config.kill_volumes.iter().enumerate() {
            volume
                .set_kill_volume(index as u64, region.world(position, scale))
                .map_err(convert)?;
        }
        for index in config.kill_volumes.len()..fluid::MAX_KILL_VOLUMES {
            volume.remove_kill_volume(index as u64);
        }
        if rt.structure != config.fills_key() {
            // Initial fills run once at creation and again whenever they are edited.
            volume.clear();
            for fill in &config.fills {
                volume
                    .fill(fill.world(position, scale), Vec3::from(fill.velocity), None)
                    .map_err(convert)?;
            }
            rt.structure = config.fills_key().to_vec();
            rt.full = false;
        }
        rt.config = config.clone();
        Ok(())
    }
}

impl Feature for Deformables {
    fn name(&self) -> &'static str {
        "deformables"
    }
    fn descriptors(&self) -> Vec<crate::ComponentDescriptor> {
        vec![
            cloth::descriptor(),
            softbody::descriptor(),
            fluid::descriptor(),
        ]
    }
    fn register_components(&self, app: &mut engine_core::EngineApp) -> Result<()> {
        crate::register_component::<Cloth>(app, "engine.Cloth", 1)?;
        crate::register_component::<SoftBody>(app, "engine.SoftBody", 1)?;
        crate::register_component::<FluidVolume>(app, "engine.FluidVolume", 1)
    }
    fn active(&self) -> bool {
        !self.is_empty()
    }
    fn validate(&self, world: &World, _settings: &Resolved) -> Result<()> {
        let (mut cloths, mut softs, mut fluids) = (0, 0, 0);
        for entity in world.iter_entities() {
            if let Some(cloth) = entity.get::<Cloth>() {
                cloths += 1;
                cloth.validate()?;
                for attachment in &cloth.attachments {
                    match crate::services::find_entity(world, &attachment.entity) {
                        Some(target) if world.get::<crate::Transform>(target).is_some() => {}
                        Some(_) => {
                            return Err(invalid(format!(
                                "engine.Cloth: attachment entity {} requires Transform",
                                attachment.entity
                            )));
                        }
                        None => {
                            return Err(invalid(format!(
                                "engine.Cloth: attachment entity {} does not exist",
                                attachment.entity
                            )));
                        }
                    }
                }
            }
            if let Some(soft) = entity.get::<SoftBody>() {
                softs += 1;
                soft.validate()?;
            }
            if let Some(fluid) = entity.get::<FluidVolume>() {
                fluids += 1;
                fluid.validate()?;
            }
        }
        if cloths > MAX_CLOTHS || softs > MAX_SOFT_BODIES || fluids > MAX_FLUID_VOLUMES {
            return Err(invalid(format!(
                "A scene has at most {MAX_CLOTHS} cloths, {MAX_SOFT_BODIES} soft bodies and {MAX_FLUID_VOLUMES} fluid volumes"
            )));
        }
        Ok(())
    }
    fn sync(&mut self, cx: Context<'_>, world: &World) -> Result<()> {
        let gravity = Vec3::from(cx.settings.gravity_3d);
        let (mut cloths, mut softs, mut fluids) =
            (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        for entity in world.iter_entities() {
            let id = entity.id();
            if let Some(config) = entity.get::<Cloth>() {
                self.sync_cloth(id, config, world, gravity)?;
                cloths.insert(id);
            }
            if let Some(config) = entity.get::<SoftBody>() {
                self.sync_soft(id, config, world, gravity)?;
                softs.insert(id);
            }
            if let Some(config) = entity.get::<FluidVolume>() {
                self.sync_fluid(id, config, world, gravity)?;
                fluids.insert(id);
            }
        }
        self.cloths.retain(|id, _| cloths.contains(id));
        self.softs.retain(|id, _| softs.contains(id));
        self.fluids.retain(|id, _| fluids.contains(id));
        Ok(())
    }
    fn after_substep(&mut self, mut cx: Context<'_>, world: &mut World, dt: f32) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        let colliders = colliders::collect(&cx);
        // Bodies beyond a solver's proxy limit are left out (nearest kept) and counted.
        let mut omitted = 0;
        for rt in self.cloths.values_mut() {
            let (min, max) = bounds(rt.world.cloth(OBJECT).expect("cloth exists").positions());
            let coupling = rt.config.coupling();
            let mut near = colliders::near(
                &colliders,
                coupling.mask,
                min,
                max,
                rt.config.thickness + PROXY_MARGIN,
            );
            omitted += colliders::limit(&mut near, min, max, engine_xpbd::MAX_PROXIES);
            step_xpbd(&mut rt.world, &mut cx, &near, coupling, dt, "engine.Cloth")?;
        }
        for rt in self.softs.values_mut() {
            let (min, max) = bounds(
                rt.world
                    .soft_body(OBJECT)
                    .expect("soft body exists")
                    .positions(),
            );
            let coupling = rt.config.coupling();
            let mut near = colliders::near(
                &colliders,
                coupling.mask,
                min,
                max,
                rt.config.particle_radius + PROXY_MARGIN,
            );
            omitted += colliders::limit(&mut near, min, max, engine_xpbd::MAX_PROXIES);
            step_xpbd(
                &mut rt.world,
                &mut cx,
                &near,
                coupling,
                dt,
                "engine.SoftBody",
            )?;
        }
        for (&entity, rt) in &mut self.fluids {
            let domain = *rt.world.volume(OBJECT).expect("volume exists").domain();
            let coupling = rt.config.coupling();
            let mut near = colliders::near(
                &colliders,
                coupling.mask,
                domain.min,
                domain.max,
                PROXY_MARGIN,
            );
            omitted += colliders::limit(
                &mut near,
                domain.min,
                domain.max,
                engine_fluids::MAX_PROXIES,
            );
            let proxies: Vec<_> = near.iter().map(Collider::fluid).collect();
            let report = rt
                .world
                .step(dt, &proxies)
                .map_err(|e| invalid(format!("engine.FluidVolume: {e}")))?;
            if coupling.two_way {
                for impulse in &report.proxy_impulses {
                    colliders::apply(
                        cx.worlds,
                        &near,
                        impulse.id,
                        impulse.linear,
                        impulse.angular,
                        coupling.scale,
                    )?;
                }
            }
            let step = &report.volumes[0].1;
            if step.capacity_reached && !rt.full {
                super::emit_named(
                    world,
                    "physics.fluid.capacity_reached",
                    entity,
                    None,
                    json!({
                        "capacity": rt.config.capacity,
                        "particles": step.particle_count,
                        "dropped": step.dropped_at_capacity,
                    }),
                )?;
            }
            rt.full = step.capacity_reached;
        }
        super::pressure::note_proxies_omitted(world, omitted);
        Ok(())
    }
    fn after_step(&mut self, _cx: Context<'_>, world: &mut World) -> Result<()> {
        if self.is_empty() && !world.contains_resource::<PhysicsRenderData>() {
            return Ok(());
        }
        self.frame += 1;
        let mut data = PhysicsRenderData {
            frame: self.frame,
            ..PhysicsRenderData::default()
        };
        for (&entity, rt) in &self.cloths {
            let cloth = rt.world.cloth(OBJECT).expect("cloth exists");
            data.cloths.insert(
                entity,
                ClothRender {
                    positions: cloth.positions().to_vec(),
                    normals: cloth.normals().to_vec(),
                    triangles: rt.triangles.clone(),
                },
            );
        }
        for (&entity, rt) in &self.softs {
            let body = rt.world.soft_body(OBJECT).expect("soft body exists");
            data.soft_bodies.insert(
                entity,
                SoftBodyRender {
                    positions: body.positions().to_vec(),
                    normals: render::vertex_normals(body.positions(), &rt.triangles),
                    triangles: rt.triangles.clone(),
                    volume: body.volume(),
                    rest_volume: body.rest_volume(),
                },
            );
        }
        for (&entity, rt) in &self.fluids {
            let volume = rt.world.volume(OBJECT).expect("volume exists");
            data.fluids.insert(
                entity,
                FluidRender {
                    positions: volume.positions().to_vec(),
                    velocities: volume.velocities().to_vec(),
                    densities: volume.densities().to_vec(),
                    surface: volume.surface_flags().to_vec(),
                    particle_radius: volume.params().particle_radius,
                    rest_density: volume.params().rest_density,
                    capacity: volume.capacity(),
                    domain_min: volume.domain().min,
                    domain_max: volume.domain().max,
                },
            );
        }
        world.insert_resource(data);
        Ok(())
    }
}

fn bounds(positions: &[Vec3]) -> (Vec3, Vec3) {
    positions.iter().fold(
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        |(min, max), p| (min.min(*p), max.max(*p)),
    )
}

/// Steps one cloth or soft-body solver against `near` and applies the reactions.
fn step_xpbd(
    solver: &mut XpbdWorld,
    cx: &mut Context<'_>,
    near: &[Collider],
    coupling: Coupling,
    dt: f32,
    name: &str,
) -> Result<()> {
    let proxies: Vec<_> = near.iter().map(Collider::xpbd).collect();
    solver
        .step(dt, &proxies)
        .map_err(|e| invalid(format!("{name}: {e}")))?;
    if coupling.two_way {
        for reaction in solver.proxy_impulses() {
            colliders::apply(
                cx.worlds,
                near,
                reaction.id,
                reaction.impulse,
                reaction.angular_impulse,
                coupling.scale,
            )?;
        }
    }
    Ok(())
}
