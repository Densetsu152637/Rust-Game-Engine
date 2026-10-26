//! The runtime system: execute dirty volumes and replace their output.
use super::component::{PcgMode, PcgVolume, load_graph};
use super::surfaces::{PcgNodes, PcgSurfaceRegistry};
use crate::runtime::{AuthoredId, EntityLookup, ProjectResource, RuntimeLog};
use crate::world::coordinates::WorldOrigin;
use crate::{Components, MeshRenderer, Project, Scene, SceneEntity, Transform};
use bevy_ecs::prelude::*;
use engine_pcg::{Aabb, InstanceKind, NodeReport, RunSettings};
use std::collections::{BTreeMap, BTreeSet};

/// Hard ceiling of entities one volume may own (instances plus prefab children).
const MAX_ENTITIES: usize = 262_144;

/// Bookkeeping on a volume entity: the values last generated and the entities that resulted.
#[derive(Component, Clone, Debug)]
pub struct PcgState {
    /// Volume values at the last generation attempt (success or failure).
    pub applied: PcgVolume,
    /// Authored ids of the entities currently owned by the volume.
    pub entities: Vec<String>,
}

/// What the last generation attempt of one volume did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PcgReport {
    /// Number of attempts so far (successful or not).
    pub attempts: u64,
    /// Instances in the current output.
    pub instances: usize,
    /// Entities in the current output (prefab instances count their children).
    pub entities: usize,
    /// Per-node counts of the last successful run.
    pub nodes: Vec<NodeReport>,
    /// Empty on success; otherwise the diagnostics of the failed attempt. A failed attempt
    /// leaves the previous output in place.
    pub diagnostics: Vec<String>,
}

/// Output ids owned by each volume entity, kept outside the volume so the output can be swept
/// after the volume entity (and its [`PcgState`]) is gone, for example when a streamed cell
/// unloads.
#[derive(Resource, Clone, Debug, Default)]
pub(super) struct PcgOwned(BTreeMap<Entity, (String, Vec<String>)>);

/// Despawns the output of every volume whose entity no longer exists or lost its `PcgVolume`.
fn sweep_vanished(world: &mut World) {
    let vanished: Vec<Entity> = world
        .resource::<PcgOwned>()
        .0
        .keys()
        .copied()
        .filter(|e| {
            world
                .get_entity(*e)
                .map_or(true, |r| !r.contains::<PcgVolume>())
        })
        .collect();
    for volume in vanished {
        let Some((id, ids)) = world.resource_mut::<PcgOwned>().0.remove(&volume) else {
            continue;
        };
        for old in &ids {
            let removed = world.resource_mut::<EntityLookup>().0.remove(old);
            if let Some(entity) = removed {
                world.despawn(entity);
            }
        }
        world.resource_mut::<PcgStatus>().0.remove(&id);
    }
}

/// Per-volume reports keyed by the volume's authored id.
#[derive(Resource, Clone, Debug, Default)]
pub struct PcgStatus(pub BTreeMap<String, PcgReport>);

struct Built {
    entities: Vec<SceneEntity>,
    instances: usize,
    nodes: Vec<NodeReport>,
}

pub(super) fn run(world: &mut World) {
    sweep_vanished(world);
    let mut jobs: Vec<(Entity, PcgVolume, String)> = {
        let mut query =
            world.query::<(Entity, &PcgVolume, Option<&PcgState>, Option<&AuthoredId>)>();
        query
            .iter(world)
            .filter_map(|(entity, volume, state, id)| {
                let owned = state.is_some_and(|s| !s.entities.is_empty());
                let dirty = match state {
                    Some(state) => state.applied != *volume,
                    None => volume.mode == PcgMode::Runtime,
                };
                let wanted = match volume.mode {
                    PcgMode::Runtime => dirty,
                    PcgMode::Baked => dirty && owned,
                };
                wanted.then(|| {
                    let id = id.map_or_else(|| format!("{entity:?}"), |a| a.id.clone());
                    (entity, volume.clone(), id)
                })
            })
            .collect()
    };
    jobs.sort_by(|a, b| a.2.cmp(&b.2));
    for (entity, volume, id) in jobs {
        apply(world, entity, volume, &id);
    }
}

