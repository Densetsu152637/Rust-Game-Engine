//! Authored pose graphs evaluated in the animation stage before physics and skinning.
use super::*;
use engine_animation::{
    LocalPose,
    graph::{Graph, GraphPlayer},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnimationGraphPlayer {
    pub graph: String,
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
                let mut player = GraphPlayer::new(graph, rest, clips).map_err(mapped)?;
                player.set_parameters(&config.parameters).map_err(mapped)?;
                // Apply authored parameters at time zero without advancing any clip clock.
                player.update(0.).map_err(mapped)?;
                self.players.insert(
                    entity.id(),
                    Player {
                        config: config.clone(),
                        targets,
                        player,
                    },
                );
            } else if let Some(player) = self.players.get_mut(&entity.id()) {
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
