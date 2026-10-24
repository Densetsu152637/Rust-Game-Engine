use super::*;
use engine_animation::{
    Animator, Clip, ClipEvent, Interpolation, LocalPose, Playback, RotationTrack, Track,
    VectorTrack,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sampling {
    Step,
    #[default]
    Linear,
}
impl From<Sampling> for Interpolation {
    fn from(value: Sampling) -> Self {
        match value {
            Sampling::Step => Self::Step,
            Sampling::Linear => Self::Linear,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorChannel {
    pub times: Vec<f32>,
    pub values: Vec<[f32; 3]>,
    #[serde(default)]
    pub interpolation: Sampling,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotationChannel {
    pub times: Vec<f32>,
    pub values: Vec<[f32; 4]>,
    #[serde(default)]
    pub interpolation: Sampling,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipTrack {
    pub joint: usize,
    #[serde(default)]
    pub translation: Option<VectorChannel>,
    #[serde(default)]
    pub rotation: Option<RotationChannel>,
    #[serde(default)]
    pub scale: Option<VectorChannel>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub time: f32,
    pub name: String,
}
/// Versioned clip payload. Joint indices refer to the player's complete node binding list.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipDocument {
    pub version: u32,
    pub duration: f32,
    pub joints: usize,
    #[serde(default)]
    pub tracks: Vec<ClipTrack>,
    #[serde(default)]
    pub events: Vec<Event>,
}
fn vector(v: VectorChannel) -> Result<VectorTrack> {
    VectorTrack::new(
        v.times,
        v.values.into_iter().map(Vec3::from).collect(),
        v.interpolation.into(),
    )
    .map_err(mapped)
}
impl ClipDocument {
    pub fn into_clip(self) -> Result<Arc<Clip>> {
        if self.version != 1
            || self.joints > 4096
            || self.tracks.len() > 4096
            || self.events.len() > 4096
        {
            return Err(invalid("Unsupported animation version or capacity"));
        }
        let tracks = self
            .tracks
            .into_iter()
            .map(|t| {
                Ok(Track {
                    joint: t.joint,
                    translation: t.translation.map(vector).transpose()?,
                    rotation: t
                        .rotation
                        .map(|v| {
                            RotationTrack::new(
                                v.times,
                                v.values.into_iter().map(Quat::from_array).collect(),
                                v.interpolation.into(),
                            )
                            .map_err(mapped)
                        })
                        .transpose()?,
                    scale: t.scale.map(vector).transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Arc::new(
            Clip::new(
                self.duration,
                self.joints,
                tracks,
                self.events
                    .into_iter()
                    .map(|e| ClipEvent {
                        time: e.time,
                        name: e.name,
                    })
                    .collect(),
            )
            .map_err(mapped)?,
        ))
    }
}
pub fn load_clip(project: &crate::Project, id: &str) -> Result<Arc<Clip>> {
    project.require_asset(id, "animation")?;
    let path = project.resolve_asset(id)?;
    let document: ClipDocument =
        serde_json::from_slice(&crate::schema::read_bounded(&path, 16 * 1024 * 1024)?)?;
    document.into_clip()
}
pub(super) fn bindings(
    world: &World,
    owner: Entity,
    config: &AnimationPlayer,
) -> Result<Vec<Entity>> {
    if !config.speed.is_finite()
        || !(0.0..=100.).contains(&config.speed)
        || config.bindings.len() > 4096
    {
        return Err(invalid("Invalid animation playback limits"));
    }
    let targets = if config.bindings.is_empty() {
        vec![owner]
    } else {
        config
            .bindings
            .iter()
            .map(|id| {
                crate::services::find_entity(world, id)
                    .ok_or_else(|| invalid(format!("Missing animation binding {id}")))
            })
            .collect::<Result<Vec<_>>>()?
    };
    let mut unique = BTreeSet::new();
    for e in &targets {
        if !unique.insert(*e) || world.get::<Transform>(*e).is_none() {
            return Err(invalid(
                "Animation bindings must be unique entities with Transform",
            ));
        }
    }
    Ok(targets)
}
struct Player {
    config: AnimationPlayer,
    targets: Vec<Entity>,
    animator: Animator,
}
#[derive(Default)]
pub(super) struct Animations {
    players: BTreeMap<Entity, Player>,
}
impl Animations {
    pub fn sync(&mut self, world: &World) -> Result<()> {
        self.players
            .retain(|e, _| world.get::<AnimationPlayer>(*e).is_some());
        for entity in world.iter_entities() {
            if let Some(config) = entity.get::<AnimationPlayer>() {
                let targets = bindings(world, entity.id(), config)?;
                if self
                    .players
                    .get(&entity.id())
                    .is_none_or(|p| p.config != *config || p.targets != targets)
                {
                    let project = &world.resource::<crate::runtime::ProjectResource>().0;
                    let clip = load_clip(project, &config.clip)?;
                    if clip.joint_count() != targets.len() {
                        return Err(invalid("Clip joint count differs from animation bindings"));
                    }
                    let rest = targets
                        .iter()
                        .map(|e| {
                            let t = world.get::<Transform>(*e).expect("validated binding");
                            LocalPose {
                                translation: Vec3::from(t.position),
                                rotation: Quat::from_array(t.rotation).normalize(),
                                scale: Vec3::from(t.scale),
                            }
                        })
                        .collect();
                    let mut animator = Animator::new(rest).map_err(mapped)?;
                    animator
                        .play(
                            clip,
                            Playback {
                                looping: config.looping,
                                speed: config.speed,
                            },
                        )
                        .map_err(mapped)?;
                    self.players.insert(
                        entity.id(),
                        Player {
                            config: config.clone(),
                            targets,
                            animator,
                        },
                    );
                }
            }
        }
        Ok(())
    }
    pub fn step(&mut self, world: &mut World, dt: f32) -> Result<()> {
        for (owner, player) in &mut self.players {
            let events = player.animator.update(dt).map_err(mapped)?;
            for (e, pose) in player.targets.iter().zip(player.animator.pose()) {
                let mut transform = world
                    .get_mut::<Transform>(*e)
                    .ok_or_else(|| invalid("Animation target removed"))?;
                transform.position = pose.translation.to_array();
                transform.rotation = pose.rotation.to_array();
                transform.scale = pose.scale.to_array();
            }
            for event in events {
                emit(
                    world,
                    *owner,
                    None,
                    "animation.event",
                    serde_json::json!({"name":event.name,"time":event.time,"clip":player.config.clip}),
                )?;
            }
        }
        Ok(())
    }
}