fn apply(world: &mut World, volume_entity: Entity, volume: PcgVolume, id: &str) {
    let previous = world
        .get::<PcgState>(volume_entity)
        .map(|s| s.entities.clone())
        .unwrap_or_default();
    let attempts = world
        .resource::<PcgStatus>()
        .0
        .get(id)
        .map_or(0, |r| r.attempts)
        + 1;
    let outcome = if volume.mode == PcgMode::Baked || volume.graph.is_empty() {
        Ok(Built {
            entities: Vec::new(),
            instances: 0,
            nodes: Vec::new(),
        })
    } else {
        build(world, volume_entity, &volume, id)
    };
    // Generated ids must not shadow entities the volume does not own: spawning would silently
    // overwrite their `EntityLookup` entry.
    let outcome = outcome.and_then(|built| {
        let lookup = &world.resource::<EntityLookup>().0;
        let owned: BTreeSet<&String> = previous.iter().collect();
        let clashes: Vec<String> = built
            .entities
            .iter()
            .filter(|e| lookup.contains_key(&e.id) && !owned.contains(&e.id))
            .map(|e| {
                format!(
                    "pcg.entity.id_collision: generated entity id `{}` already exists",
                    e.id
                )
            })
            .collect();
        if clashes.is_empty() {
            Ok(built)
        } else {
            Err(clashes)
        }
    });
    match outcome {
        Ok(built) => {
            for old in &previous {
                let removed = world.resource_mut::<EntityLookup>().0.remove(old);
                if let Some(entity) = removed {
                    world.despawn(entity);
                }
            }
            let ids: Vec<String> = built.entities.iter().map(|e| e.id.clone()).collect();
            for authored in &built.entities {
                crate::runtime::spawn(world, authored);
            }
            if built
                .entities
                .iter()
                .any(|e| !e.components.custom.is_empty())
            {
                let catalog = world
                    .resource::<ProjectResource>()
                    .0
                    .component_catalog
                    .clone();
                let scene = Scene {
                    schema: 1,
                    id: format!("pcg:{id}"),
                    entities: built.entities.clone(),
                    source: std::path::PathBuf::new(),
                };
                if let Err(error) = crate::catalog::hydrate(world, &scene, &catalog) {
                    world
                        .resource_mut::<RuntimeLog>()
                        .0
                        .push(format!("pcg {id}: component hydration failed: {error}"));
                }
            }
            world.resource_mut::<PcgStatus>().0.insert(
                id.to_owned(),
                PcgReport {
                    attempts,
                    instances: built.instances,
                    entities: ids.len(),
                    nodes: built.nodes,
                    diagnostics: Vec::new(),
                },
            );
            world
                .resource_mut::<PcgOwned>()
                .0
                .insert(volume_entity, (id.to_owned(), ids.clone()));
            world.entity_mut(volume_entity).insert(PcgState {
                applied: volume,
                entities: ids,
            });
        }
        Err(diagnostics) => {
            for line in &diagnostics {
                world
                    .resource_mut::<RuntimeLog>()
                    .0
                    .push(format!("pcg {id}: {line}"));
            }
            let mut status = world.resource_mut::<PcgStatus>();
            let report = status.0.entry(id.to_owned()).or_default();
            report.attempts = attempts;
            report.diagnostics = diagnostics;
            world.entity_mut(volume_entity).insert(PcgState {
                applied: volume,
                entities: previous,
            });
        }
    }
}

