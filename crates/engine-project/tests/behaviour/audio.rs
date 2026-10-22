//! Audio ownership: clips from the project catalogue, entity-scoped sources,
//! attachments that follow transforms, listeners, event observers and teardown.
use super::common::*;
use bevy_ecs::prelude::{Entity, World};
use engine_audio::{
    AudioControl, AudioEngine, Bus, Listener, PlayOptions, SourceAction, SourceHandle, VoiceState,
};
use engine_project::{
    ActionFrame, Runtime, Transform,
    audio::{self, EntityAudio, RuntimeAudio},
};
use serde_json::json;

struct Rig {
    // Declared first so the runtime drops before the engine it controls.
    rt: Runtime,
    control: AudioControl,
    _engine: AudioEngine,
    _project: Proj,
}

fn rig_from(p: Proj) -> Rig {
    let engine = AudioEngine::offline(Default::default()).unwrap();
    let control = engine.control();
    let mut rt = p.runtime();
    rt.set_audio(control.clone()).unwrap();
    Rig {
        rt,
        control,
        _engine: engine,
        _project: p,
    }
}

fn rig() -> Rig {
    rig_from(Proj::new())
}

impl Rig {
    fn world(&mut self) -> &mut World {
        self.rt.app_mut().world_mut()
    }
    fn entity(&self, id: &str) -> Entity {
        self.rt.entity(id).unwrap()
    }
    fn source(&mut self, owner: &str, attach: Option<&str>) -> (Entity, SourceHandle) {
        let owner = self.entity(owner);
        let attach = attach.map(|id| self.entity(id));
        let handle =
            audio::source(self.world(), owner, "sfx", PlayOptions::default(), attach).unwrap();
        (owner, handle)
    }
    fn act(&mut self, owner: Entity, handle: SourceHandle, action: SourceAction) {
        audio::control(self.world(), owner, handle, action).unwrap();
    }
    fn state(&mut self, owner: Entity, handle: SourceHandle) -> VoiceState {
        let voice = self
            .world()
            .get::<EntityAudio>(owner)
            .unwrap()
            .sources
            .voice(handle);
        voice.map_or(VoiceState::Stopped, |v| self.control.voice_state(v))
    }
}

// ---- clips --------------------------------------------------------------------------------

#[test]
fn audio_assets_are_decoded_when_audio_is_configured() {
    let mut r = rig();
    let clip = r.world().resource::<RuntimeAudio>().clips["sfx"].clone();
    assert_eq!(
        (clip.sample_rate(), clip.channels(), clip.frames()),
        (8000, 1, 800)
    );
}

#[test]
fn only_audio_kind_assets_become_clips() {
    let mut r = rig();
    let keys: Vec<_> = r
        .world()
        .resource::<RuntimeAudio>()
        .clips
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, vec!["sfx"]);
}

#[test]
fn stereo_clips_keep_their_channel_count() {
    let mut p = Proj::new();
    p.write("st.wav", wav(44100, 2, 100));
    p.add_asset("st", "audio", "st.wav");
    let mut r = rig_from(p);
    let clip = r.world().resource::<RuntimeAudio>().clips["st"].clone();
    assert_eq!((clip.channels(), clip.frames()), (2, 100));
}

#[test]
fn a_corrupt_wav_is_rejected_when_audio_is_configured() {
    let p = Proj::new();
    let project = p.project();
    p.write("sfx.wav", "not a wav");
    let engine = AudioEngine::offline(Default::default()).unwrap();
    let mut rt = Runtime::new(project).unwrap();
    expect_code(rt.set_audio(engine.control()), "audio.wav.invalid");
}

#[test]
fn a_deleted_wav_prevents_runtime_creation_with_a_missing_asset_error() {
    let p = Proj::new();
    let project = p.project();
    std::fs::remove_file(p.root().join("sfx.wav")).unwrap();
    expect_code(Runtime::new(project), "project.asset.missing");
}

#[test]
fn load_clip_returns_declared_audio_assets() {
    let mut r = rig();
    assert_eq!(audio::load_clip(r.world(), "sfx").unwrap().frames(), 800);
}

#[test]
fn load_clip_rejects_assets_of_other_kinds() {
    let mut r = rig();
    expect_code(audio::load_clip(r.world(), "texture"), "project.asset.kind");
}

#[test]
fn load_clip_rejects_unknown_assets() {
    let mut r = rig();
    expect_code(
        audio::load_clip(r.world(), "ghost"),
        "project.asset.unknown",
    );
}

