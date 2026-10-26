//! ECS adapters for the independent mixer. Entity removal drops scoped source voices.
use crate::acoustics::{AcousticSource, AcousticUnits, AudioEmitter, AudioListener};
use crate::{
    Error, Project, Result,
    runtime::{ProjectResource, TriggerEvents},
};
use bevy_ecs::prelude::*;
use engine_audio::{
    AudioControl, AudioSources, Clip, Listener, ObserverHandle, PlayOptions, SourceAction,
    SourceHandle, VoiceHandle, VoiceState,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
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
    /// Forward and up follow the attachment's world rotation (authored listener).
    oriented: bool,
}
impl RuntimeAudio {
    pub fn new(control: AudioControl, project: &Project) -> Result<Self> {
        // Resolve catalog paths now, but never fully decode an unused/streamed asset.
        for asset in project.assets.values().filter(|a| a.kind == "audio") {
            project.resolve_asset(&asset.id)?;
        }
        let clips = BTreeMap::new();
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
    /// Every source created through this module, attached or not.
    handles: BTreeSet<SourceHandle>,
}
/// Marks an entity whose `engine.AudioEmitter` already created its source.
#[derive(Component, Clone, Copy, Debug)]
pub struct AuthoredEmitter {
    pub source: SourceHandle,
}

/// While an enabled `engine.AcousticEnvironment` exists, an `engine.AcousticSource`
/// on the owner dictates the voice's acoustic options; otherwise the native
/// options are used as given.
fn merge_acoustics(world: &World, owner: Entity, options: &mut PlayOptions) {
    if let (Some(units), Some(source)) = (
        world.get_resource::<AcousticUnits>(),
        world.get::<AcousticSource>(owner),
    ) {
        options.acoustics = source.voice_acoustics(units.0);
    }
}
pub fn source(
    world: &mut World,
    owner: Entity,
    asset: &str,
    options: PlayOptions,
    attachment: Option<Entity>,
) -> Result<SourceHandle> {
    source_prepared(
        world,
        owner,
        AuthoredSource::Resident(asset),
        options,
        attachment,
    )
}
enum AuthoredSource<'a> {
    Resident(&'a str),
    Stream(&'a str, usize),
    Graph(&'a str, engine_audio::ProceduralSourceConfig),
}
fn source_prepared(
    world: &mut World,
    owner: Entity,
    kind: AuthoredSource<'_>,
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
    merge_acoustics(world, owner, &mut options);
    options.validate().map_err(audio_error)?;
    let control = world
        .get_resource::<RuntimeAudio>()
        .ok_or_else(|| Error::new("audio.unavailable", "Host did not configure audio"))?
        .control
        .clone();
    enum Ready {
        Resident(Arc<Clip>),
        Stream(std::path::PathBuf, usize),
        Graph(
            Arc<engine_audio::graph::CompiledGraph>,
            engine_audio::ProceduralSourceConfig,
        ),
    }
    let prepared = match kind {
        AuthoredSource::Resident(asset) => {
            let cached = world.resource::<RuntimeAudio>().clips.get(asset).cloned();
            let clip = match cached {
                Some(clip) => clip,
                None => load_clip(world, asset).map_err(|e| {
                    if e.diagnostics.first().is_some_and(|d| {
                        matches!(
                            d.code.as_str(),
                            "project.asset.unknown" | "project.asset.kind"
                        )
                    }) {
                        Error::new("audio.asset.unknown", asset)
                    } else {
                        e
                    }
                })?,
            };
            let retained: usize = world
                .resource::<RuntimeAudio>()
                .clips
                .iter()
                .filter(|(id, _)| id.as_str() != asset)
                .fold(0usize, |bytes, (_, clip)| {
                    bytes.saturating_add(clip.samples().len() * std::mem::size_of::<f32>())
                });
            if retained.saturating_add(clip.samples().len() * std::mem::size_of::<f32>())
                > 128 * 1024 * 1024
            {
                return Err(Error::new(
                    "audio.resident.cache_limit",
                    "Resident cache exceeds 128 MiB",
                ));
            }
            world
                .resource_mut::<RuntimeAudio>()
                .clips
                .insert(asset.into(), clip.clone());
            Ready::Resident(clip)
        }
        AuthoredSource::Stream(asset, capacity) => {
            let project = &world.resource::<ProjectResource>().0;
            project.require_asset(asset, "audio")?;
            Ready::Stream(project.resolve_asset(asset)?, capacity)
        }
        AuthoredSource::Graph(asset, config) => {
            let project = &world.resource::<ProjectResource>().0;
            project.require_asset(asset, "graph")?;
            let path = project.resolve_asset(asset)?;
            let bytes =
                crate::schema::read_bounded(&path, engine_graph::MAX_DOCUMENT_BYTES as u64)?;
            let document = engine_graph::GraphDocument::from_json(&bytes)
                .map_err(|e| Error::new(&e.code, e.message))?;
            let program = engine_audio::graph::compile(&document)
                .map_err(|e| crate::graphs::into_error(e, &path))?;
            Ready::Graph(program, config)
        }
    };
    if world.get::<EntityAudio>(owner).is_none() {
        world.entity_mut(owner).insert(EntityAudio {
            sources: AudioSources::new(control, format!("entity:{}", owner.to_bits()))
                .map_err(audio_error)?,
            attachments: BTreeMap::new(),
            handles: BTreeSet::new(),
        });
    }
    let mut audio = world
        .get_mut::<EntityAudio>(owner)
        .expect("inserted entity audio");
    let handle = match prepared {
        Ready::Resident(clip) => audio.sources.create(clip, options.clone()),
        Ready::Stream(path, capacity) => audio.sources.create_stream(
            path,
            options.clone(),
            engine_audio::SourceStreamConfig {
                queue_frames: capacity,
                ..Default::default()
            },
        ),
        Ready::Graph(program, config) => {
            audio
                .sources
                .create_procedural(program, options.clone(), config)
        }
    }
    .map_err(audio_error)?;
    audio.handles.insert(handle);
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
    merge_acoustics(world, owner, &mut options);
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
        oriented: false,
    });
    Ok(())
}
/// Listener pose of an entity: world position, forward (-Z) and up (+Y) rotated
/// by the entity's world transform.
fn entity_listener(world: &World, entity: Entity) -> Result<Listener> {
    let matrix = glam::Mat4::from_cols_array_2d(
        &crate::world_transform(world, entity)
            .map_err(|_| {
                Error::new(
                    "audio.attachment.invalid",
                    "Listener requires a live transformed entity",
                )
            })?
            .matrix,
    );
    let direction = |axis: glam::Vec3| matrix.transform_vector3(axis).normalize_or_zero();
    Ok(Listener {
        position: matrix.w_axis.truncate().to_array(),
        forward: direction(glam::Vec3::NEG_Z).to_array(),
        up: direction(glam::Vec3::Y).to_array(),
    })
}
/// Make `entity`'s world pose the listener; it follows position and rotation each tick.
pub fn listener_from_entity(world: &mut World, entity: Entity) -> Result<()> {
    let value = entity_listener(world, entity)?;
    value.validate().map_err(audio_error)?;
    let mut audio = world
        .get_resource_mut::<RuntimeAudio>()
        .ok_or_else(|| Error::new("audio.unavailable", "Host did not configure audio"))?;
    audio.control.set_listener(value).map_err(audio_error)?;
    audio.listener = Some(AttachedListener {
        owner: entity,
        attachment: Some(entity),
        value,
        oriented: true,
    });
    Ok(())
}
/// Create the sources and listener that `engine.AudioEmitter` and
/// `engine.AudioListener` components author. Idempotent: emitters that already
/// have a source and an existing listener are left alone. A no-op without audio.
pub fn instantiate_authored(world: &mut World) -> Result<()> {
    if !world.contains_resource::<RuntimeAudio>() {
        return Ok(());
    }
    let key = |world: &World, entity: Entity| {
        world
            .get::<crate::runtime::AuthoredId>(entity)
            .map_or_else(|| entity.to_bits().to_string(), |a| a.id.clone())
    };
    let mut emitters: Vec<_> = world
        .iter_entities()
        .filter(|e| e.contains::<AudioEmitter>() && !e.contains::<AuthoredEmitter>())
        .map(|e| e.id())
        .collect();
    emitters.sort_by_key(|e| key(world, *e));
    for entity in emitters {
        let emitter = world
            .get::<AudioEmitter>(entity)
            .expect("filtered emitter")
            .clone();
        let owner = format!("emitter:{}", key(world, entity));
        let attachment = emitter.spatial.is_some().then_some(entity);
        emitter.validate()?;
        if emitter.clip.is_empty() && emitter.graph.is_none() {
            return Err(Error::new(
                "audio.emitter.source",
                "Emitter requires a clip or graph asset",
            ));
        }
        let kind = if let Some(graph) = emitter.graph.as_deref() {
            AuthoredSource::Graph(
                graph,
                engine_audio::ProceduralSourceConfig {
                    queue_frames: emitter.queue_frames,
                    duration_frames: emitter.duration_frames,
                    parameters: emitter.parameters.clone(),
                },
            )
        } else if emitter.streamed {
            AuthoredSource::Stream(&emitter.clip, emitter.queue_frames)
        } else {
            AuthoredSource::Resident(&emitter.clip)
        };
        let handle = source_prepared(
            world,
            entity,
            kind,
            emitter.play_options(&owner),
            attachment,
        )?;
        for (event, action) in [
            (&emitter.play_on, SourceAction::Play),
            (&emitter.stop_on, SourceAction::Stop),
        ] {
            if let Some(event) = event {
                observe(world, entity, event.clone(), handle, action)?;
            }
        }
        if emitter.autoplay {
            control(world, entity, handle, SourceAction::Play)?;
        }
        world
            .entity_mut(entity)
            .insert(AuthoredEmitter { source: handle });
    }
    if world.resource::<RuntimeAudio>().listener.is_none() {
        let mut listeners: Vec<_> = world
            .iter_entities()
            .filter(|e| e.get::<AudioListener>().is_some_and(|l| l.active))
            .map(|e| e.id())
            .collect();
        listeners.sort_by_key(|e| key(world, *e));
        if let Some(entity) = listeners.first() {
            listener_from_entity(world, *entity)?;
        }
    }
    Ok(())
}
/// A playing spatial voice with acoustics enabled, for the propagation service.
pub(crate) struct AcousticVoice {
    pub owner: Entity,
    pub voice: VoiceHandle,
    /// World position in mixer units.
    pub position: [f32; 3],
    /// The owner's `engine.AcousticSource`, if any.
    pub component: Option<AcousticSource>,
}
pub(crate) fn acoustic_voices(world: &World) -> Vec<AcousticVoice> {
    let Some(audio) = world.get_resource::<RuntimeAudio>() else {
        return vec![];
    };
    let mut voices = vec![];
    for entity in world.iter_entities() {
        let Some(owned) = entity.get::<EntityAudio>() else {
            continue;
        };
        for handle in &owned.handles {
            let (Some(options), Some(voice)) =
                (owned.sources.options(*handle), owned.sources.voice(*handle))
            else {
                continue;
            };
            if options.acoustics.enabled
                && options.spatial.enabled
                && audio.control.voice_state(voice) != VoiceState::Stopped
            {
                voices.push(AcousticVoice {
                    owner: entity.id(),
                    voice,
                    position: options.spatial.position,
                    component: entity.get::<AcousticSource>().cloned(),
                });
            }
        }
    }
    voices.sort_by_key(|v| v.voice);
    voices
}
/// The mixer's current listener, for the propagation service.
pub(crate) fn current_listener(world: &World) -> Option<Listener> {
    world
        .get_resource::<RuntimeAudio>()?
        .control
        .listener()
        .ok()
}
pub fn synchronize(world: &mut World) -> Result<()> {
    if !world.contains_resource::<RuntimeAudio>() {
        return Ok(());
    }
    let owners: Vec<_> = world
        .iter_entities()
        .filter(|e| e.contains::<EntityAudio>())
        .map(|e| e.id())
        .collect();
    for owner in owners {
        if let Some(mut audio) = world.get_mut::<EntityAudio>(owner) {
            audio.sources.refresh();
            if let Some(failure) = audio
                .handles
                .iter()
                .find_map(|handle| audio.sources.failure(*handle))
            {
                return Err(audio_error(failure));
            }
        }
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
            if position.is_some() {
                merge_acoustics(world, owner, &mut options);
            }
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
                audio.handles.remove(&source);
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
                    if attached.oriented {
                        if let Ok(pose) = entity_listener(world, entity) {
                            value = pose;
                        } else {
                            audio.control.reset_listener().map_err(audio_error)?;
                            return Ok(());
                        }
                    } else if let Ok(t) = crate::world_transform(world, entity) {
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
/// Play accepted 3D physics contact transitions at their post-step attached positions.
pub(crate) fn notify_physics_contacts(world: &mut World) -> Result<()> {
    if !world.contains_resource::<RuntimeAudio>() {
        return Ok(());
    }
    for (target, name) in crate::events::pending_audio_contacts(world) {
        if world.get::<EntityAudio>(target).is_some() {
            notify(world, target, &name)?;
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
    let project = &world.resource::<ProjectResource>().0;
    project.require_asset(id, "audio")?;
    let audio = world
        .get_resource::<RuntimeAudio>()
        .ok_or_else(|| Error::new("audio.asset.unknown", id))?;
    if let Some(clip) = audio.clips.get(id) {
        return Ok(clip.clone());
    }
    engine_audio::load_resident(
        project.resolve_asset(id)?,
        engine_audio::DecodeLimits::default(),
        64 * 1024 * 1024,
    )
    .map_err(audio_error)
}
/// Native streamed file source, sharing regular handles, events, attachments and acoustics.
pub fn stream_source(
    world: &mut World,
    owner: Entity,
    asset: &str,
    options: PlayOptions,
    attachment: Option<Entity>,
    queue_frames: usize,
) -> Result<SourceHandle> {
    source_prepared(
        world,
        owner,
        AuthoredSource::Stream(asset, queue_frames),
        options,
        attachment,
    )
}
pub fn procedural_source(
    world: &mut World,
    owner: Entity,
    asset: &str,
    options: PlayOptions,
    attachment: Option<Entity>,
    config: engine_audio::ProceduralSourceConfig,
) -> Result<SourceHandle> {
    source_prepared(
        world,
        owner,
        AuthoredSource::Graph(asset, config),
        options,
        attachment,
    )
}
pub fn seek_source(
    world: &mut World,
    owner: Entity,
    source: SourceHandle,
    frame: u64,
) -> Result<engine_audio::StreamGeneration> {
    world
        .get_mut::<EntityAudio>(owner)
        .ok_or_else(|| Error::new("audio.owner.missing", "Unknown owner"))?
        .sources
        .seek(source, frame)
        .map_err(audio_error)
}
pub fn set_parameters(
    world: &mut World,
    owner: Entity,
    source: SourceHandle,
    values: &[engine_audio::graph::ParameterUpdate<'_>],
) -> Result<()> {
    world
        .get_mut::<EntityAudio>(owner)
        .ok_or_else(|| Error::new("audio.owner.missing", "Unknown owner"))?
        .sources
        .set_parameters(source, values)
        .map_err(audio_error)
}
