//! Physics service: authored bodies and settings, the feature seam, and the native API.
//!
//! # Feature seam
//!
//! A physics feature lives in `simulation/physics/<feature>.rs` (or a directory) and
//! implements [`Feature`]. It is registered by adding its `mod` item below and ONE entry
//! to [`features`]; the registry order is the execution order of every hook. Hooks run:
//!
//! 1. `validate` — every prepare/step, before any sync (reject bad authored data);
//! 2. `sync` — after authored bodies were mirrored into engine-physics;
//! 3. per substep: `before_substep`, engine-physics 2D and 3D steps, `after_substep`
//!    (drain engine-physics feature output, emit `physics.<feature>.<event>` via [`emit`]);
//! 4. `after_step` — after dynamic body poses were written back to `Transform`s.
//!
//! Authored components are declared by the feature through `descriptors` and
//! `register_components`, so no shared simulation file changes. Native API functions go in
//! the feature module and are re-exported from here with one `pub use` line.
use super::{Services, SimulationConfig, invalid, mapped};
use crate::Result;
use bevy_ecs::prelude::*;
use engine_physics::{World2D, World3D};
mod api;
mod bodies;
mod contacts;
mod descriptors;
mod joints;
mod settings;
mod types;
pub use api::*;
pub(crate) use bodies::Bodies;
pub use joints::{
    AxisKind, AxisSetting, BrokenJoint, JointKind, JointMotor, JointReadout, JointStatus,
    PhysicsJoint2D, PhysicsJoint3D, add_joint_2d, add_joint_3d, broken_joints, clear_joint_motor,
    joint_state, joint_status, remove_joint, set_joint_break_limits, set_joint_motor,
};
pub(crate) use settings::Resolved;
pub use settings::{MAX_DISABLED_PAIRS, MAX_MATERIALS, NamedMaterial, PhysicsSettings};
pub use types::*;

/// Physics features in execution order. Add one entry per feature.
fn features() -> Vec<Box<dyn Feature>> {
    vec![
        Box::new(contacts::Contacts),
        Box::new(joints::Joints::new()),
    ]
}

/// The engine-physics worlds owned by the service. Body IDs are `Entity::to_bits()`.
pub(crate) struct Worlds {
    pub(crate) two: World2D,
    pub(crate) three: World3D,
}
/// What a feature hook may read and mutate.
pub(crate) struct Context<'a> {
    pub(crate) worlds: &'a mut Worlds,
    /// Synced body records (authored config and pose) by entity.
    #[allow(dead_code)] // Seam for feature leaves; the contacts reference feature needs none.
    pub(crate) bodies: &'a Bodies,
    /// Resolved `engine.PhysicsSettings` (layers, materials, gravity, iterations).
    #[allow(dead_code)] // Seam for feature leaves; the contacts reference feature needs none.
    pub(crate) settings: &'a Resolved,
}
/// A physics feature plugged into sync and step. See the module docs for hook order.
pub(crate) trait Feature: Send + Sync {
    /// Stable lowercase name; events are `physics.<name>.<event>`.
    fn name(&self) -> &'static str;
    /// Authored component descriptors owned by this feature.
    fn descriptors(&self) -> Vec<crate::ComponentDescriptor> {
        vec![]
    }
    /// Registers the feature's authored component adapters.
    fn register_components(&self, _app: &mut engine_core::EngineApp) -> Result<()> {
        Ok(())
    }
    fn validate(&self, _world: &World, _settings: &Resolved) -> Result<()> {
        Ok(())
    }
    fn sync(&mut self, _cx: Context<'_>, _world: &World) -> Result<()> {
        Ok(())
    }
    fn before_substep(&mut self, _cx: Context<'_>, _dt: f32) -> Result<()> {
        Ok(())
    }
    fn after_substep(&mut self, _cx: Context<'_>, _world: &mut World, _dt: f32) -> Result<()> {
        Ok(())
    }
    fn after_step(&mut self, _cx: Context<'_>, _world: &mut World) -> Result<()> {
        Ok(())
    }
}
/// Emits `physics.<feature>.<event>` to `target`'s event bindings.
pub(crate) fn emit(
    world: &mut World,
    feature: &dyn Feature,
    event: &str,
    target: Entity,
    source: Option<Entity>,
    payload: serde_json::Value,
) -> Result<()> {
    let name = format!("physics.{}.{event}", feature.name());
    super::emit(world, target, source, &name, payload)
}