#[test]
fn load_clip_without_configured_audio_reports_the_asset_unknown() {
    let mut rt = Proj::new().runtime();
    expect_code(
        audio::load_clip(rt.app_mut().world(), "sfx"),
        "audio.asset.unknown",
    );
}

// ---- sources ------------------------------------------------------------------------------------

#[test]
fn sources_need_configured_audio() {
    let mut rt = Proj::new().runtime();
    let owner = rt.entity("player").unwrap();
    let result = audio::source(
        rt.app_mut().world_mut(),
        owner,
        "sfx",
        PlayOptions::default(),
        None,
    );
    expect_code(result, "audio.unavailable");
}

#[test]
fn sources_need_a_known_clip() {
    let mut r = rig();
    let owner = r.entity("player");
    expect_code(
        audio::source(r.world(), owner, "ghost", PlayOptions::default(), None),
        "audio.asset.unknown",
    );
}

#[test]
fn sources_need_a_live_owner() {
    let mut r = rig();
    let owner = r.entity("player");
    r.world().despawn(owner);
    expect_code(
        audio::source(r.world(), owner, "sfx", PlayOptions::default(), None),
        "audio.owner.dead",
    );
}

fn bad_options(options: PlayOptions) {
    let mut r = rig();
    let owner = r.entity("player");
    expect_code(
        audio::source(r.world(), owner, "sfx", options, None),
        "audio.play.invalid",
    );
    assert!(r.world().get::<EntityAudio>(owner).is_none());
}

table! { bad_options;
    master_bus_is_not_a_valid_source_bus => (PlayOptions { bus: Bus::Master, ..Default::default() });
    zero_pitch_is_invalid => (PlayOptions { pitch: 0.0, ..Default::default() });
    pitch_above_eight_is_invalid => (PlayOptions { pitch: 8.5, ..Default::default() });
    nan_pitch_is_invalid => (PlayOptions { pitch: f32::NAN, ..Default::default() });
}

#[test]
fn creating_a_source_does_not_start_playback() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    assert_eq!(r.control.voice_count(), 0);
    assert_eq!(r.state(owner, handle), VoiceState::Stopped);
}

#[test]
fn play_pause_resume_and_stop_drive_one_voice() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    r.act(owner, handle, SourceAction::Play);
    assert_eq!(
        (r.control.voice_count(), r.state(owner, handle)),
        (1, VoiceState::Playing)
    );
    r.act(owner, handle, SourceAction::Pause);
    assert_eq!(r.state(owner, handle), VoiceState::Paused);
    r.act(owner, handle, SourceAction::Resume);
    assert_eq!(r.state(owner, handle), VoiceState::Playing);
    r.act(owner, handle, SourceAction::Stop);
    assert_eq!(
        (r.control.voice_count(), r.state(owner, handle)),
        (0, VoiceState::Stopped)
    );
}

#[test]
fn playing_again_restarts_the_same_source_without_stacking_voices() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    r.act(owner, handle, SourceAction::Play);
    r.act(owner, handle, SourceAction::Play);
    assert_eq!(r.control.voice_count(), 1);
}

#[test]
fn two_sources_on_one_owner_play_independently() {
    let mut r = rig();
    let (owner, a) = r.source("player", None);
    let (_, b) = r.source("player", None);
    r.act(owner, a, SourceAction::Play);
    r.act(owner, b, SourceAction::Play);
    r.act(owner, a, SourceAction::Stop);
    assert_eq!(r.control.voice_count(), 1);
    assert_eq!(r.state(owner, b), VoiceState::Playing);
}

#[test]
fn owners_have_separate_source_collections() {
    let mut r = rig();
    let (player, a) = r.source("player", None);
    let (camera, b) = r.source("camera", None);
    r.act(player, a, SourceAction::Play);
    assert_eq!(r.state(camera, b), VoiceState::Stopped);
    assert_eq!(r.control.voice_count(), 1);
}

#[test]
fn controlling_or_observing_without_audio_on_the_owner_is_an_error() {
    let mut r = rig();
    let owner = r.entity("player");
    expect_code(
        audio::control(r.world(), owner, foreign_handle(), SourceAction::Play),
        "audio.owner.missing",
    );
    expect_code(
        audio::notify(r.world(), owner, "ping"),
        "audio.owner.missing",
    );
    expect_code(
        audio::observe(
            r.world(),
            owner,
            "ping",
            foreign_handle(),
            SourceAction::Play,
        ),
        "audio.owner.missing",
    );
}

/// A handle that no owner knows: create and discard a source on a scratch owner.
fn foreign_handle() -> SourceHandle {
    let engine = AudioEngine::offline(Default::default()).unwrap();
    let clip = engine_audio::Clip::new(8000, 1, vec![0.0; 8]).unwrap();
    let mut sources = engine_audio::AudioSources::new(engine.control(), "scratch").unwrap();
    sources.create(clip, PlayOptions::default()).unwrap()
}

