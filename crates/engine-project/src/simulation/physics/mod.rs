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
use std::collections::BTreeSet;
mod api;
mod bodies;
mod character;
mod contacts;
mod debug_view;
mod deformables;
mod descriptors;
mod destruction;
mod joints;
mod pressure;
mod queries;
mod ragdoll;
mod request;
mod settings;
mod stepping;
mod types;
mod vehicles;
pub use api::*;
pub(crate) use bodies::Bodies;
pub use character::{
    AutostepSettings, CharacterController2D, CharacterController3D, CharacterReadout,
    character_impulse, character_jump, character_state, set_character_velocity,
};
pub use debug_view::*;
pub use deformables::{
    Cloth, ClothAttachment, ClothEdge, ClothRender, ClothSource, EmitterShape, FluidEmitter,
    FluidRegion, FluidRender, FluidVolume, MAX_CLOTHS, MAX_FLUID_CAPACITY, MAX_FLUID_VOLUMES,
    MAX_SOFT_BODIES, PhysicsRenderData, RegionShape, SoftBody, SoftBodyRender, SoftBodySource,
    deformable_debug_segments,
};
pub use destruction::{
    DebrisFragment, DebrisSettings, Destructible3D, DestructibleMesh, DestructibleStatus,
    FracturePattern, FractureReport, Fractured, FragmentInfo, FragmentRender, FragmentRenderer,
    RecyclePolicy, ThresholdMode, debris_count, destructible_status, fracture_now, fragment_mesh,
    fragments, preview_fracture, set_fracture_threshold,
};
pub use joints::{
    AxisKind, AxisSetting, BrokenJoint, JointKind, JointMotor, JointReadout, JointStatus,
    PhysicsJoint2D, PhysicsJoint3D, add_joint_2d, add_joint_3d, broken_joints, clear_joint_motor,
    joint_state, joint_status, remove_joint, set_joint_break_limits, set_joint_motor,
};
pub use pressure::{PressureStatus, pressure_status};
pub use queries::*;
pub use ragdoll::{
    BoneJoint, BoneReadout, BoneShape, HitReaction, MAX_BONES, Ragdoll, RagdollBone, RagdollMode,
    RagdollStatus, add_ragdoll, ragdoll_blend_out, ragdoll_bone_impulse, ragdoll_bone_state,
    ragdoll_from_hierarchy, ragdoll_hit, ragdoll_joint_state, ragdoll_set_blend, ragdoll_set_mode,
    ragdoll_set_strength, ragdoll_status,
};
pub use request::{
    MAX_QUERY_LIMIT, MAX_QUERY_TICKS, QueryRequest, debug_scene, query_scene, run_query,
};
pub(crate) use settings::Resolved;
pub use settings::{
    FIXED_TIMESTEP_RANGE, MAX_DISABLED_PAIRS, MAX_FIXED_SUBSTEPS, MAX_MATERIALS, NamedMaterial,
    PhysicsSettings,
};
pub use stepping::{Interpolation, StepStatus, interpolation_alpha, step_status};
pub use types::*;
pub use vehicles::{
    AntiRollSettings, CastKind, CurveSettings, DifferentialKind, DifferentialSettings,
    EngineSettings, GearMode, GearboxSettings, InputMapping, SteeringSettings, TyreSettings,
    Vehicle3D, VehicleReadout, WheelReadout, WheelSettings, set_vehicle_controls, vehicle_set_gear,
    vehicle_shift_down, vehicle_shift_up, vehicle_state, vehicle_wheel_pose, vehicle_wheels,
};

/// Physics features in execution order. Add one entry per feature.
fn features() -> Vec<Box<dyn Feature>> {
    vec![
        Box::new(contacts::Contacts),
        Box::new(joints::Joints::new()),
        Box::new(character::Characters::new()),
        Box::new(destruction::Destruction),
        Box::new(vehicles::Vehicles::new()),
        Box::new(ragdoll::Ragdolls::new()),
        Box::new(deformables::Deformables::new()),
    ]
}

