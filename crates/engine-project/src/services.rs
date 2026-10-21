//! Native script service functions borrow the same authoritative ECS world.
use crate::{
    Collider2D, Components, Error, Project, Result, Scene, SceneEntity, Script, Transform,
    gameplay2d::{self, Body, MoveResult},
    runtime::{
        AuthoredId, EntityLookup, PendingScene, ProjectResource, SceneRequest, ScriptRegistry,
    },
    schema::load_scene,
};
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::World;
use std::{collections::BTreeSet, path::PathBuf};
/// Recompute capture/lock state for same-callback reads after UI or control mutations.
pub fn gameplay_input(world: &World) -> crate::InputSnapshot {
    let mut input = world.resource::<crate::ui::UiModel>().gameplay_frame();
    if !world
        .resource::<crate::runtime::ControlLocks>()
        .0
        .is_empty()
    {
        input = crate::InputSnapshot {
            focused: input.focused,
            ..Default::default()
        };
    }
    input
}
/// Native schedules chain UI/control writers -> this exclusive gate -> gameplay readers.
pub fn refresh_gameplay_input(world: &mut World) {
    let input = gameplay_input(world);
    world.insert_resource(input);
}
pub fn find_entity(world: &World, id: &str) -> Option<Entity> {
    world
        .get_resource::<EntityLookup>()?
        .get(id)
        .filter(|e| world.get_entity(*e).is_ok())
}
pub fn resolve_asset(world: &World, id: &str) -> Result<PathBuf> {
    world
        .get_resource::<ProjectResource>()
        .ok_or_else(|| Error::new("runtime.project.unavailable", "No project resource"))?
        .0
        .resolve_asset(id)
}
pub fn sync_collision(world: &mut World) -> Result<()> {
    let bodies: Vec<_> = world
        .iter_entities()
        .filter_map(|e| {
            Some((
                e.get::<AuthoredId>()?.id.clone(),
                e.get::<Transform>()?.clone(),
                e.get::<Collider2D>()?.clone(),
            ))
        })
        .collect();
    let mut collision = world.resource_mut::<gameplay2d::World>();
    let ids: BTreeSet<_> = bodies.iter().map(|(id, _, _)| id.clone()).collect();
    let removed: Vec<_> = collision
        .ids()
        .filter(|id| !ids.contains(*id))
        .map(str::to_owned)
        .collect();
    for id in removed {
        collision.remove(&id);
    }
    for (id, t, b) in bodies {
        collision.upsert(
            id,
            Body {
                position: [t.position[0], t.position[1]],
                size: b.size,
                offset: b.offset,
                trigger: b.trigger,
                layer: b.layer,
                mask: b.mask,
            },
        )?;
    }
    Ok(())
}
pub fn move_entity(world: &mut World, id: &str, delta: [f32; 2]) -> Result<MoveResult> {
    sync_collision(world)?;
    let result = world
        .resource_mut::<gameplay2d::World>()
        .move_body(id, delta)?;
    let entity = find_entity(world, id)
        .ok_or_else(|| Error::new("runtime.entity.unknown", "Unknown entity"))?;
    let mut t = world.get_mut::<Transform>(entity).ok_or_else(|| {
        Error::new(
            "runtime.transform.missing",
            "Moving entity requires Transform",
        )
    })?;
    t.position[0] = result.position[0];
    t.position[1] = result.position[1];
    Ok(result)
}
pub fn safe_position(
    world: &mut World,
    id: &str,
    radius: f32,
    step: f32,
) -> Result<Option<[f32; 2]>> {
    sync_collision(world)?;
    let collision = world.resource::<gameplay2d::World>();
    let body = collision
        .body(id)
        .ok_or_else(|| Error::new("collision.body.unknown", "Unknown body"))?;
    collision.find_safe(body, radius, step, id)
}
pub fn snapshot_entity(world: &World, entity: Entity) -> Result<SceneEntity> {
    let e = world
        .get_entity(entity)
        .map_err(|_| Error::new("runtime.entity.dead", "Entity no longer exists"))?;
    let id = e
        .get::<AuthoredId>()
        .ok_or_else(|| Error::new("runtime.entity.unauthored", "Entity has no authored ID"))?;
    Ok(SceneEntity {
        id: id.id.clone(),
        name: id.name.clone(),
        components: Components {
            transform: e.get::<Transform>().cloned(),
            script: e.get::<Script>().cloned(),
            mesh_renderer: e.get::<crate::MeshRenderer>().cloned(),
            sprite_renderer: e.get::<crate::SpriteRenderer>().cloned(),
            camera_2d: e.get::<crate::Camera2D>().cloned(),
            collider_2d: e.get::<Collider2D>().cloned(),
        },
    })
}
pub(crate) fn prepare_scene(
    world: &World,
    project: &Project,
    request: &SceneRequest,
) -> Result<(Scene, gameplay2d::World)> {
    let asset = project.require_asset(&request.asset, "scene")?;
    let mut scene = load_scene(&project.root, &asset.path)?;
    project.validate_scene(&scene)?;
    if !request.traveller.is_empty() {
        let traveller = find_entity(world, &request.traveller)
            .ok_or_else(|| Error::new("runtime.scene.traveller", "Traveller does not exist"))?;
        let old = snapshot_entity(world, traveller)?;
        if let Some(target) = scene
            .entities
            .iter_mut()
            .find(|e| e.id == request.traveller)
        {
            *target = old;
        } else {
            scene.entities.push(old);
        }
    }
    if !request.spawn.is_empty() {
        let position = scene
            .entities
            .iter()
            .find(|e| e.id == request.spawn)
            .and_then(|e| e.components.transform.as_ref())
            .map(|t| t.position)
            .ok_or_else(|| {
                Error::new(
                    "runtime.scene.spawn",
                    "Spawn must name a transformed destination entity",
                )
            })?;
        scene
            .entities
            .iter_mut()
            .find(|e| e.id == request.traveller)
            .and_then(|e| e.components.transform.as_mut())
            .ok_or_else(|| {
                Error::new(
                    "runtime.scene.traveller",
                    "Spawn requires a transformed traveller",
                )
            })?
            .position = position;
    }
    project.validate_scene(&scene)?;
    let registry = world.resource::<ScriptRegistry>();
    let mut collision = gameplay2d::World::default();
    for entity in &scene.entities {
        if let Some(script) = &entity.components.script
            && !registry.factories.contains_key(&script.asset)
        {
            return Err(Error::new(
                "script.asset.unregistered",
                format!("Unregistered native script {}", script.asset),
            ));
        }
        if let (Some(t), Some(c)) = (&entity.components.transform, &entity.components.collider_2d) {
            collision.upsert(
                entity.id.clone(),
                Body {
                    position: [t.position[0], t.position[1]],
                    size: c.size,
                    offset: c.offset,
                    trigger: c.trigger,
                    layer: c.layer,
                    mask: c.mask,
                },
            )?;
        }
    }
    if let Some(body) = collision.body(&request.traveller)
        && !collision.is_safe(body, &request.traveller)?
    {
        return Err(Error::new(
            "runtime.scene.spawn.blocked",
            "Traveller spawn overlaps solid body",
        ));
    }
    Ok((scene, collision))
}
/// Queue one validated transition. No authored scene mutation occurs until tick completion.
pub fn request_scene(world: &mut World, request: SceneRequest) -> Result<()> {
    let project = world.resource::<ProjectResource>().0.clone();
    prepare_scene(world, &project, &request)?;
    if world.resource::<PendingScene>().0.is_some() {
        return Err(Error::new(
            "runtime.scene.pending",
            "Scene transition is already pending",
        ));
    }
    world.resource_mut::<PendingScene>().0 = Some(request);
    Ok(())
}