#[test]
fn controlling_an_unknown_source_on_a_known_owner_is_an_error() {
    let mut r = rig();
    let (owner, _) = r.source("player", None);
    let result = audio::control(r.world(), owner, foreign_handle(), SourceAction::Play);
    assert_eq!(code(result), "audio.source.missing");
}

#[test]
fn configure_changes_a_playing_voice_in_place() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    r.act(owner, handle, SourceAction::Play);
    audio::configure(
        r.world(),
        owner,
        handle,
        PlayOptions {
            gain: 0.25,
            pitch: 2.0,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let voice = r
        .world()
        .get::<EntityAudio>(owner)
        .unwrap()
        .sources
        .voice(handle)
        .unwrap();
    let snapshot = r.control.voice(voice).unwrap();
    assert_eq!((snapshot.options.gain, snapshot.options.pitch), (0.25, 2.0));
}

#[test]
fn configure_requires_audio_on_the_owner() {
    let mut r = rig();
    let owner = r.entity("player");
    expect_code(
        audio::configure(
            r.world(),
            owner,
            foreign_handle(),
            PlayOptions::default(),
            None,
        ),
        "audio.source.missing",
    );
}

#[test]
fn configure_validates_before_changing_anything() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    let bad = PlayOptions {
        pitch: 0.0,
        ..Default::default()
    };
    expect_code(
        audio::configure(r.world(), owner, handle, bad, None),
        "audio.play.invalid",
    );
    assert_eq!(
        r.world()
            .get::<EntityAudio>(owner)
            .unwrap()
            .sources
            .options(handle)
            .unwrap()
            .pitch,
        1.0
    );
}

// ---- attachments and listeners -------------------------------------------------------------------

fn at_player(x: f32) -> impl FnOnce(&mut Proj) {
    move |p| p.entity("player")["components"]["Transform"]["position"] = json!([x, 2, 0])
}

#[test]
fn attached_sources_start_at_the_attachments_position_with_spatial_audio_on() {
    let mut p = Proj::new();
    at_player(4.0)(&mut p);
    let mut r = rig_from(p);
    let (owner, handle) = r.source("camera", Some("player"));
    r.act(owner, handle, SourceAction::Play);
    let voice = r
        .world()
        .get::<EntityAudio>(owner)
        .unwrap()
        .sources
        .voice(handle)
        .unwrap();
    let options = r.control.voice(voice).unwrap().options;
    assert!(options.spatial.enabled);
    assert_eq!(options.spatial.position, [4.0, 2.0, 0.0]);
}

#[test]
fn unattached_sources_stay_non_spatial() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    assert!(
        !r.world()
            .get::<EntityAudio>(owner)
            .unwrap()
            .sources
            .options(handle)
            .unwrap()
            .spatial
            .enabled
    );
}

#[test]
fn attaching_to_an_entity_without_a_transform_is_refused() {
    let mut r = rig();
    let (owner, camera) = (r.entity("player"), r.entity("camera"));
    let result = audio::source(
        r.world(),
        owner,
        "sfx",
        PlayOptions::default(),
        Some(camera),
    );
    expect_code(result, "audio.attachment.invalid");
}

#[test]
fn synchronisation_moves_voices_with_their_attachment_every_tick() {
    let mut r = rig();
    let (owner, handle) = r.source("camera", Some("player"));
    r.act(owner, handle, SourceAction::Play);
    let player = r.entity("player");
    for x in [1.0_f32, 5.0, -3.0] {
        r.world().get_mut::<Transform>(player).unwrap().position = [x, 0.0, 0.0];
        r.rt.tick(0.1, ActionFrame::default()).unwrap();
        let world = r.rt.app().world();
        let voice = world
            .get::<EntityAudio>(owner)
            .unwrap()
            .sources
            .voice(handle)
            .unwrap();
        assert_eq!(
            r.control.voice(voice).unwrap().options.spatial.position[0],
            x
        );
    }
}

#[test]
fn sources_are_dropped_when_their_attachment_disappears() {
    let mut r = rig();
    let (owner, handle) = r.source("camera", Some("player"));
    r.act(owner, handle, SourceAction::Play);
    let player = r.entity("player");
    r.world().despawn(player);
    audio::synchronize(r.world()).unwrap();
    assert_eq!(r.control.voice_count(), 0);
    assert!(
        !r.world()
            .get::<EntityAudio>(owner)
            .unwrap()
            .sources
            .contains(handle)
    );
}

