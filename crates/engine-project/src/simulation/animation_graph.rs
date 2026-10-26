//! Authored pose graphs evaluated in the animation stage before physics and skinning.
use super::*;
use engine_animation::{
    LocalPose, Skeleton,
    graph::{Graph, GraphPlayer},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnimationGraphPlayer {
    pub graph: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig: Option<String>,
    pub bindings: Vec<String>,
    pub parameters: BTreeMap<String, Value>,
}
pub(super) fn bindings(
    world: &World,
    owner: Entity,
    config: &AnimationGraphPlayer,
) -> Result<Vec<Entity>> {
    if config.parameters.len() > 64 {
        return Err(invalid("Animation graph parameter override limit exceeded"));
    }
    let is_remote = |entity| {
        world
            .get::<crate::networking::NetworkReplica>(entity)
            .is_some_and(|replica| replica.authority == crate::networking::ReplicaAuthority::Remote)
    };
    if is_remote(owner) {
        return Err(invalid("Remote replica cannot own an animation graph"));
    }
    let targets = animation::bindings(
        world,
        owner,
        &AnimationPlayer {
            bindings: config.bindings.clone(),
            ..Default::default()
        },
    )?;
    if targets.iter().any(|entity| is_remote(*entity)) {
        return Err(invalid(
            "Animation graph target already has remote network pose authority",
        ));
    }
    Ok(targets)
}
struct Player {
    config: AnimationGraphPlayer,
    targets: Vec<Entity>,
    player: GraphPlayer,
    rig: Option<RigBinding>,
}
#[derive(Default)]
pub(super) struct GraphAnimations {
    players: BTreeMap<Entity, Player>,
}
impl GraphAnimations {
    pub fn sync(&mut self, world: &mut World) -> Result<()> {
        self.players
            .retain(|entity, _| world.get::<AnimationGraphPlayer>(*entity).is_some());
        for entity in world.iter_entities() {
            let Some(config) = entity.get::<AnimationGraphPlayer>() else {
                continue;
            };
            let targets = bindings(world, entity.id(), config)?;
            let rebuild = self.players.get(&entity.id()).is_none_or(|p| {
                p.config.graph != config.graph
                    || p.config.rig != config.rig
                    || p.config.bindings != config.bindings
                    || p.targets != targets
            });
            if rebuild {
                let project = &world.resource::<crate::runtime::ProjectResource>().0;
                let document = crate::graphs::load_graph(project, &config.graph)?;
                let graph = Arc::new(Graph::compile(&document).map_err(|d| {
                    crate::graphs::into_error(d, std::path::Path::new(&config.graph))
                })?);
                let clips = graph
                    .clip_ids()
                    .map(|id| Ok((id.to_owned(), animation::load_clip(project, id)?)))
                    .collect::<Result<_>>()?;
                let rest = targets
                    .iter()
                    .map(|target| {
                        let transform = world
                            .get::<Transform>(*target)
                            .expect("validated graph binding");
                        LocalPose {
                            translation: Vec3::from(transform.position),
                            rotation: Quat::from_array(transform.rotation).normalize(),
                            scale: Vec3::from(transform.scale),
                        }
                    })
                    .collect();
                let rig = config
                    .rig
                    .as_deref()
                    .map(|id| RigBinding::load(project, id, world, &targets))
                    .transpose()?;
                let mut player = if let Some(rig) = &rig {
                    let skeleton = Skeleton::new(
                        rig.parents.clone(),
                        vec![Mat4::IDENTITY; targets.len()],
                        rest,
                    )
                    .map_err(mapped)?;
                    GraphPlayer::with_skeleton(graph, Arc::new(skeleton), clips, &config.parameters)
                        .map_err(mapped)?
                } else {
                    GraphPlayer::with_parameters(graph, rest, clips, &config.parameters)
                        .map_err(mapped)?
                };
                // Apply startup transitions at time zero without advancing any clip clock.
                player.update(0.).map_err(mapped)?;
                self.players.insert(
                    entity.id(),
                    Player {
                        config: config.clone(),
                        targets,
                        player,
                        rig,
                    },
                );
            } else if let Some(player) = self.players.get_mut(&entity.id()) {
                if let Some(rig) = &player.rig {
                    rig.validate(world, &targets)?;
                }
                player
                    .player
                    .set_parameters(&config.parameters)
                    .map_err(mapped)?;
                player.config = config.clone();
            }
        }
        // A graph's initial pose is authoritative on startup, including before first extraction.
        for player in self.players.values() {
            write_pose(world, &player.targets, player.player.pose())?;
        }
        Ok(())
    }
    pub fn step(&mut self, world: &mut World, dt: f32) -> Result<()> {
        for (owner, player) in &mut self.players {
            let events = player.player.update(dt).map_err(mapped)?;
            write_pose(world, &player.targets, player.player.pose())?;
            for event in events {
                emit(
                    world,
                    *owner,
                    None,
                    "animation.event",
                    serde_json::json!({"name":event.event.name,"time":event.event.time,"clip":event.clip,"graph":player.config.graph}),
                )?;
            }
        }
        Ok(())
    }
}
fn write_pose(world: &mut World, targets: &[Entity], pose: &[LocalPose]) -> Result<()> {
    for (target, pose) in targets.iter().zip(pose) {
        let mut transform = world
            .get_mut::<Transform>(*target)
            .ok_or_else(|| invalid("Animation graph target removed"))?;
        transform.position = pose.translation.to_array();
        transform.rotation = pose.rotation.to_array();
        transform.scale = pose.scale.to_array();
    }
    Ok(())
}

/// Indexed topology comes from the validated imported model, never guessed from bindings.
struct RigBinding {
    parents: Vec<Option<usize>>,
}
impl RigBinding {
    fn load(project: &crate::Project, id: &str, world: &World, targets: &[Entity]) -> Result<Self> {
        project.require_asset(id, "model")?;
        let model = crate::model_import::load_model(&project.resolve_asset(id)?)?;
        let skeleton = super::imported_skeleton(&model.scene)?;
        let rig = Self {
            parents: skeleton.parents().to_vec(),
        };
        rig.validate(world, targets)?;
        Ok(rig)
    }
    fn validate(&self, world: &World, targets: &[Entity]) -> Result<()> {
        if self.parents.len() != targets.len() {
            return Err(invalid(
                "IK rig requires bindings for every model node, including ancestors",
            ));
        }
        let ids = targets
            .iter()
            .map(|entity| {
                world
                    .get::<crate::AuthoredId>(*entity)
                    .map(|id| id.id.clone())
                    .ok_or_else(|| invalid("IK rig bindings require stable authored IDs"))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut anchor: Option<Option<String>> = None;
        for (i, entity) in targets.iter().enumerate() {
            let actual = world
                .get::<crate::Parent>(*entity)
                .map(|parent| parent.0.clone());
            if let Some(parent) = self.parents[i] {
                if actual.as_deref() != Some(ids[parent].as_str()) {
                    return Err(invalid(
                        "IK rig binding hierarchy differs from model; include every ancestor",
                    ));
                }
            } else {
                if actual.as_ref().is_some_and(|id| ids.contains(id)) {
                    return Err(invalid("IK rig root has an unexpected bound parent"));
                }
                if let Some(expected) = &anchor {
                    if *expected != actual {
                        return Err(invalid("IK rig roots must share one external anchor"));
                    }
                } else {
                    anchor = Some(actual);
                }
            }
        }
        let mut current = anchor.flatten();
        let mut seen = BTreeSet::new();
        while let Some(id) = current {
            if !seen.insert(id.clone()) || seen.len() > 256 {
                return Err(invalid("Invalid IK external anchor hierarchy"));
            }
            let entity = crate::services::find_entity(world, &id)
                .ok_or_else(|| invalid("Missing IK external anchor"))?;
            let transform = world
                .get::<Transform>(entity)
                .ok_or_else(|| invalid("IK external anchor requires Transform"))?;
            let scale = Vec3::from(transform.scale);
            if !scale.is_finite()
                || scale.min_element() <= 0.
                || scale.max_element() - scale.min_element() > scale.max_element() * 1e-5
            {
                return Err(invalid(
                    "IK external anchor requires positive uniform scale",
                ));
            }
            // Also validates quaternion, singularity and finite composition for excluded ancestors.
            crate::world_transform(world, entity)?;
            current = world
                .get::<crate::Parent>(entity)
                .map(|parent| parent.0.clone());
        }
        Ok(())
    }
}
