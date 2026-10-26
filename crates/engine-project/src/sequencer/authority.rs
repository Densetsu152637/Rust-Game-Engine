use super::origin::RootTranslation;
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
/// World-space physics/network/navigation services retain authority over descendant poses.
/// Skeletal animation writes local poses and can compose beneath a cinematic anchor.
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
        return Ok(target == owner);
    }
    for id in bindings {
        let binding = crate::services::find_entity(world, id)
            .ok_or_else(|| invalid("Existing pose writer binding missing"))?;
        if target == binding {
            return Ok(true);
        }
    }
    Ok(false)
}
/// Reject overlapping local pose writers; animated-rig anchors and skin readers can compose.
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
                    "Sequence target already has animation pose authority",
                ));
            }
        }
        if let Some(graph) = other.get::<simulation::AnimationGraphPlayer>() {
            if bound_pose(world, target, other.id(), &graph.bindings)? {
                return Err(invalid(
                    "Sequence target already has animation graph pose authority",
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
fn parent(world: &World, target: Entity) -> Result<Option<Entity>> {
    world
        .get::<crate::Parent>(target)
        .map(|parent| {
            crate::services::find_entity(world, &parent.0)
                .ok_or_else(|| invalid("Sequence parent no longer exists"))
        })
        .transpose()
}
#[derive(Default)]
pub(super) struct Capture {
    fields: BTreeMap<(Entity, Property), (Value, Value)>,
    cameras: BTreeMap<Entity, (bool, bool)>,
    parents: BTreeMap<Entity, Option<Entity>>,
    translations: BTreeMap<Entity, RootTranslation>,
    key_origin: Option<[f64; 3]>,
}
impl Capture {
    pub fn new(
        world: &mut World,
        targets: &[Entity],
        timeline: &CompiledTimeline,
        key_origin: Option<[f64; 3]>,
    ) -> Result<Self> {
        let mut result = Self {
            key_origin,
            ..Self::default()
        };
        for track in &timeline.document().tracks {
            let (binding, property) = track.target();
            let target = targets[binding];
            let value = read(world, target, property)?;
            if property != Property::CameraFov {
                let parent = parent(world, target)?;
                result.parents.insert(target, parent);
                if property == Property::Translation
                    && parent.is_none()
                    && let Value::Vector(baseline) = value
                {
                    let current = crate::world::coordinates::capture_origin(world);
                    let key = *result.key_origin.get_or_insert(current);
                    let baseline =
                        crate::world::coordinates::capture_translation(world, target, baseline)?;
                    result
                        .translations
                        .insert(target, RootTranslation::new(key, current, baseline)?);
                }
            }
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
    pub fn key_origin(&self) -> Option<[f64; 3]> {
        self.key_origin
    }
    pub fn origin_shifted(&mut self, from: [f64; 3], to: [f64; 3], roots: &RootRebases) {
        for (target, frame) in &mut self.translations {
            let Some(actual) = roots.get(target) else {
                frame.invalidate();
                continue;
            };
            if let Some((before, last)) = self.fields.get_mut(&(*target, Property::Translation))
                && let Some(shifted) = frame.shifted(from, to, actual, *last)
            {
                *before = shifted.0;
                *last = shifted.1;
            }
        }
    }
    pub fn validate(&self, world: &World) -> Result<()> {
        for (target, captured) in &self.parents {
            if parent(world, *target)? != *captured {
                return Err(invalid(
                    "Sequence target parent changed; stop and reacquire",
                ));
            }
        }
        if self
            .translations
            .iter()
            .any(|(target, frame)| !frame.valid() || !frame.component_owned(world, *target))
        {
            return Err(invalid(
                "Sequence translation lost ownership during origin shift",
            ));
        }
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
            let value = if sample.property == Property::Translation
                && let Value::Vector(value) = sample.value
                && let Some(frame) = self.translations.get_mut(&target)
            {
                let projected = frame.sample(value)?;
                let value = Value::Vector(projected.local);
                frame.write(world, target, projected)?;
                value
            } else {
                write(world, target, sample.property, sample.value)?;
                sample.value
            };
            if let Some((_, last)) = self.fields.get_mut(&(target, sample.property)) {
                *last = value;
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
    pub fn restore(mut self, world: &mut World) {
        // Do not overwrite a new owner or externally changed value during cancellation.
        for ((target, property), (before, last)) in self.fields {
            let parent_unchanged = self.parents.get(&target).is_none_or(|captured| {
                parent(world, target).is_ok_and(|current| current == *captured)
            });
            let origin_owned = self
                .translations
                .get(&target)
                .is_none_or(|frame| frame.valid() && frame.component_owned(world, target));
            if parent_unchanged
                && (property != Property::Translation || origin_owned)
                && read(world, target, property).is_ok_and(|value| value == last)
            {
                if property == Property::Translation
                    && let Some(frame) = self.translations.get_mut(&target)
                {
                    let _ = frame.restore(world, target);
                } else {
                    let _ = write(world, target, property, before);
                }
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
