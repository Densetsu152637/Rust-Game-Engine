//! Video component and host-controlled local playback (`docs/video.md`).
use crate::{
    ComponentDescriptor, Error, Result,
    captions::{CaptionClock, Captions, PlayOptions, PlaybackId},
    rendering3d::{TextureStreams, valid_stream_name},
};
use bevy_ecs::prelude::*;
use engine_video::{DecoderTools, VideoPlayer};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub const VIDEO_PLAYER_COMPONENT: &str = "engine.VideoPlayer";
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct VideoPlayerComponent {
    pub asset: String,
    pub stream: String,
    pub captions: String,
    pub autoplay: bool,
}
impl Default for VideoPlayerComponent {
    fn default() -> Self {
        Self {
            asset: String::new(),
            stream: "video".into(),
            captions: String::new(),
            autoplay: true,
        }
    }
}
impl VideoPlayerComponent {
    pub fn validate(&self) -> Result<()> {
        if !valid_stream_name(&self.stream)
            || [&self.asset, &self.captions]
                .iter()
                .any(|s| s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(Error::new(
                "video.component",
                "Expected asset/caption IDs and a valid texture stream name",
            ));
        }
        Ok(())
    }
}
pub fn descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: VIDEO_PLAYER_COMPONENT.into(),
        version: 1,
        schema: serde_json::json!({"type":"object","additionalProperties":false,"properties":{"asset":{"type":"string","maxLength":256},"stream":{"type":"string","maxLength":64},"captions":{"type":"string","maxLength":256},"autoplay":{"type":"boolean"}}}),
        defaults: serde_json::json!({"asset":"","stream":"video","captions":"","autoplay":true}),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}