#[test]
fn reconfiguring_without_an_attachment_detaches_the_source() {
    let mut r = rig();
    let (owner, handle) = r.source("camera", Some("player"));
    audio::configure(r.world(), owner, handle, PlayOptions::default(), None).unwrap();
    let player = r.entity("player");
    r.world().despawn(player);
    audio::synchronize(r.world()).unwrap();
    assert!(
        r.world()
            .get::<EntityAudio>(owner)
            .unwrap()
            .sources
            .contains(handle)
    );
}

#[test]
fn listeners_need_configured_audio() {
    let mut rt = Proj::new().runtime();
    let owner = rt.entity("player").unwrap();
    expect_code(
        audio::listener(rt.app_mut().world_mut(), owner, Listener::default(), None),
        "audio.unavailable",
    );
}

#[test]
fn listener_follows_its_attachment_across_ticks() {
    let mut r = rig();
    let player = r.entity("player");
    audio::listener(r.world(), player, Listener::default(), Some(player)).unwrap();
    r.world().get_mut::<Transform>(player).unwrap().position = [3.0, 4.0, 0.0];
    r.rt.tick(0.1, ActionFrame::default()).unwrap();
    assert_eq!(r.control.listener().unwrap().position, [3.0, 4.0, 0.0]);
}

#[test]
fn a_fixed_listener_keeps_its_configured_position() {
    let mut r = rig();
    let player = r.entity("player");
    let fixed = Listener {
        position: [9.0, 9.0, 0.0],
        ..Default::default()
    };
    audio::listener(r.world(), player, fixed, None).unwrap();
    r.rt.tick(0.1, ActionFrame::default()).unwrap();
    assert_eq!(r.control.listener().unwrap().position, [9.0, 9.0, 0.0]);
}

#[test]
fn listener_resets_when_its_owner_disappears() {
    let mut r = rig();
    let (camera, player) = (r.entity("camera"), r.entity("player"));
    audio::listener(
        r.world(),
        camera,
        Listener {
            position: [1.0, 1.0, 1.0],
            ..Default::default()
        },
        None,
    )
    .unwrap();
    r.world().despawn(camera);
    let _ = player;
    audio::synchronize(r.world()).unwrap();
    assert_eq!(r.control.listener().unwrap(), Listener::default());
}

#[test]
fn listener_resets_when_its_attachment_loses_the_transform() {
    let mut r = rig();
    let player = r.entity("player");
    audio::listener(r.world(), player, Listener::default(), Some(player)).unwrap();
    r.world().entity_mut(player).remove::<Transform>();
    audio::synchronize(r.world()).unwrap();
    assert_eq!(r.control.listener().unwrap(), Listener::default());
}

#[test]
fn invalid_listener_orientations_are_refused() {
    let mut r = rig();
    let player = r.entity("player");
    let flat = Listener {
        forward: [0.0; 3],
        ..Default::default()
    };
    expect_code(
        audio::listener(r.world(), player, flat, None),
        "audio.listener.invalid",
    );
    let parallel = Listener {
        forward: [0.0, 1.0, 0.0],
        up: [0.0, 2.0, 0.0],
        ..Default::default()
    };
    expect_code(
        audio::listener(r.world(), player, parallel, None),
        "audio.listener.invalid",
    );
}

#[test]
fn listener_requires_a_live_owner_and_a_transformed_attachment() {
    let mut r = rig();
    let (player, camera) = (r.entity("player"), r.entity("camera"));
    expect_code(
        audio::listener(r.world(), player, Listener::default(), Some(camera)),
        "audio.attachment.invalid",
    );
    r.world().despawn(player);
    expect_code(
        audio::listener(r.world(), player, Listener::default(), None),
        "audio.owner.dead",
    );
}

// ---- observers and trigger events ---------------------------------------------------------------------

#[test]
fn observers_start_sources_when_their_event_fires() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    audio::observe(r.world(), owner, "door:open", handle, SourceAction::Play).unwrap();
    assert_eq!(audio::notify(r.world(), owner, "door:open").unwrap(), 1);
    assert_eq!(r.state(owner, handle), VoiceState::Playing);
}

#[test]
fn unobserved_events_are_a_successful_noop() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    audio::observe(r.world(), owner, "door:open", handle, SourceAction::Play).unwrap();
    assert_eq!(audio::notify(r.world(), owner, "door:close").unwrap(), 0);
    assert_eq!(r.control.voice_count(), 0);
}

