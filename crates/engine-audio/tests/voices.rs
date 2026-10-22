//! Voice handles, limits, ownership, source controllers, lifecycle and threading.

mod common;

use common::*;
use engine_audio::{
    AudioConfig, AudioEngine, AudioSources, Bus, PlayOptions, SourceAction, VoiceState,
};
use std::sync::Arc;

fn owned(owner: &str) -> PlayOptions {
    PlayOptions {
        looped: true,
        owner: owner.into(),
        ..Default::default()
    }
}

#[test]
fn handles_are_unique_and_strictly_increasing_across_plays() {
    let e = engine();
    let clip = mono(48_000, &[0.1]);
    let handles: Vec<_> = (0..8)
        .map(|_| e.play(clip.clone(), looped()).unwrap())
        .collect();
    for pair in handles.windows(2) {
        assert!(pair[0] < pair[1]);
        assert!(pair[0].get() < pair[1].get());
    }
}

#[test]
fn voice_limit_rejects_the_next_play_and_leaves_existing_voices_intact() {
    let e = offline(48_000, 3);
    let clip = mono(48_000, &[0.1]);
    for _ in 0..3 {
        e.play(clip.clone(), looped()).unwrap();
    }
    let error = e.play(clip, looped()).unwrap_err();
    assert_eq!(error.code, "audio.voice.limit");
    assert_eq!(e.voice_count(), 3);
}

#[test]
fn stopping_a_voice_frees_a_slot_at_the_limit() {
    let e = offline(48_000, 1);
    let clip = mono(48_000, &[0.1]);
    let first = e.play(clip.clone(), looped()).unwrap();
    assert!(e.play(clip.clone(), looped()).is_err());
    assert!(e.stop(first));
    e.play(clip, looped()).unwrap();
    assert_eq!(e.voice_count(), 1);
}

#[test]
fn finished_one_shot_frees_its_slot_after_mixing() {
    let mut e = offline(48_000, 1);
    let clip = mono(48_000, &[0.1, 0.2]);
    e.play(clip.clone(), PlayOptions::default()).unwrap();
    assert!(e.play(clip.clone(), PlayOptions::default()).is_err());
    mix(&mut e, 2);
    e.play(clip, PlayOptions::default()).unwrap();
}