pub(crate) fn component_descriptors() -> Vec<crate::ComponentDescriptor> {
    let mut result = descriptors::component_descriptors();
    for feature in features() {
        result.extend(feature.descriptors());
    }
    result
}
pub(crate) fn register_components(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<PhysicsBody2D>(app, "engine.PhysicsBody2D", 1)?;
    crate::register_component::<PhysicsBody3D>(app, "engine.PhysicsBody3D", 1)?;
    crate::register_component::<PhysicsSettings>(app, "engine.PhysicsSettings", 1)?;
    for feature in features() {
        feature.register_components(app)?;
    }
    Ok(())
}
/// Scene-level checks run before every prepare/step: settings uniqueness and validity,
/// named material/layer references, then each feature's `validate`.
pub(super) fn validate(world: &World) -> Result<()> {
    let found = settings::find(world)?;
    let Some(services) = world.get_resource::<Services>() else {
        if found.is_some() {
            return Err(invalid(
                "Simulation services disabled for engine.PhysicsSettings",
            ));
        }
        return Ok(());
    };
    let physics = &services.physics;
    let resolved = Resolved::new(&physics.config, found)?;
    for entity in world.iter_entities() {
        let material = entity
            .get::<PhysicsBody2D>()
            .map(|b| &b.material)
            .or(entity.get::<PhysicsBody3D>().map(|b| &b.material));
        if let Some(material) = material {
            resolved.material(material)?;
        }
    }
    for feature in &physics.features {
        feature.validate(world, &resolved)?;
    }
    Ok(())
}
pub(super) struct Physics {
    config: SimulationConfig,
    worlds: Worlds,
    bodies: Bodies,
    settings: Resolved,
    features: Vec<Box<dyn Feature>>,
}
/// Reborrows the service parts as a hook [`Context`].
macro_rules! context {
    ($worlds:expr, $bodies:expr, $settings:expr) => {
        Context {
            worlds: &mut *$worlds,
            bodies: &*$bodies,
            settings: &*$settings,
        }
    };
}
impl Physics {
    pub fn new(config: SimulationConfig) -> Result<Self> {
        Ok(Self {
            worlds: Worlds {
                two: World2D::new(config.gravity_2d, config.body_capacity).map_err(mapped)?,
                three: World3D::new(config.gravity_3d, config.body_capacity).map_err(mapped)?,
            },
            bodies: Bodies::default(),
            settings: Resolved::new(&config, None)?,
            features: features(),
            config,
        })
    }
    pub fn sync(&mut self, world: &World) -> Result<()> {
        let resolved = Resolved::new(&self.config, settings::find(world)?)?;
        if resolved != self.settings {
            let worlds = &mut self.worlds;
            worlds
                .two
                .set_gravity(resolved.gravity_2d)
                .map_err(mapped)?;
            worlds
                .three
                .set_gravity(resolved.gravity_3d)
                .map_err(mapped)?;
            for result in [
                worlds.two.set_solver_iterations(resolved.solver_iterations),
                worlds
                    .three
                    .set_solver_iterations(resolved.solver_iterations),
            ] {
                result.map_err(mapped)?;
            }
            if resolved.layers != self.settings.layers
                || resolved.materials != self.settings.materials
            {
                // Bodies are rebuilt from their authored data with the new groups/materials.
                self.bodies.clear(worlds);
            }
            self.settings = resolved;
        }
        self.bodies.sync(&mut self.worlds, &self.settings, world)?;
        let Self {
            worlds,
            bodies,
            settings,
            features,
            ..
        } = self;
        for feature in features {
            feature.sync(context!(worlds, bodies, settings), world)?;
        }
        Ok(())
    }
    pub fn step(&mut self, world: &mut World, dt: f32) -> Result<()> {
        if self.bodies.is_empty() {
            return Ok(());
        }
        // Bounded subdivision keeps Rapier within its documented dt range.
        if !dt.is_finite() || dt <= 0. || dt > 1. {
            return Err(invalid("Physics frame delta must be in (0,1] seconds"));
        }
        let substeps = (dt / 0.016667).ceil() as usize;
        let h = dt / substeps as f32;
        let Self {
            worlds,
            bodies,
            settings,
            features,
            ..
        } = self;
        for _ in 0..substeps {
            for feature in features.iter_mut() {
                feature.before_substep(context!(worlds, bodies, settings), h)?;
            }
            worlds.two.step(h).map_err(mapped)?;
            worlds.three.step(h).map_err(mapped)?;
            for feature in features.iter_mut() {
                feature.after_substep(context!(worlds, bodies, settings), world, h)?;
            }
        }
        bodies.write_back(worlds, world)?;
        for feature in features.iter_mut() {
            feature.after_step(context!(worlds, bodies, settings), world)?;
        }
        Ok(())
    }
}
/// The installed physics service, for native API functions.
fn service(world: &World) -> Result<&Physics> {
    world
        .get_resource::<Services>()
        .map(|services| &services.physics)
        .ok_or_else(|| invalid("Physics services are disabled"))
}
fn service_mut(world: &mut World) -> Result<Mut<'_, Physics>> {
    world
        .get_resource_mut::<Services>()
        .map(|services| services.map_unchanged(|services| &mut services.physics))
        .ok_or_else(|| invalid("Physics services are disabled"))
}