#[test]
fn several_observers_run_in_registration_order() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    audio::observe(r.world(), owner, "go", handle, SourceAction::Play).unwrap();
    audio::observe(r.world(), owner, "go", handle, SourceAction::Pause).unwrap();
    assert_eq!(audio::notify(r.world(), owner, "go").unwrap(), 2);
    assert_eq!(r.state(owner, handle), VoiceState::Paused);
}

#[test]
fn trigger_overlaps_notify_audio_observers_by_entity_and_phase() {
    let mut p = Proj::new();
    p.add_entity(trigger("zone", 3.0, 0.0));
    let mut r = rig_from(p);
    let (owner, handle) = r.source("camera", None);
    audio::observe(
        r.world(),
        owner,
        "trigger:zone:enter",
        handle,
        SourceAction::Play,
    )
    .unwrap();
    audio::observe(
        r.world(),
        owner,
        "trigger:zone:exit",
        handle,
        SourceAction::Stop,
    )
    .unwrap();
    r.rt.tick(0.1, ActionFrame::default()).unwrap();
    assert_eq!(r.control.voice_count(), 0);
    r.rt.move_entity("player", [3.0, 0.0]).unwrap();
    r.rt.tick(0.1, ActionFrame::default()).unwrap();
    assert_eq!(r.control.voice_count(), 1);
    r.rt.move_entity("player", [-6.0, 0.0]).unwrap();
    r.rt.tick(0.1, ActionFrame::default()).unwrap();
    assert_eq!(r.control.voice_count(), 0);
}

#[test]
fn trigger_events_for_the_player_side_are_also_published() {
    let mut p = Proj::new();
    p.add_entity(trigger("zone", 0.0, 0.0));
    let mut r = rig_from(p);
    let (owner, handle) = r.source("camera", None);
    audio::observe(
        r.world(),
        owner,
        "trigger:player:enter",
        handle,
        SourceAction::Play,
    )
    .unwrap();
    r.rt.tick(0.1, ActionFrame::default()).unwrap();
    assert_eq!(r.control.voice_count(), 1);
}

// ---- teardown -------------------------------------------------------------------------------------------------

#[test]
fn stopping_the_runtime_silences_every_voice_and_resets_the_listener() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    r.act(owner, handle, SourceAction::Play);
    let player = r.entity("player");
    audio::listener(
        r.world(),
        player,
        Listener {
            position: [5.0, 0.0, 0.0],
            ..Default::default()
        },
        None,
    )
    .unwrap();
    r.rt.stop().unwrap();
    assert_eq!(r.control.voice_count(), 0);
    assert_eq!(r.control.listener().unwrap(), Listener::default());
}

#[test]
fn shutdown_removes_the_entity_audio_components() {
    let mut r = rig();
    let (owner, _) = r.source("player", None);
    audio::shutdown(r.world()).unwrap();
    assert!(r.world().get::<EntityAudio>(owner).is_none());
}

#[test]
fn dropping_the_runtime_releases_its_voices() {
    let mut r = rig();
    let (owner, handle) = r.source("player", None);
    r.act(owner, handle, SourceAction::Play);
    let control = r.control.clone();
    drop(r.rt);
    assert_eq!(control.voice_count(), 0);
}

#[test]
fn scene_transitions_silence_the_voices_of_despawned_owners() {
    let mut p = Proj::new();
    p.add_scene(
        "scene:b",
        "b.json",
        scene_json("room:b", vec![marker("door_b", 10.0, 0.0)]),
    );
    let mut r = rig_from(p);
    let (camera, a) = r.source("camera", None);
    let (player, b) = r.source("player", None);
    r.act(camera, a, SourceAction::Play);
    r.act(player, b, SourceAction::Play);
    r.rt.request_scene("scene:b", "door_b", "player").unwrap();
    r.rt.tick(0.1, ActionFrame::default()).unwrap();
    assert_eq!(r.control.voice_count(), 1);
    assert_eq!(r.state(player, b), VoiceState::Playing);
}

#[test]
fn removing_an_owner_entity_stops_only_its_voices() {
    let mut r = rig();
    let (camera, a) = r.source("camera", None);
    let (player, b) = r.source("player", None);
    r.act(camera, a, SourceAction::Play);
    r.act(player, b, SourceAction::Play);
    r.world().despawn(camera);
    assert_eq!(r.control.voice_count(), 1);
}

#[test]
fn configuring_audio_twice_replaces_the_runtime_clips_resource() {
    let p = Proj::new();
    let engine = AudioEngine::offline(Default::default()).unwrap();
    let mut rt = p.runtime();
    rt.set_audio(engine.control()).unwrap();
    rt.set_audio(engine.control()).unwrap();
    assert_eq!(rt.app().world().resource::<RuntimeAudio>().clips.len(), 1);
}