fn build(
    world: &World,
    volume_entity: Entity,
    volume: &PcgVolume,
    id: &str,
) -> Result<Built, Vec<String>> {
    let one = |message: String| vec![message];
    volume.check().map_err(|e| one(e.to_string()))?;
    let project = world.resource::<ProjectResource>().0.clone();
    let graph = load_graph(&project, &volume.graph).map_err(|e| one(e.to_string()))?;
    let surface = world
        .resource::<PcgSurfaceRegistry>()
        .get(&volume.surface)
        .ok_or_else(|| {
            one(format!(
                "pcg.surface.unknown: no surface `{}` is registered",
                volume.surface
            ))
        })?;
    let position = crate::transforms::world_transform(world, volume_entity)
        .map(|t| t.position())
        .or_else(|_| {
            world
                .get::<Transform>(volume_entity)
                .map(|t| t.position)
                .ok_or(())
        })
        .map_err(|()| one("volume entity has no Transform".into()))?;
    let half = volume.size.map(|v| v * 0.5);
    let centre: [f32; 3] = std::array::from_fn(|i| position[i] + volume.center[i]);
    let bounds = Aabb::new(
        std::array::from_fn(|i| centre[i] - half[i]),
        std::array::from_fn(|i| centre[i] + half[i]),
    );
    let mut settings = RunSettings::new(graph.seed_for(volume.seed), bounds);
    settings.domain_offset = world
        .get_resource::<WorldOrigin>()
        .map_or([0.0; 3], |o| o.offset);
    settings.limits.max_instances = volume.max_instances as usize;
    let registry = &world.resource::<PcgNodes>().0;
    let result = engine_pcg::run(&graph, registry, &*surface, &settings).map_err(|e| {
        e.diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    })?;

    let mut problems = Vec::new();
    let mut checked = BTreeSet::new();
    for instance in &result.instances {
        let kind = match instance.kind {
            InstanceKind::Mesh => "mesh",
            InstanceKind::Prefab => "prefab",
        };
        if checked.insert((kind, instance.asset.as_str()))
            && let Err(error) = project.require_asset(&instance.asset, kind)
        {
            problems.push(format!("pcg.asset.invalid: {error}"));
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let entities = instance_entities(&project, id, &result.instances).map_err(one)?;
    Ok(Built {
        entities,
        instances: result.instances.len(),
        nodes: result.nodes,
    })
}

/// Scene entities for `instances`: one per mesh instance, an anchor plus the prefab's entities
/// per prefab instance. Ids are `pcg:<volume>:<n>` (prefab children append their prefab ids).
pub fn instance_entities(
    project: &Project,
    volume_id: &str,
    instances: &[engine_pcg::Instance],
) -> Result<Vec<SceneEntity>, String> {
    let mut prefabs: BTreeMap<&str, Vec<SceneEntity>> = BTreeMap::new();
    let mut out: Vec<SceneEntity> = Vec::with_capacity(instances.len());
    for (n, instance) in instances.iter().enumerate() {
        let id = format!("pcg:{volume_id}:{n}");
        let transform = Transform {
            version: 1,
            position: instance.position,
            rotation: instance.rotation,
            scale: instance.scale,
        };
        match instance.kind {
            InstanceKind::Mesh => out.push(SceneEntity {
                id,
                name: instance.asset.clone(),
                parent: None,
                components: Components {
                    transform: Some(transform),
                    mesh_renderer: Some(MeshRenderer {
                        version: 1,
                        mesh: instance.asset.clone(),
                        texture: None,
                    }),
                    ..Components::default()
                },
            }),
            InstanceKind::Prefab => {
                if !prefabs.contains_key(instance.asset.as_str()) {
                    let asset = project
                        .require_asset(&instance.asset, "prefab")
                        .map_err(|e| e.to_string())?;
                    let scene =
                        crate::load_scene(&project.root, &asset.path).map_err(|e| e.to_string())?;
                    prefabs.insert(instance.asset.as_str(), scene.entities);
                }
                let template = prefabs[instance.asset.as_str()].clone();
                out.push(SceneEntity {
                    id: id.clone(),
                    name: instance.asset.clone(),
                    parent: None,
                    components: Components {
                        transform: Some(transform),
                        ..Components::default()
                    },
                });
                out.extend(crate::document::prefab_entities(
                    project,
                    template,
                    &format!("{id}/"),
                    Some(&id),
                ));
            }
        }
        if out.len() > MAX_ENTITIES {
            return Err(format!(
                "pcg.limit.entities: generation would exceed {MAX_ENTITIES} entities"
            ));
        }
    }
    Ok(out)
}
