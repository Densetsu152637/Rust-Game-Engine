//! ECS adapters for the independent mixer. Entity removal drops scoped source voices.
use crate::{
    Error, Project, Result,
    runtime::{ProjectResource, TriggerEvents},
};
use bevy_ecs::prelude::*;
use engine_audio::{
    AudioControl, AudioSources, Clip, Listener, ObserverHandle, PlayOptions, SourceAction,
    SourceHandle,
};
use std::{collections::BTreeMap, sync::Arc};
fn audio_error(error: engine_audio::AudioError) -> Error {
    Error::new(error.code, error.message)
}
#[derive(Resource)]
pub struct RuntimeAudio {
    pub control: AudioControl,
    pub clips: BTreeMap<String, Arc<Clip>>,
    listener: Option<AttachedListener>,
}
struct AttachedListener {
    owner: Entity,
    attachment: Option<Entity>,
    value: Listener,
}
impl RuntimeAudio {
    pub fn new(control: AudioControl, project: &Project) -> Result<Self> {
        let mut clips = BTreeMap::new();
        for asset in project.assets.values().filter(|a| a.kind == "audio") {
            clips.insert(
                asset.id.clone(),
                engine_audio::load_wav(&project.resolve_asset(&asset.id)?).map_err(audio_error)?,
            );
        }
        Ok(Self {
            control,
            clips,
            listener: None,
        })
    }
}
#[derive(Component)]
pub struct EntityAudio {
    pub sources: AudioSources,
    attachments: BTreeMap<SourceHandle, (Entity, PlayOptions)>,
}
pub fn source(
    world: &mut World,
    owner: Entity,
    asset: &str,
    mut options: PlayOptions,
    attachment: Option<Entity>,
) -> Result<SourceHandle> {
    if world.get_entity(owner).is_err() {
        return Err(Error::new(
            "audio.owner.dead",
            "Audio owner no longer exists",
        ));
    }
    if let Some(entity) = attachment {
        options.spatial.position = crate::world_transform(world, entity)
            .map_err(|_| {
                Error::new(
                    "audio.attachment.invalid",
                    "Attachment requires a live transformed entity",
                )
            })?
            .position();
        options.spatial.enabled = true;
    }
    options.validate().map_err(audio_error)?;
    let audio = world
        .get_resource::<RuntimeAudio>()
        .ok_or_else(|| Error::new("audio.unavailable", "Host did not configure audio"))?;
    let clip = audio
        .clips
        .get(asset)
        .cloned()
        .ok_or_else(|| Error::new("audio.asset.unknown", asset))?;
    let control = audio.control.clone();
    if world.get::<EntityAudio>(owner).is_none() {
        world.entity_mut(owner).insert(EntityAudio {
            sources: AudioSources::new(control, format!("entity:{}", owner.to_bits()))
                .map_err(audio_error)?,
            attachments: BTreeMap::new(),
        });
    }
    let mut audio = world
        .get_mut::<EntityAudio>(owner)
        .expect("inserted entity audio");
    let handle = audio
        .sources
        .create(clip, options.clone())
        .map_err(audio_error)?;
    if let Some(entity) = attachment {
        audio.attachments.insert(handle, (entity, options));
    }
    Ok(handle)
}
pub fn configure(
    world: &mut World,
    owner: Entity,
    source: SourceHandle,
    mut options: PlayOptions,
    attachment: Option<Entity>,
) -> Result<()> {
    if let Some(entity) = attachment {
        options.spatial.position = crate::world_transform(world, entity)
            .map_err(|_| {
                Error::new(
                    "audio.attachment.invalid",
                    "Attachment requires a live transformed entity",
                )
            })?
            .position();
        options.spatial.enabled = true;
    }
    options.validate().map_err(audio_error)?;
    let mut audio = world
        .get_mut::<EntityAudio>(owner)
        .ok_or_else(|| Error::new("audio.source.missing", "Unknown audio owner"))?;
    audio
        .sources
        .configure(source, options.clone())
        .map_err(audio_error)?;
    if let Some(entity) = attachment {
        audio.attachments.insert(source, (entity, options));
    } else {
        audio.attachments.remove(&source);
    }
    Ok(())
}
pub fn control(
    world: &mut World,
    owner: Entity,
    source: SourceHandle,
    action: SourceAction,
) -> Result<()> {
    world
        .get_mut::<EntityAudio>(owner)
        .ok_or_else(|| Error::new("audio.owner.missing", "Unknown owner"))?
        .sources
        .control(source, action)
        .map_err(audio_error)
}
pub fn observe(
    world: &mut World,
    owner: Entity,
    event: impl Into<String>,
    source: SourceHandle,
    action: SourceAction,
) -> Result<ObserverHandle> {
    world
        .get_mut::<EntityAudio>(owner)
        .ok_or_else(|| Error::new("audio.owner.missing", "Unknown owner"))?
        .sources
        .observe(event, source, action)
        .map_err(audio_error)
}
pub fn notify(world: &mut World, owner: Entity, event: &str) -> Result<usize> {
    world
        .get_mut::<EntityAudio>(owner)
        .ok_or_else(|| Error::new("audio.owner.missing", "Unknown owner"))?
        .sources
        .notify(event)
        .map_err(audio_error)
}
pub fn listener(
    world: &mut World,
    owner: Entity,
    mut value: Listener,
    attachment: Option<Entity>,
) -> Result<()> {
    if world.get_entity(owner).is_err() {
        return Err(Error::new(
            "audio.owner.dead",
            "Listener owner no longer exists",
        ));
    }
    if let Some(entity) = attachment {
        value.position = crate::world_transform(world, entity)
            .map_err(|_| {
                Error::new(
                    "audio.attachment.invalid",
                    "Attachment requires a live transformed entity",
                )
            })?
            .position();
    }
    value.validate().map_err(audio_error)?;
    let mut audio = world
        .get_resource_mut::<RuntimeAudio>()
        .ok_or_else(|| Error::new("audio.unavailable", "Host did not configure audio"))?;
    audio.control.set_listener(value).map_err(audio_error)?;
    audio.listener = Some(AttachedListener {
        owner,
        attachment,
        value,
    });
    Ok(())
}
pub fn synchronize(world: &mut World) -> Result<()> {
    if !world.contains_resource::<RuntimeAudio>() {
        return Ok(());
    }
    let updates: Vec<_> = world
        .iter_entities()
        .filter_map(|e| {
            e.get::<EntityAudio>()
                .map(|a| (e.id(), a.attachments.clone()))
        })
        .collect();
    for (owner, attachments) in updates {
        for (source, (attachment, mut options)) in attachments {
            let position = crate::world_transform(world, attachment)
                .ok()
                .map(|t| t.position());
            let mut audio = world.get_mut::<EntityAudio>(owner).expect("queried owner");
            if let Some(position) = position {
                options.spatial.position = position;
                audio
                    .sources
                    .configure(source, options.clone())
                    .map_err(audio_error)?;
                audio.attachments.insert(source, (attachment, options));
            } else {
                audio.sources.remove(source);
                audio.attachments.remove(&source);
            }
        }
    }
    world.resource_scope(|world, mut audio: Mut<RuntimeAudio>| -> Result<()> {
        if let Some(attached) = audio.listener.take() {
            if world.get_entity(attached.owner).is_err() {
                audio.control.reset_listener().map_err(audio_error)?;
            } else {
                let mut value = attached.value;
                if let Some(entity) = attached.attachment {
                    if let Ok(t) = crate::world_transform(world, entity) {
                        value.position = t.position();
                    } else {
                        audio.control.reset_listener().map_err(audio_error)?;
                        return Ok(());
                    }
                }
                audio.control.set_listener(value).map_err(audio_error)?;
                audio.listener = Some(AttachedListener { value, ..attached });
            }
        }
        Ok(())
    })
}
pub fn notify_triggers(world: &mut World) -> Result<()> {
    let events = world.resource::<TriggerEvents>().0.clone();
    if events.is_empty() {
        return Ok(());
    }
    let owners: Vec<_> = world
        .iter_entities()
        .filter(|e| e.contains::<EntityAudio>())
        .map(|e| e.id())
        .collect();
    for event in events {
        let phase = match event.phase {
            crate::gameplay2d::TriggerPhase::Enter => "enter",
            crate::gameplay2d::TriggerPhase::Stay => "stay",
            crate::gameplay2d::TriggerPhase::Exit => "exit",
        };
        for owner in &owners {
            for id in [&event.first, &event.second] {
                notify(world, *owner, &format!("trigger:{id}:{phase}"))?;
            }
        }
    }
    Ok(())
}
pub fn shutdown(world: &mut World) -> Result<()> {
    let owners: Vec<_> = world
        .iter_entities()
        .filter(|e| e.contains::<EntityAudio>())
        .map(|e| e.id())
        .collect();
    for owner in owners {
        world.entity_mut(owner).remove::<EntityAudio>();
    }
    if let Some(mut audio) = world.get_resource_mut::<RuntimeAudio>() {
        audio.listener = None;
        audio.control.reset_listener().map_err(audio_error)?;
    }
    Ok(())
}
/// Load a declared audio asset through the confined project catalog.
pub fn load_clip(world: &World, id: &str) -> Result<Arc<Clip>> {
    world
        .resource::<ProjectResource>()
        .0
        .require_asset(id, "audio")?;
    world
        .get_resource::<RuntimeAudio>()
        .and_then(|a| a.clips.get(id).cloned())
        .ok_or_else(|| Error::new("audio.asset.unknown", id))
}