/// The engine-physics worlds owned by the service. Body IDs are `Entity::to_bits()`.
pub(crate) struct Worlds {
    pub(crate) two: World2D,
    pub(crate) three: World3D,
    /// Entities that could not get engine-physics bodies because a world was full; they are
    /// retried every sync (see `pressure`).
    pub(crate) starved: BTreeSet<Entity>,
    /// Entities added to `starved` since the capacity warning last went out.
    pub(crate) starved_new: usize,
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
    /// Seconds of the current frame (the animation time since the last sync); 0 when the
    /// scene is only being prepared.
    pub(crate) frame_dt: f32,
    /// In `before_substep`: the fraction of the frame's steps done once this one has run
    /// (`1/n`, `2/n`, ... `1`). Elsewhere 1.
    pub(crate) progress: f32,
}
/// What a frame did, passed to [`Feature::after_frame`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    /// Blend factor between the previous and the latest step: the accumulator fraction when
    /// `interpolation` is on, 1 otherwise (the latest step).
    pub(crate) alpha: f32,
    /// Steps the frame ran (0 on frames shorter than the fixed step).
    #[allow(dead_code)] // Seam for feature leaves.
    pub(crate) steps: usize,
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
    /// True while the feature simulates something on its own (without rigid bodies), which
    /// keeps the step running in scenes that have no engine-physics bodies.
    fn active(&self) -> bool {
        false
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
    /// After dynamic poses were written back, on frames that ran at least one step.
    fn after_step(&mut self, _cx: Context<'_>, _world: &mut World) -> Result<()> {
        Ok(())
    }
    /// Every frame after the step loop and `after_step`, including frames without a step.
    /// Runs after interpolated body poses were written, so it may compose with them.
    fn after_frame(&mut self, _cx: Context<'_>, _world: &mut World, _frame: Frame) -> Result<()> {
        Ok(())
    }
}
pub(crate) use pressure::{emit, emit_named};

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
    stepper: stepping::Stepper,
    /// Delta of the last step, the fallback frame time when the app clock is not running.
    last_dt: f32,
}
/// Reborrows the service parts as a hook [`Context`].
macro_rules! context {
    ($worlds:expr, $bodies:expr, $settings:expr, $dt:expr, $progress:expr) => {
        Context {
            worlds: &mut *$worlds,
            bodies: &*$bodies,
            settings: &*$settings,
            frame_dt: $dt,
            progress: $progress,
        }
    };
}
impl Physics {
    pub fn new(config: SimulationConfig) -> Result<Self> {
        Ok(Self {
            worlds: Worlds {
                two: World2D::new(config.gravity_2d, config.body_capacity).map_err(mapped)?,
                three: World3D::new(config.gravity_3d, config.body_capacity).map_err(mapped)?,
                starved: BTreeSet::new(),
                starved_new: 0,
            },
            bodies: Bodies::default(),
            settings: Resolved::new(&config, None)?,
            features: features(),
            stepper: stepping::Stepper::default(),
            last_dt: 0.,
            config,
        })
    }
    /// Seconds of the frame being simulated: the app clock's delta, which the animation
    /// service also advanced by, or the previous step's delta outside a running app.
    fn frame_dt(&self, world: &World) -> f32 {
        world
            .get_resource::<engine_core::Time>()
            .map(|time| time.dt)
            .filter(|dt| dt.is_finite() && *dt > 0.)
            .unwrap_or(self.last_dt)
    }
    pub fn sync(&mut self, world: &World) -> Result<()> {
        let resolved = Resolved::new(&self.config, settings::find(world)?)?;
        // Debug and stepping policy changes must not wake bodies or rebuild them.
        if resolved.simulation_differs(&self.settings) {
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
                // Bodies are rebuilt from their authored data with the new groups/materials,
                // at their current pose and velocity. Ragdolls notice the change themselves.
                self.bodies.clear(worlds);
            }
        }
        self.settings = resolved;
        self.bodies.sync(&mut self.worlds, &self.settings, world)?;
        let frame_dt = self.frame_dt(world);
        let Self {
            worlds,
            bodies,
            settings,
            features,
            ..
        } = self;
        for feature in features {
            feature.sync(context!(worlds, bodies, settings, frame_dt, 1.), world)?;
        }
        Ok(())
    }
    pub fn step(&mut self, world: &mut World, dt: f32) -> Result<()> {
        // Features such as ragdolls own engine-physics bodies without authored body components.
        if self.bodies.is_empty()
            && self.worlds.two.is_empty()
            && self.worlds.three.is_empty()
            && !self.features.iter().any(|f| f.active())
        {
            return Ok(());
        }
        self.report_pressure(world)?;
        if let Some(step) = self.settings.fixed_timestep {
            return self.step_fixed(world, dt, step);
        }
        // Bounded subdivision keeps Rapier within its documented dt range.
        if !dt.is_finite() || dt <= 0. || dt > 1. {
            return Err(invalid("Physics frame delta must be in (0,1] seconds"));
        }
        self.last_dt = dt;
        let substeps = (dt / 0.016667).ceil() as usize;
        let h = dt / substeps as f32;
        let Self {
            worlds,
            bodies,
            settings,
            features,
            stepper,
            ..
        } = self;
        for k in 1..=substeps {
            substep(
                worlds,
                bodies,
                settings,
                features,
                world,
                h,
                (dt, k, substeps),
            )?;
        }
        stepper.variable(substeps);
        bodies.write_back(worlds, world)?;
        for feature in features.iter_mut() {
            feature.after_step(context!(worlds, bodies, settings, dt, 1.), world)?;
        }
        let frame = Frame {
            alpha: 1.,
            steps: substeps,
        };
        for feature in features.iter_mut() {
            feature.after_frame(context!(worlds, bodies, settings, dt, 1.), world, frame)?;
        }
        Ok(())
    }
    /// Accumulates `dt` and runs whole `step`-second steps, at most `max_substeps`.
    fn step_fixed(&mut self, world: &mut World, dt: f32, step: f32) -> Result<()> {
        if !dt.is_finite() || dt <= 0. || dt > stepping::MAX_FIXED_FRAME {
            return Err(invalid("Physics frame delta must be in (0,10] seconds"));
        }
        self.last_dt = dt;
        let Self {
            worlds,
            bodies,
            settings,
            features,
            stepper,
            ..
        } = self;
        let interpolate = settings.interpolation == Interpolation::Interpolate;
        let plan = stepper.plan(dt, step, settings.max_substeps);
        for k in 1..=plan.steps {
            if interpolate {
                stepper.capture(bodies, worlds);
            }
            substep(
                worlds,
                bodies,
                settings,
                features,
                world,
                step,
                (dt, k, plan.steps),
            )?;
        }
        if interpolate {
            stepper.write_interpolated(bodies, worlds, world)?;
        } else if plan.steps > 0 {
            bodies.write_back(worlds, world)?;
        }
        if plan.steps > 0 {
            for feature in features.iter_mut() {
                feature.after_step(context!(worlds, bodies, settings, dt, 1.), world)?;
            }
        }
        let frame = Frame {
            alpha: if interpolate {
                stepper.status.alpha
            } else {
                1.
            },
            steps: plan.steps,
        };
        for feature in features.iter_mut() {
            feature.after_frame(context!(worlds, bodies, settings, dt, 1.), world, frame)?;
        }
        stepper.report(world, &plan, settings.max_substeps)
    }
    /// Delivers the warnings about events dropped and bodies starved since the last step.
    fn report_pressure(&mut self, world: &mut World) -> Result<()> {
        pressure::report_events(world)?;
        if self.worlds.starved_new > 0
            && pressure::report_capacity(
                world,
                self.config.body_capacity,
                self.worlds.starved.len(),
                self.worlds.starved_new,
            )?
        {
            self.worlds.starved_new = 0;
        }
        Ok(())
    }
}
/// One engine-physics step of `h` seconds with the feature hooks around it. `position` is
/// `(frame delta, 1-based index of this step, steps in the frame)`; kinematic targets reach
/// `index / steps` of their frame motion in this step.
fn substep(
    worlds: &mut Worlds,
    bodies: &Bodies,
    settings: &Resolved,
    features: &mut [Box<dyn Feature>],
    world: &mut World,
    h: f32,
    position: (f32, usize, usize),
) -> Result<()> {
    let (dt, index, steps) = position;
    let progress = index as f32 / steps as f32;
    bodies.drive_kinematics(worlds, index, steps);
    for feature in features.iter_mut() {
        feature.before_substep(context!(worlds, bodies, settings, dt, progress), h)?;
    }
    worlds.two.step(h).map_err(mapped)?;
    worlds.three.step(h).map_err(mapped)?;
    for feature in features.iter_mut() {
        feature.after_substep(context!(worlds, bodies, settings, dt, 1.), world, h)?;
    }
    Ok(())
}
/// Delivers `physics.step.clamped` to the settings entity, if the scene has one.
fn emit_step_clamped(world: &mut World, status: &StepStatus, max_substeps: usize) -> Result<()> {
    let Some(target) = world
        .iter_entities()
        .find(|entity| entity.contains::<PhysicsSettings>())
        .map(|entity| entity.id())
    else {
        return Ok(());
    };
    let payload = serde_json::json!({
        "dropped_seconds": status.dropped_time,
        "max_substeps": max_substeps,
        "fixed_timestep": status.fixed_timestep,
        "clamped_frames": status.clamped_frames,
    });
    emit_named(world, "physics.step.clamped", target, None, payload)
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