#[test]
fn rejected_play_does_not_consume_a_voice_slot() {
    let e = offline(48_000, 2);
    let clip = mono(48_000, &[0.1]);
    for pitch in [0.0, f32::NAN, 9.0] {
        assert!(
            e.play(
                clip.clone(),
                PlayOptions {
                    pitch,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    assert_eq!(e.voice_count(), 0);
    e.play(clip.clone(), looped()).unwrap();
    e.play(clip, looped()).unwrap();
}

#[test]
fn stop_reports_whether_a_voice_was_removed() {
    let e = engine();
    let v = e.play(mono(48_000, &[0.1]), looped()).unwrap();
    assert!(e.stop(v));
    assert!(!e.stop(v));
    assert_eq!(e.voice_state(v), VoiceState::Stopped);
}

#[test]
fn pause_resume_and_gain_on_a_stopped_voice_report_false() {
    let e = engine();
    let v = e.play(mono(48_000, &[0.1]), looped()).unwrap();
    e.stop(v);
    assert!(!e.pause(v));
    assert!(!e.resume(v));
    assert!(!e.set_gain(v, 0.5));
    assert!(e.voice(v).is_none());
}

#[test]
fn stale_handle_from_another_engine_is_unknown() {
    let a = engine();
    let b = engine();
    let v = a.play(mono(48_000, &[0.1]), looped()).unwrap();
    assert!(!b.stop(v));
    assert_eq!(b.voice_state(v), VoiceState::Stopped);
    assert_eq!(a.voice_state(v), VoiceState::Playing);
}

macro_rules! set_gain_is_clamped {
    ($($name:ident: $input:expr, $stored:expr;)*) => {$(
        #[test]
        fn $name() {
            let e = engine();
            let v = e.play(mono(48_000, &[0.1]), looped()).unwrap();
            assert!(e.set_gain(v, $input));
            assert_eq!(e.voice(v).unwrap().options.gain, $stored);
        }
    )*};
}

set_gain_is_clamped! {
    set_gain_keeps_values_inside_zero_one: 0.3f32, 0.3f32;
    set_gain_clamps_above_one: 4.0f32, 1.0f32;
    set_gain_clamps_negative_to_zero: -2.0f32, 0.0f32;
    set_gain_turns_nan_into_silence: f32::NAN, 0.0f32;
    set_gain_turns_infinity_into_silence: f32::INFINITY, 0.0f32;
    set_gain_turns_negative_infinity_into_silence: f32::NEG_INFINITY, 0.0f32;
}

#[test]
fn play_clamps_gain_in_the_stored_options() {
    let e = engine();
    let loud = e
        .play(
            mono(48_000, &[0.1]),
            PlayOptions {
                gain: 7.0,
                ..looped()
            },
        )
        .unwrap();
    let nan = e
        .play(
            mono(48_000, &[0.1]),
            PlayOptions {
                gain: f32::NAN,
                ..looped()
            },
        )
        .unwrap();
    assert_eq!(e.voice(loud).unwrap().options.gain, 1.0);
    assert_eq!(e.voice(nan).unwrap().options.gain, 0.0);
}

#[test]
fn bus_gain_defaults_to_unity_and_clamps() {
    let e = engine();
    for bus in [Bus::Master, Bus::Music, Bus::Effects, Bus::Dialogue] {
        assert_eq!(e.bus_gain(bus).unwrap(), 1.0);
    }
    e.set_bus_gain(Bus::Music, 3.0).unwrap();
    e.set_bus_gain(Bus::Dialogue, -3.0).unwrap();
    e.set_bus_gain(Bus::Effects, f32::NAN).unwrap();
    assert_eq!(e.bus_gain(Bus::Music).unwrap(), 1.0);
    assert_eq!(e.bus_gain(Bus::Dialogue).unwrap(), 0.0);
    assert_eq!(e.bus_gain(Bus::Effects).unwrap(), 0.0);
    assert_eq!(e.bus_gain(Bus::Master).unwrap(), 1.0);
}

#[test]
fn voices_cannot_be_routed_to_the_master_bus() {
    let e = engine();
    let error = e
        .play(
            mono(48_000, &[0.1]),
            PlayOptions {
                bus: Bus::Master,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.code, "audio.play.invalid");
}

#[test]
fn owner_length_boundary_is_256_bytes() {
    let e = engine();
    let clip = mono(48_000, &[0.1]);
    e.play(clip.clone(), owned(&"o".repeat(256))).unwrap();
    assert!(e.play(clip, owned(&"o".repeat(257))).is_err());
}

#[test]
fn stop_owner_only_removes_that_owners_voices() {
    let e = engine();
    let clip = mono(48_000, &[0.1]);
    let a1 = e.play(clip.clone(), owned("a")).unwrap();
    let a2 = e.play(clip.clone(), owned("a")).unwrap();
    let b = e.play(clip.clone(), owned("b")).unwrap();
    let anon = e.play(clip, looped()).unwrap();
    e.stop_owner("a");
    assert_eq!(e.voice_state(a1), VoiceState::Stopped);
    assert_eq!(e.voice_state(a2), VoiceState::Stopped);
    assert_eq!(e.voice_state(b), VoiceState::Playing);
    assert_eq!(e.voice_state(anon), VoiceState::Playing);
}

#[test]
fn stop_owner_with_empty_name_targets_unowned_voices_only() {
    let e = engine();
    let clip = mono(48_000, &[0.1]);
    let named = e.play(clip.clone(), owned("scene")).unwrap();
    let anon = e.play(clip, looped()).unwrap();
    e.stop_owner("");
    assert_eq!(e.voice_state(anon), VoiceState::Stopped);
    assert_eq!(e.voice_state(named), VoiceState::Playing);
}

#[test]
fn stop_owner_is_an_exact_match_not_a_prefix_match() {
    let e = engine();
    let clip = mono(48_000, &[0.1]);
    let long = e.play(clip, owned("scene-2")).unwrap();
    e.stop_owner("scene");
    assert_eq!(e.voice_state(long), VoiceState::Playing);
}

#[test]
fn stop_all_clears_playing_and_paused_voices() {
    let e = engine();
    let clip = mono(48_000, &[0.1]);
    let paused = e.play(clip.clone(), looped()).unwrap();
    e.play(clip, looped()).unwrap();
    e.pause(paused);
    e.stop_all();
    assert_eq!(e.voice_count(), 0);
    assert_eq!(e.voice_state(paused), VoiceState::Stopped);
}

#[test]
fn configure_unknown_voice_returns_false_without_error() {
    let e = engine();
    let v = e.play(mono(48_000, &[0.1]), looped()).unwrap();
    e.stop(v);
    assert!(!e.configure(v, looped()).unwrap());
}

#[test]
fn configure_validates_even_for_unknown_voices() {
    let e = engine();
    let v = e.play(mono(48_000, &[0.1]), looped()).unwrap();
    e.stop(v);
    let bad = PlayOptions {
        pitch: 100.0,
        ..looped()
    };
    assert_eq!(e.configure(v, bad).unwrap_err().code, "audio.play.invalid");
}

#[test]
fn configure_preserves_cursor_and_pause_state_but_replaces_options() {
    let mut e = engine();
    let v = e.play(mono(48_000, &[0.0; 16]), looped()).unwrap();
    mix(&mut e, 5);
    e.pause(v);
    e.configure(
        v,
        PlayOptions {
            gain: 0.25,
            bus: Bus::Music,
            ..looped()
        },
    )
    .unwrap();
    let s = e.voice(v).unwrap();
    assert_eq!((s.cursor, s.state), (5.0, VoiceState::Paused));
    assert_eq!((s.options.gain, s.options.bus), (0.25, Bus::Music));
}

#[test]
fn configure_can_turn_a_looping_voice_into_a_one_shot() {
    let mut e = engine();
    let v = e.play(mono(48_000, &[0.5, 0.5]), looped()).unwrap();
    mix(&mut e, 1);
    e.configure(v, PlayOptions::default()).unwrap();
    near_all(&left(&mut e, 3), &[0.5, 0.0, 0.0]);
    assert_eq!(e.voice_state(v), VoiceState::Stopped);
}

#[test]
fn snapshot_reports_the_options_the_voice_was_started_with() {
    let e = engine();
    let options = PlayOptions {
        looped: true,
        bus: Bus::Dialogue,
        gain: 0.5,
        owner: "npc".into(),
        pitch: 1.5,
        ..Default::default()
    };
    let v = e.play(mono(48_000, &[0.1]), options.clone()).unwrap();
    let s = e.voice(v).unwrap();
    assert_eq!(s.options, options);
    assert_eq!((s.state, s.cursor), (VoiceState::Playing, 0.0));
}

macro_rules! invalid_config {
    ($($name:ident: $rate:expr, $voices:expr;)*) => {$(
        #[test]
        fn $name() {
            let error = AudioEngine::offline(AudioConfig { sample_rate: $rate, max_voices: $voices })
                .err()
                .expect("config must be rejected");
            assert_eq!(error.code, "audio.options.invalid");
        }
    )*};
}

invalid_config! {
    config_rejects_rate_below_minimum: 7_999u32, 8usize;
    config_rejects_rate_above_maximum: 192_001u32, 8usize;
    config_rejects_zero_rate: 0u32, 8usize;
    config_rejects_zero_voices: 48_000u32, 0usize;
    config_rejects_voice_limit_above_1024: 48_000u32, 1_025usize;
}

#[test]
fn config_boundaries_are_inclusive() {
    for (rate, voices) in [(8_000, 1), (192_000, 1024), (44_100, 7)] {
        let e = AudioEngine::offline(AudioConfig {
            sample_rate: rate,
            max_voices: voices,
        })
        .unwrap();
        assert_eq!(e.voice_count(), 0);
    }
}

#[test]
fn default_config_is_48k_with_64_voices() {
    let c = AudioConfig::default();
    assert_eq!((c.sample_rate, c.max_voices), (48_000, 64));
}

#[test]
fn device_constructor_validates_config_before_touching_hardware() {
    let error = AudioEngine::device(AudioConfig {
        sample_rate: 1,
        max_voices: 4,
    })
    .err()
    .unwrap();
    assert_eq!(error.code, "audio.options.invalid");
}

#[test]
fn offline_engine_reports_no_device_state() {
    let e = engine();
    assert!(!e.playback_enabled());
    assert!(e.device_error().is_none());
    assert_eq!(e.device_frames(), 0);
}

#[test]
fn shutdown_clears_voices_and_rejects_further_playback() {
    let mut e = engine();
    let v = e.play(mono(48_000, &[0.1]), looped()).unwrap();
    e.shutdown();
    assert_eq!(e.voice_count(), 0);
    assert_eq!(e.voice_state(v), VoiceState::Stopped);
    let clip = mono(48_000, &[0.1]);
    assert_eq!(
        e.play(clip, looped()).unwrap_err().code,
        "audio.not_initialized"
    );
    assert_eq!(
        e.set_bus_gain(Bus::Music, 0.5).unwrap_err().code,
        "audio.not_initialized"
    );
    assert_eq!(
        e.set_listener(Default::default()).unwrap_err().code,
        "audio.not_initialized"
    );
    assert_eq!(
        e.mix(&mut [0.0; 2]).unwrap_err().code,
        "audio.not_initialized"
    );
}

#[test]
fn shutdown_is_idempotent() {
    let mut e = engine();
    e.shutdown();
    e.shutdown();
    assert_eq!(e.voice_count(), 0);
}

#[test]
fn shutdown_resets_the_listener_to_the_default_pose() {
    let mut e = engine();
    e.set_listener(engine_audio::Listener {
        position: [4.0, 0.0, 0.0],
        ..Default::default()
    })
    .unwrap();
    e.shutdown();
    assert_eq!(e.listener().unwrap(), Default::default());
}

#[test]
fn controls_cloned_before_drop_become_inert_afterwards() {
    let control = {
        let e = engine();
        e.play(mono(48_000, &[0.1]), looped()).unwrap();
        e.control()
    };
    assert_eq!(control.voice_count(), 0);
    assert_eq!(
        control
            .play(mono(48_000, &[0.1]), looped())
            .unwrap_err()
            .code,
        "audio.not_initialized"
    );
}

#[test]
fn clones_share_one_voice_table() {
    let e = engine();
    let a = e.control();
    let b = e.control();
    let v = a.play(mono(48_000, &[0.1]), looped()).unwrap();
    assert_eq!(b.voice_count(), 1);
    assert!(b.stop(v));
    assert_eq!(a.voice_count(), 0);
}

#[test]
fn concurrent_players_never_exceed_the_limit_or_duplicate_handles() {
    let e = offline(48_000, 40);
    let clip = mono(48_000, &[0.1]);
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let control = e.control();
            let clip = clip.clone();
            std::thread::spawn(move || {
                (0..10)
                    .filter_map(|_| control.play(clip.clone(), looped()).ok())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut handles: Vec<_> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    assert_eq!(handles.len(), 40);
    assert_eq!(e.voice_count(), 40);
    handles.sort();
    handles.dedup();
    assert_eq!(handles.len(), 40);
}

// ---- AudioSources ----------------------------------------------------------------------

#[test]
fn source_play_stamps_the_controller_owner_on_its_voice() {
    let e = engine();
    let mut sources = AudioSources::new(e.control(), "level-1").unwrap();
    let s = sources
        .create(mono(48_000, &[0.1]), owned("ignored"))
        .unwrap();
    assert_eq!(sources.options(s).unwrap().owner, "level-1");
    sources.control(s, SourceAction::Play).unwrap();
    assert_eq!(
        e.voice(sources.voice(s).unwrap()).unwrap().options.owner,
        "level-1"
    );
}

#[test]
fn source_replay_restarts_from_the_beginning_with_a_new_voice() {
    let mut e = engine();
    let mut sources = AudioSources::new(e.control(), "s").unwrap();
    let s = sources.create(mono(48_000, &[0.0; 32]), looped()).unwrap();
    sources.control(s, SourceAction::Play).unwrap();
    let first = sources.voice(s).unwrap();
    mix(&mut e, 10);
    sources.control(s, SourceAction::Play).unwrap();
    let second = sources.voice(s).unwrap();
    assert_ne!(first, second);
    assert_eq!(e.voice(second).unwrap().cursor, 0.0);
    assert_eq!(e.voice_count(), 1);
}

#[test]
fn source_pause_resume_and_stop_follow_the_voice_state() {
    let e = engine();
    let mut sources = AudioSources::new(e.control(), "s").unwrap();
    let s = sources.create(mono(48_000, &[0.1]), looped()).unwrap();
    assert_eq!(sources.state(s), VoiceState::Stopped);
    sources.control(s, SourceAction::Pause).unwrap();
    assert_eq!(sources.state(s), VoiceState::Stopped);
    sources.control(s, SourceAction::Play).unwrap();
    assert_eq!(sources.state(s), VoiceState::Playing);
    sources.control(s, SourceAction::Pause).unwrap();
    assert_eq!(sources.state(s), VoiceState::Paused);
    sources.control(s, SourceAction::Resume).unwrap();
    assert_eq!(sources.state(s), VoiceState::Playing);
    sources.control(s, SourceAction::Stop).unwrap();
    assert_eq!(sources.state(s), VoiceState::Stopped);
    assert!(sources.voice(s).is_none());
}

#[test]
fn dropping_one_controller_leaves_another_controllers_voices_alone() {
    let e = engine();
    let clip = mono(48_000, &[0.1]);
    let mut keep = AudioSources::new(e.control(), "keep").unwrap();
    let kept = keep.create(clip.clone(), looped()).unwrap();
    keep.control(kept, SourceAction::Play).unwrap();
    {
        let mut temp = AudioSources::new(e.control(), "temp").unwrap();
        let t = temp.create(clip, looped()).unwrap();
        temp.control(t, SourceAction::Play).unwrap();
        assert_eq!(e.voice_count(), 2);
    }
    assert_eq!(e.voice_count(), 1);
    assert_eq!(keep.state(kept), VoiceState::Playing);
}

#[test]
fn removing_a_source_drops_its_observers_but_not_other_sources() {
    let e = engine();
    let clip = mono(48_000, &[0.1]);
    let mut sources = AudioSources::new(e.control(), "s").unwrap();
    let a = sources.create(clip.clone(), looped()).unwrap();
    let b = sources.create(clip, looped()).unwrap();
    sources.observe("go", a, SourceAction::Play).unwrap();
    sources.observe("go", b, SourceAction::Play).unwrap();
    assert!(sources.remove(a));
    assert!(!sources.remove(a));
    assert_eq!(sources.notify("go").unwrap(), 1);
    assert_eq!(sources.state(b), VoiceState::Playing);
}

#[test]
fn observer_on_unknown_source_is_rejected() {
    let e = engine();
    let mut sources = AudioSources::new(e.control(), "s").unwrap();
    let s = sources.create(mono(48_000, &[0.1]), looped()).unwrap();
    sources.remove(s);
    assert_eq!(
        sources
            .observe("x", s, SourceAction::Play)
            .unwrap_err()
            .code,
        "audio.source.missing"
    );
}

#[test]
fn unknown_source_operations_report_missing() {
    let e = engine();
    let mut other = AudioSources::new(e.control(), "o").unwrap();
    let foreign = other.create(mono(48_000, &[0.1]), looped()).unwrap();
    let mut sources = AudioSources::new(e.control(), "s").unwrap();
    assert_eq!(
        sources
            .control(foreign, SourceAction::Play)
            .unwrap_err()
            .code,
        "audio.source.missing"
    );
    assert_eq!(
        sources.configure(foreign, looped()).unwrap_err().code,
        "audio.source.missing"
    );
    assert!(sources.options(foreign).is_none());
}

#[test]
fn notify_runs_observers_in_registration_order() {
    let e = engine();
    let mut sources = AudioSources::new(e.control(), "s").unwrap();
    let s = sources.create(mono(48_000, &[0.1]), looped()).unwrap();
    sources.observe("ev", s, SourceAction::Play).unwrap();
    sources.observe("ev", s, SourceAction::Pause).unwrap();
    sources.observe("ev", s, SourceAction::Resume).unwrap();
    assert_eq!(sources.notify("ev").unwrap(), 3);
    assert_eq!(sources.state(s), VoiceState::Playing);
}

#[test]
fn notify_only_fires_observers_of_the_matching_event() {
    let e = engine();
    let mut sources = AudioSources::new(e.control(), "s").unwrap();
    let a = sources.create(mono(48_000, &[0.1]), looped()).unwrap();
    let b = sources.create(mono(48_000, &[0.1]), looped()).unwrap();
    sources.observe("open", a, SourceAction::Play).unwrap();
    sources.observe("close", b, SourceAction::Play).unwrap();
    assert_eq!(sources.notify("open").unwrap(), 1);
    assert_eq!(sources.state(a), VoiceState::Playing);
    assert_eq!(sources.state(b), VoiceState::Stopped);
    assert_eq!(sources.notify("OPEN").unwrap(), 0);
}

#[test]
fn source_play_fails_with_voice_limit_and_leaves_the_source_stopped() {
    let e = offline(48_000, 1);
    let clip = mono(48_000, &[0.1]);
    e.play(clip.clone(), looped()).unwrap();
    let mut sources = AudioSources::new(e.control(), "s").unwrap();
    let s = sources.create(clip, looped()).unwrap();
    assert_eq!(
        sources.control(s, SourceAction::Play).unwrap_err().code,
        "audio.voice.limit"
    );
    assert_eq!(sources.state(s), VoiceState::Stopped);
}

#[test]
fn source_clear_stops_all_voices_and_forgets_handles() {
    let e = engine();
    let mut sources = AudioSources::new(e.control(), "s").unwrap();
    let s = sources.create(mono(48_000, &[0.1]), looped()).unwrap();
    sources.control(s, SourceAction::Play).unwrap();
    sources.clear();
    assert_eq!(e.voice_count(), 0);
    assert!(!sources.contains(s));
}

#[test]
fn source_audio_matches_direct_voice_output() {
    let mut a = engine();
    let mut b = engine();
    let clip: Arc<_> = mono(32_000, &[0.1, 0.5, -0.3, 0.2]);
    a.play(
        clip.clone(),
        PlayOptions {
            gain: 0.5,
            ..Default::default()
        },
    )
    .unwrap();
    let mut sources = AudioSources::new(b.control(), "s").unwrap();
    let s = sources
        .create(
            clip,
            PlayOptions {
                gain: 0.5,
                ..Default::default()
            },
        )
        .unwrap();
    sources.control(s, SourceAction::Play).unwrap();
    assert_eq!(mix(&mut a, 12), mix(&mut b, 12));
}