struct AttachedVideo {
    config: VideoPlayerComponent,
    player: VideoPlayer,
    caption: Option<PlaybackId>,
}
/// Host opt-in: insert this resource after configuring trusted executable paths.
/// At most eight attached players; entity removal closes processes and voices.
#[derive(Resource)]
pub struct RuntimeVideos {
    pub tools: DecoderTools,
    players: BTreeMap<Entity, AttachedVideo>,
}
impl RuntimeVideos {
    pub fn new(tools: DecoderTools) -> Self {
        Self {
            tools,
            players: BTreeMap::new(),
        }
    }
    pub fn player_mut(&mut self, entity: Entity) -> Option<&mut VideoPlayer> {
        self.players.get_mut(&entity).map(|p| &mut p.player)
    }
    pub fn player(&self, entity: Entity) -> Option<&VideoPlayer> {
        self.players.get(&entity).map(|p| &p.player)
    }
}
pub fn caption_clock(player: &VideoPlayer) -> CaptionClock {
    let clock = player.clock();
    Arc::new(move || clock.seconds())
}
pub(crate) fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<VideoPlayerComponent>(app, VIDEO_PLAYER_COMPONENT, 1)
}
fn failure(e: engine_video::VideoError) -> Error {
    Error::new("video.playback", e.to_string())
}
pub fn poll_texture(
    player: &mut VideoPlayer,
    streams: &mut TextureStreams,
    name: &str,
) -> Result<bool> {
    if let Some(frame) = player.poll().map_err(failure)? {
        streams.upload_rgba(name, frame.rgba)?;
        return Ok(true);
    }
    Ok(false)
}
fn attach_captions(
    world: &mut World,
    track: &str,
    player: &VideoPlayer,
) -> Result<Option<PlaybackId>> {
    if track.is_empty() {
        return Ok(None);
    }
    world
        .resource_mut::<Captions>()
        .play(
            track,
            PlayOptions {
                clock: Some(caption_clock(player)),
                ..Default::default()
            },
        )
        .map(Some)
}
pub(crate) fn tick(world: &mut World) -> Result<()> {
    let configs: BTreeMap<Entity, VideoPlayerComponent> = world
        .query::<(Entity, &VideoPlayerComponent)>()
        .iter(world)
        .map(|(e, c)| (e, c.clone()))
        .collect();
    if !world.contains_resource::<RuntimeVideos>() {
        if configs.values().any(|c| c.autoplay && !c.asset.is_empty()) {
            return Err(Error::new(
                "video.decoder.unconfigured",
                "Insert RuntimeVideos with configured FFmpeg/ffprobe paths before playing video",
            ));
        }
        return Ok(());
    }
    world.resource_scope(|world, mut videos: Mut<RuntimeVideos>| -> Result<()> {
        let removed: Vec<_> = videos
            .players
            .iter()
            .filter(|(e, p)| configs.get(e) != Some(&p.config))
            .map(|(e, _)| *e)
            .collect();
        for e in removed {
            if let Some(p) = videos.players.remove(&e) {
                world
                    .resource_mut::<TextureStreams>()
                    .remove(&p.config.stream);
                if let Some(id) = p.caption {
                    world.resource_mut::<Captions>().stop(id);
                }
            }
        }
        let mut names: BTreeSet<String> = videos
            .players
            .values()
            .map(|p| p.config.stream.clone())
            .collect();
        for (e, c) in &configs {
            c.validate()?;
            if videos.players.contains_key(e) || !c.autoplay || c.asset.is_empty() {
                continue;
            }
            if videos.players.len() >= 8
                || !names.insert(c.stream.clone())
                || world.resource::<TextureStreams>().get(&c.stream).is_some()
            {
                return Err(Error::new(
                    "video.player.limit",
                    "At most eight video players with distinct unoccupied texture streams",
                ));
            }
            let project = &world.resource::<crate::runtime::ProjectResource>().0;
            let asset = project
                .assets
                .get(&c.asset)
                .ok_or_else(|| Error::new("video.asset", "Unknown video asset"))?;
            if asset.kind != "video" {
                return Err(Error::new("video.asset", "Expected video asset kind"));
            }
            let path = project.resolve_asset(&c.asset)?;
            let audio = world
                .get_resource::<crate::audio::RuntimeAudio>()
                .map(|a| a.control.clone());
            let player = VideoPlayer::open(&path, videos.tools.clone(), audio).map_err(failure)?;
            world.resource_mut::<TextureStreams>().create_cpu(
                &c.stream,
                player.info().width,
                player.info().height,
            )?;
            let caption = match attach_captions(world, &c.captions, &player) {
                Ok(id) => id,
                Err(error) => {
                    world.resource_mut::<TextureStreams>().remove(&c.stream);
                    return Err(error);
                }
            };
            videos.players.insert(
                *e,
                AttachedVideo {
                    config: c.clone(),
                    player,
                    caption,
                },
            );
        }
        for p in videos.players.values_mut() {
            let result = poll_texture(
                &mut p.player,
                &mut world.resource_mut::<TextureStreams>(),
                &p.config.stream,
            );
            if (result.is_err()
                || matches!(
                    p.player.snapshot().state,
                    engine_video::PlaybackState::Stopped | engine_video::PlaybackState::Failed
                ))
                && let Some(id) = p.caption.take()
            {
                world.resource_mut::<Captions>().stop(id);
            }
            result?;
            if p.caption.is_none()
                && matches!(
                    p.player.snapshot().state,
                    engine_video::PlaybackState::Playing | engine_video::PlaybackState::Paused
                )
            {
                p.caption = attach_captions(world, &p.config.captions, &p.player)?;
            }
        }
        Ok(())
    })
}
pub(crate) fn shutdown(world: &mut World) {
    if let Some(videos) = world.remove_resource::<RuntimeVideos>() {
        for p in videos.players.values() {
            if let Some(mut streams) = world.get_resource_mut::<TextureStreams>() {
                streams.remove(&p.config.stream);
            }
            if let Some(id) = p.caption
                && let Some(mut captions) = world.get_resource_mut::<Captions>()
            {
                captions.stop(id);
            }
        }
    }
}
