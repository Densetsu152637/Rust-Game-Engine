//! Local TRS authoring and resolved world-space transforms shared by consumers.
use crate::{Error, Result, Transform};
use bevy_ecs::prelude::*;
use glam::{Mat4, Quat, Vec3};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Component, Clone, Debug)]
pub struct Parent(pub String);
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct WorldTransform {
    pub matrix: [[f32; 4]; 4],
}
impl WorldTransform {
    pub fn position(self) -> [f32; 3] {
        self.transform_point([0.0; 3])
    }
    pub fn transform_point(self, point: [f32; 3]) -> [f32; 3] {
        Mat4::from_cols_array_2d(&self.matrix)
            .transform_point3(Vec3::from(point))
            .to_array()
    }
}
fn error(message: impl Into<String>) -> Error {
    Error::new("scene.transform.invalid", message)
}
pub fn local_matrix(t: &Transform) -> Result<Mat4> {
    let q = Quat::from_array(t.rotation);
    if t.position
        .iter()
        .chain(&t.rotation)
        .chain(&t.scale)
        .any(|n| !n.is_finite())
        || q.length_squared() < 1e-12
        || t.scale.iter().any(|n| n.abs() < 1e-8)
    {
        return Err(error(
            "Require finite TRS, nonzero quaternion and nonsingular scale",
        ));
    }
    Ok(Mat4::from_scale_rotation_translation(
        Vec3::from(t.scale),
        q.normalize(),
        Vec3::from(t.position),
    ))
}
fn resolve(
    world: &World,
    entity: Entity,
    visiting: &mut BTreeSet<Entity>,
    cache: &mut BTreeMap<Entity, Mat4>,
) -> Result<Mat4> {
    if let Some(matrix) = cache.get(&entity) {
        return Ok(*matrix);
    }
    if visiting.len() >= 256 || !visiting.insert(entity) {
        return Err(error("Parent cycle or hierarchy exceeds 256 levels"));
    }
    let local = world
        .get::<Transform>(entity)
        .ok_or_else(|| error("Entity requires Transform"))?;
    let mut matrix = local_matrix(local)?;
    if let Some(parent) = world.get::<Parent>(entity) {
        let parent = crate::services::find_entity(world, &parent.0)
            .ok_or_else(|| error("Parent does not exist"))?;
        matrix = resolve(world, parent, visiting, cache)? * matrix;
    }
    visiting.remove(&entity);
    if !matrix.is_finite() {
        return Err(error("World transform overflow"));
    }
    cache.insert(entity, matrix);
    Ok(matrix)
}
/// Resolves current local values, including mutations made earlier in a callback.
pub fn world_transform(world: &World, entity: Entity) -> Result<WorldTransform> {
    Ok(WorldTransform {
        matrix: resolve(world, entity, &mut BTreeSet::new(), &mut BTreeMap::new())?
            .to_cols_array_2d(),
    })
}
pub fn refresh_world_transforms(world: &mut World) -> Result<()> {
    let removed: Vec<_> = world
        .iter_entities()
        .filter(|e| e.contains::<WorldTransform>() && !e.contains::<Transform>())
        .map(|e| e.id())
        .collect();
    for entity in removed {
        world.entity_mut(entity).remove::<WorldTransform>();
    }
    let mut cache = BTreeMap::new();
    for entity in world.iter_entities().filter(|e| e.contains::<Transform>()) {
        resolve(world, entity.id(), &mut BTreeSet::new(), &mut cache)?;
    }
    for (entity, matrix) in cache {
        world.entity_mut(entity).insert(WorldTransform {
            matrix: matrix.to_cols_array_2d(),
        });
    }
    Ok(())
}
/// Resolves an authored scene without constructing a runtime (transition validation).
pub fn scene_world_transform(scene: &crate::Scene, id: &str) -> Result<WorldTransform> {
    let mut chain = Vec::new();
    let mut current = id;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current) || visited.len() > 256 {
            return Err(error("Parent cycle or hierarchy too deep"));
        }
        let entity = scene
            .entities
            .iter()
            .find(|e| e.id == current)
            .ok_or_else(|| error("Unknown entity"))?;
        chain.push(local_matrix(
            entity
                .components
                .transform
                .as_ref()
                .ok_or_else(|| error("Entity requires Transform"))?,
        )?);
        match &entity.parent {
            Some(parent) => current = parent,
            None => break,
        }
    }
    let mut matrix = Mat4::IDENTITY;
    for local in chain.into_iter().rev() {
        matrix *= local;
    }
    if !matrix.is_finite() {
        return Err(error("World transform overflow"));
    }
    Ok(WorldTransform {
        matrix: matrix.to_cols_array_2d(),
    })
}
pub fn set_world_position(world: &mut World, entity: Entity, position: [f32; 3]) -> Result<()> {
    let local = if let Some(parent) = world.get::<Parent>(entity) {
        let parent = crate::services::find_entity(world, &parent.0)
            .ok_or_else(|| error("Missing parent"))?;
        Mat4::from_cols_array_2d(&world_transform(world, parent)?.matrix)
            .inverse()
            .transform_point3(Vec3::from(position))
            .to_array()
    } else {
        position
    };
    if local.iter().any(|n| !n.is_finite()) {
        return Err(error("Invalid world position"));
    }
    world
        .get_mut::<Transform>(entity)
        .ok_or_else(|| error("Entity requires Transform"))?
        .position = local;
    Ok(())
}
/// An oriented local rectangle projects to a conservative world AABB for the 2D solver.
pub(crate) fn collider_body(
    matrix: WorldTransform,
    b: &crate::Collider2D,
) -> crate::gameplay2d::Body {
    let mut low = [f32::INFINITY; 2];
    let mut high = [f32::NEG_INFINITY; 2];
    for x in [-0.5, 0.5] {
        for y in [-0.5, 0.5] {
            let p = matrix.transform_point([
                b.offset[0] + x * b.size[0],
                b.offset[1] + y * b.size[1],
                0.0,
            ]);
            for axis in 0..2 {
                low[axis] = low[axis].min(p[axis]);
                high[axis] = high[axis].max(p[axis]);
            }
        }
    }
    let p = matrix.position();
    crate::gameplay2d::Body {
        position: [p[0], p[1]],
        size: [high[0] - low[0], high[1] - low[1]],
        offset: [
            (high[0] + low[0]) / 2.0 - p[0],
            (high[1] + low[1]) / 2.0 - p[1],
        ],
        trigger: b.trigger,
        layer: b.layer,
        mask: b.mask,
    }
}
