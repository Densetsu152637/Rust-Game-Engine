use super::*;
use crate::{
    Transform,
    rendering3d::{Camera3D, Projection3D},
    simulation::{self, physics},
};
use engine_sequencer::{CompiledTimeline, Value};
use std::collections::BTreeMap;

pub(super) fn resolve(
    world: &World,
    owner: Entity,
    config: &SequencePlayer,
) -> Result<Vec<Entity>> {
    config.validate()?;
    if crate::networking::is_remote_replica(world, owner) {
        return Err(invalid("Remote replica cannot own a sequence"));
    }
    if config.bindings.is_empty() {
        return Ok(vec![owner]);
    }
    config
        .bindings
        .iter()
        .map(|id| {
            crate::services::find_entity(world, id)
                .ok_or_else(|| invalid(format!("Missing sequence binding {id}")))
        })
        .collect()
}
/// A local ancestor move also changes the world pose of its descendants. Reject that
/// when simulation or retained skinning already consumed/will own those poses.
fn affects(world: &World, target: Entity, mut descendant: Entity) -> Result<bool> {
    let mut visited = BTreeSet::new();
    loop {
        if descendant == target {
            return Ok(true);
        }
        if !visited.insert(descendant) || visited.len() > 256 {
            return Err(invalid("Sequence target hierarchy is cyclic or too deep"));
        }
        let Some(parent) = world.get::<crate::Parent>(descendant) else {
            return Ok(false);
        };
        descendant = crate::services::find_entity(world, &parent.0)
            .ok_or_else(|| invalid("Sequence hierarchy parent missing"))?;
    }
}
fn bound_pose(world: &World, target: Entity, owner: Entity, bindings: &[String]) -> Result<bool> {
    if bindings.is_empty() {
        return affects(world, target, owner);
    }
    for id in bindings {
        let binding = crate::services::find_entity(world, id)
            .ok_or_else(|| invalid("Existing pose writer binding missing"))?;
        if affects(world, target, binding)? {
            return Ok(true);
        }
    }
    Ok(false)
}
/// Reject known writers and their ancestors, including cross-entity binding dependencies.
pub(super) fn pose_authority(world: &World, target: Entity) -> Result<()> {
    world
        .get_entity(target)
        .map_err(|_| invalid("Sequence target removed"))?;
    for other in world.iter_entities() {
        let physical = crate::networking::is_remote_replica(world, other.id())
            || other.contains::<simulation::PhysicsBody2D>()
            || other.contains::<simulation::PhysicsBody3D>()
            || other.contains::<simulation::NavigationAgent>()
            || other.contains::<simulation::CrowdAgent>()
            || other.contains::<physics::CharacterController2D>()
            || other.contains::<physics::CharacterController3D>()
            || other.contains::<physics::Vehicle3D>()
            || other.contains::<physics::Cloth>()
            || other.contains::<physics::SoftBody>();
        if physical && affects(world, target, other.id())? {
            return Err(invalid(
                "Sequence target/descendant has physics, navigation, or remote pose authority",
            ));
        }
        if let Some(animation) = other.get::<simulation::AnimationPlayer>() {
            if bound_pose(world, target, other.id(), &animation.bindings)? {
                return Err(invalid(
                    "Sequence target/descendant already has animation pose authority",
                ));
            }
        }
        if let Some(graph) = other.get::<simulation::AnimationGraphPlayer>() {
            if bound_pose(world, target, other.id(), &graph.bindings)? {
                return Err(invalid(
                    "Sequence target/descendant already has animation graph pose authority",
                ));
            }
        }
        if let Some(skin) = other.get::<simulation::SkinBinding>() {
            if affects(world, target, other.id())?
                || bound_pose(world, target, other.id(), &skin.bindings)?
            {
                return Err(invalid(
                    "Sequence target/descendant participates in retained skinning",
                ));
            }
        }
        if let Some(ragdoll) = other.get::<physics::Ragdoll>() {
            for bone in &ragdoll.bones {
                let entity = crate::services::find_entity(world, &bone.bone)
                    .ok_or_else(|| invalid("Existing ragdoll bone missing"))?;
                if affects(world, target, entity)? {
                    return Err(invalid("Sequence target/descendant has ragdoll authority"));
                }
            }
        }
    }
    Ok(())
}
pub(super) fn read(world: &World, target: Entity, property: Property) -> Result<Value> {
    match property {
        Property::Translation | Property::Rotation | Property::Scale => {
            pose_authority(world, target)?;
            let transform = world
                .get::<Transform>(target)
                .ok_or_else(|| invalid("Sequence transform target lacks Transform"))?;
            Ok(match property {
                Property::Translation => Value::Vector(transform.position),
                Property::Rotation => Value::Rotation(transform.rotation),
                _ => Value::Vector(transform.scale),
            })
        }
        Property::CameraFov => {
            if crate::networking::is_remote_replica(world, target) {
                return Err(invalid("Remote camera cannot be sequenced"));
            }
            match world.get::<Camera3D>(target).map(|c| &c.projection) {
                Some(Projection3D::Perspective { fov_y_radians, .. }) => {
                    Ok(Value::Scalar(*fov_y_radians))
                }
                _ => Err(invalid("Camera FOV target must be a perspective Camera3D")),
            }
        }
    }
}
pub(super) fn write(
    world: &mut World,
    target: Entity,
    property: Property,
    value: Value,
) -> Result<()> {
    // validate/read before callers mutate guarantees type and authority, including at native seek.
    read(world, target, property)?;
    match (property, value) {
        (Property::Translation, Value::Vector(v)) => {
            world
                .get_mut::<Transform>(target)
                .ok_or_else(|| invalid("Transform removed"))?
                .position = v
        }
        (Property::Rotation, Value::Rotation(v)) => {
            world
                .get_mut::<Transform>(target)
                .ok_or_else(|| invalid("Transform removed"))?
                .rotation = v
        }
        (Property::Scale, Value::Vector(v)) => {
            world
                .get_mut::<Transform>(target)
                .ok_or_else(|| invalid("Transform removed"))?
                .scale = v
        }
        (Property::CameraFov, Value::Scalar(v)) => {
            let mut camera = world
                .get_mut::<Camera3D>(target)
                .ok_or_else(|| invalid("Camera removed"))?;
            if let Projection3D::Perspective { fov_y_radians, .. } = &mut camera.projection {
                *fov_y_radians = v;
            }
        }
        _ => return Err(invalid("Sequence sample type mismatch")),
    }
    Ok(())
}
#[derive(Default)]
pub(super) struct Capture {
    fields: BTreeMap<(Entity, Property), (Value, Value)>,
    cameras: BTreeMap<Entity, (bool, bool)>,
}
impl Capture {
    pub fn new(world: &World, targets: &[Entity], timeline: &CompiledTimeline) -> Result<Self> {
        let mut result = Self::default();
        for track in &timeline.document().tracks {
            let (binding, property) = track.target();
            let target = targets[binding];
            let value = read(world, target, property)?;
            result.fields.insert((target, property), (value, value));
        }
        if !timeline.document().camera_cuts.is_empty() {
            for cut in &timeline.document().camera_cuts {
                if world.get::<Camera3D>(targets[cut.binding]).is_none() {
                    return Err(invalid("Camera cut target lacks Camera3D"));
                }
            }
            for entity in world.iter_entities() {
                if let Some(camera) = entity.get::<Camera3D>() {
                    if crate::networking::is_remote_replica(world, entity.id()) {
                        return Err(invalid(
                            "Cannot acquire camera cut authority over a remote camera",
                        ));
                    }
                    result
                        .cameras
                        .insert(entity.id(), (camera.active, camera.active));
                }
            }
        }
        Ok(result)
    }
    pub fn validate(&self, world: &World) -> Result<()> {
        for ((target, property), (_, last)) in &self.fields {
            if read(world, *target, *property)? != *last {
                return Err(invalid("An external writer changed a sequence-owned field"));
            }
        }
        if !self.cameras.is_empty() {
            if self.cameras.iter().any(|(entity, (_, last))| {
                world
                    .get::<Camera3D>(*entity)
                    .is_some_and(|camera| camera.active != *last)
            }) {
                return Err(invalid(
                    "An external writer changed sequence camera activation",
                ));
            }
            let cameras: BTreeSet<_> = world
                .iter_entities()
                .filter(|e| e.contains::<Camera3D>())
                .map(|e| e.id())
                .collect();
            if cameras != self.cameras.keys().copied().collect() {
                return Err(invalid(
                    "Camera set changed during sequence; stop and reacquire",
                ));
            }
            if cameras
                .iter()
                .any(|e| crate::networking::is_remote_replica(world, *e))
            {
                return Err(invalid("Sequence lost camera authority to remote replica"));
            }
        }
        Ok(())
    }
    pub fn apply(&mut self, world: &mut World, targets: &[Entity], player: &Player) -> Result<()> {
        self.validate(world)?;
        let samples = player
            .timeline()
            .sample(player.position())
            .map_err(mapped)?;
        for sample in samples {
            let target = targets[sample.binding];
            write(world, target, sample.property, sample.value)?;
            if let Some((_, last)) = self.fields.get_mut(&(target, sample.property)) {
                *last = sample.value;
            }
        }
        let selected = player
            .timeline()
            .camera_at(player.position())
            .map(|binding| targets[binding]);
        for (entity, (before, last)) in &mut self.cameras {
            let value = selected.map_or(*before, |selected| selected == *entity);
            if let Some(mut camera) = world.get_mut::<Camera3D>(*entity) {
                camera.active = value;
                *last = value;
            }
        }
        Ok(())
    }
    pub fn restore(self, world: &mut World) {
        // Do not overwrite a new owner or externally changed value during cancellation.
        for ((target, property), (before, last)) in self.fields {
            if read(world, target, property).is_ok_and(|value| value == last) {
                let _ = write(world, target, property, before);
            }
        }
        for (entity, (before, last)) in self.cameras {
            if !crate::networking::is_remote_replica(world, entity) {
                if let Some(mut camera) = world.get_mut::<Camera3D>(entity) {
                    if camera.active == last {
                        camera.active = before;
                    }
                }
            }
        }
    }
}
