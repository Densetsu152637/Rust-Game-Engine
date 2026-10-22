//! Native replacement for the former `moving_entity.lua` sample.
//!
//! The script is attached to an authored owner. On creation it spawns one
//! engine-owned entity at the origin and logs a start message; every update
//! advances that entity along +X by the frame delta (one unit per second); on
//! destruction it releases the entity. Y and Z are never touched.
use engine_core::bevy_ecs::prelude::{Entity, World};
use engine_project::{
    NativeScript, Transform, register_script,
    runtime::{RuntimeLog, spawn_owned},
};

/// Authored script asset ID that the project manifest binds to this crate.
pub const SCRIPT_ASSET: &str = "asset:moving-entity";
/// Message logged when the script starts, matching the Lua original.
pub const STARTED_MESSAGE: &str = "moving_entity started";

/// Register the script with a runtime; called by the native generation.
pub fn configure(app: &mut engine_core::EngineApp) -> engine_core::Result<()> {
    register_script(app, SCRIPT_ASSET, |_, _| {
        Ok(Box::new(MovingEntity::default()))
    })?;
    Ok(())
}

#[derive(Default)]
pub struct MovingEntity {
    entity: Option<Entity>,
}
impl MovingEntity {
    /// The entity created by `on_create`, if still tracked.
    pub fn entity(&self) -> Option<Entity> {
        self.entity
    }
}
impl NativeScript for MovingEntity {
    fn on_create(&mut self, world: &mut World, owner: Entity) -> engine_project::Result<()> {
        self.entity = Some(spawn_owned(world, owner, Transform::default())?);
        world
            .resource_mut::<RuntimeLog>()
            .0
            .push(STARTED_MESSAGE.into());
        Ok(())
    }
    fn on_update(
        &mut self,
        world: &mut World,
        _owner: Entity,
        delta_seconds: f32,
    ) -> engine_project::Result<()> {
        let Some(entity) = self.entity else {
            return Ok(());
        };
        // A dead handle is ignored, as the Lua script skipped dead entities.
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.position[0] += delta_seconds;
        }
        Ok(())
    }
    fn on_destroy(&mut self, world: &mut World, _owner: Entity) -> engine_project::Result<()> {
        if let Some(entity) = self.entity.take()
            && world.get_entity(entity).is_ok()
        {
            world.despawn(entity);
        }
        Ok(())
    }
}
