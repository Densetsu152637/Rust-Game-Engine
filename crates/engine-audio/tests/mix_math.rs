//! Offline mixer arithmetic: channel mapping, resampling, pitch, clipping, looping
//! boundaries, pause/resume and bus gain chains.

mod common;

use common::*;
use engine_audio::{Bus, PlayOptions, VoiceState};

#[test]
fn no_voices_produces_exact_silence_over_dirty_buffers() {
    let mut e = engine();
    assert!(mix(&mut e, 16).iter().all(|s| *s == 0.0));
}

#[test]
fn empty_output_buffer_is_accepted() {
    let mut e = engine();
    e.play(mono(48_000, &[0.5]), looped()).unwrap();
    e.mix(&mut []).unwrap();
    assert_eq!(e.voice_count(), 1);
}

#[test]
fn mono_clip_is_duplicated_to_both_channels() {
    let mut e = engine();
    e.play(mono(48_000, &[0.25, -0.75]), PlayOptions::default())
        .unwrap();
    assert_eq!(mix(&mut e, 2), [0.25, 0.25, -0.75, -0.75]);
}

#[test]
fn stereo_clip_keeps_left_and_right_separate() {
    let mut e = engine();
    e.play(
        stereo(48_000, &[0.5, -0.5, 0.25, 0.75]),
        PlayOptions::default(),
    )
    .unwrap();
    assert_eq!(mix(&mut e, 2), [0.5, -0.5, 0.25, 0.75]);
}

#[test]
fn one_shot_leaves_trailing_buffer_silent_and_retires_the_voice() {
    let mut e = engine();
    e.play(mono(48_000, &[0.5, 0.5]), PlayOptions::default())
        .unwrap();
    assert_eq!(mix(&mut e, 4), [0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(e.voice_count(), 0);
}

#[test]
fn one_shot_that_ends_exactly_on_the_buffer_edge_is_retired_in_that_call() {
    let mut e = engine();
    let v = e
        .play(mono(48_000, &[0.1, 0.2, 0.3]), PlayOptions::default())
        .unwrap();
    near_all(&left(&mut e, 3), &[0.1, 0.2, 0.3]);
    assert_eq!(e.voice_state(v), VoiceState::Stopped);
    assert!(e.voice(v).is_none());
}

#[test]
fn one_shot_in_progress_reports_cursor_in_source_frames() {
    let mut e = engine();
    let v = e
        .play(mono(48_000, &[0.0; 10]), PlayOptions::default())
        .unwrap();
    mix(&mut e, 4);
    assert_eq!(e.voice(v).unwrap().cursor, 4.0);
    mix(&mut e, 3);
    assert_eq!(e.voice(v).unwrap().cursor, 7.0);
}

#[test]
fn chunked_mixing_matches_a_single_large_mix() {
    let samples: Vec<f32> = (0..37).map(|i| ((i as f32) * 0.37).sin() * 0.8).collect();
    let make = || {
        let e = offline(44_100, 8);
        e.play(
            mono(32_000, &samples),
            PlayOptions {
                pitch: 1.3,
                ..Default::default()
            },
        )
        .unwrap();
        e.play(
            mono(48_000, &samples),
            PlayOptions {
                looped: true,
                pitch: 0.7,
                gain: 0.5,
                ..Default::default()
            },
        )
        .unwrap();
        e
    };
    let whole = mix(&mut make(), 200);
    let mut chunked_engine = make();
    let mut chunked = Vec::new();
    for size in [1, 2, 3, 5, 8, 13, 21, 34, 113] {
        chunked.extend(mix(&mut chunked_engine, size));
    }
    assert_eq!(chunked.len(), whole.len());
    assert_eq!(chunked, whole);
}

#[test]
fn two_engines_with_identical_setup_produce_identical_samples() {
    let build = || {
        let e = engine();
        e.play(mono(22_050, &[0.1, 0.9, -0.4, 0.3]), looped())
            .unwrap();
        e.play(stereo(48_000, &[0.2, -0.2, 0.6, -0.6]), looped())
            .unwrap();
        e
    };
    assert_eq!(mix(&mut build(), 500), mix(&mut build(), 500));
}

#[test]
fn upsampling_interpolates_linearly_between_source_frames() {
    let mut e = engine();
    e.play(mono(24_000, &[0.0, 1.0, 0.0]), PlayOptions::default())
        .unwrap();
    near_all(&left(&mut e, 6), &[0.0, 0.5, 1.0, 0.5, 0.0, 0.0]);
}

#[test]
fn upsampling_by_four_hits_quarter_steps() {
    let mut e = offline(32_000, 4);
    e.play(mono(8_000, &[0.0, 0.4]), PlayOptions::default())
        .unwrap();
    near_all(&left(&mut e, 5), &[0.0, 0.1, 0.2, 0.3, 0.4]);
}

#[test]
fn downsampling_skips_source_frames_without_averaging() {
    let mut e = engine();
    e.play(
        mono(96_000, &[0.0, 0.1, 0.2, 0.3, 0.4, 0.5]),
        PlayOptions::default(),
    )
    .unwrap();
    near_all(&left(&mut e, 4), &[0.0, 0.2, 0.4, 0.0]);
}

#[test]
fn clip_rate_equal_to_output_rate_advances_one_frame_per_output_frame() {
    let mut e = offline(22_050, 2);
    let v = e
        .play(mono(22_050, &[0.0; 64]), PlayOptions::default())
        .unwrap();
    mix(&mut e, 9);
    assert_eq!(e.voice(v).unwrap().cursor, 9.0);
}

#[test]
fn cd_rate_clip_on_48k_output_advances_by_the_rate_ratio() {
    let mut e = engine();
    let v = e
        .play(mono(44_100, &[0.0; 4096]), PlayOptions::default())
        .unwrap();
    mix(&mut e, 1000);
    let cursor = e.voice(v).unwrap().cursor;
    assert!((cursor - 918.75).abs() < 1e-6, "{cursor}");
}

macro_rules! pitch_advances_cursor {
    ($($name:ident: $pitch:expr, $frames:expr, $expected:expr;)*) => {$(
        #[test]
        fn $name() {
            let mut e = engine();
            let v = e.play(
                mono(48_000, &[0.0; 4096]),
                PlayOptions { pitch: $pitch, ..Default::default() },
            ).unwrap();
            mix(&mut e, $frames);
            assert_eq!(e.voice(v).unwrap().cursor, $expected);
        }
    )*};
}

pitch_advances_cursor! {
    pitch_one_eighth_is_the_slowest_playback: 0.125f32, 80usize, 10.0f64;
    pitch_half_plays_at_half_speed: 0.5f32, 20usize, 10.0f64;
    pitch_one_is_realtime: 1.0f32, 10usize, 10.0f64;
    pitch_double_plays_at_double_speed: 2.0f32, 10usize, 20.0f64;
    pitch_eight_is_the_fastest_playback: 8.0f32, 10usize, 80.0f64;
}

#[test]
fn pitch_multiplies_with_the_sample_rate_ratio() {
    let mut e = engine();
    let v = e
        .play(
            mono(24_000, &[0.0; 1024]),
            PlayOptions {
                pitch: 4.0,
                ..Default::default()
            },
        )
        .unwrap();
    mix(&mut e, 10);
    assert_eq!(e.voice(v).unwrap().cursor, 20.0);
}

#[test]
fn pitched_one_shot_finishes_in_proportionally_fewer_frames() {
    let mut e = engine();
    e.play(
        mono(48_000, &[0.5; 8]),
        PlayOptions {
            pitch: 4.0,
            ..Default::default()
        },
    )
    .unwrap();
    near_all(&left(&mut e, 4), &[0.5, 0.5, 0.0, 0.0]);
    assert_eq!(e.voice_count(), 0);
}

#[test]
fn simultaneous_voices_sum_before_clamping() {
    let mut e = engine();
    e.play(mono(48_000, &[0.25]), PlayOptions::default())
        .unwrap();
    e.play(mono(48_000, &[0.5]), PlayOptions::default())
        .unwrap();
    assert_eq!(mix(&mut e, 1), [0.75, 0.75]);
}

#[test]
fn positive_overflow_clamps_to_plus_one() {
    let mut e = engine();
    for _ in 0..3 {
        e.play(mono(48_000, &[0.9]), PlayOptions::default())
            .unwrap();
    }
    assert_eq!(mix(&mut e, 1), [1.0, 1.0]);
}

#[test]
fn negative_overflow_clamps_to_minus_one() {
    let mut e = engine();
    for _ in 0..3 {
        e.play(mono(48_000, &[-0.9]), PlayOptions::default())
            .unwrap();
    }
    assert_eq!(mix(&mut e, 1), [-1.0, -1.0]);
}

#[test]
fn opposite_phase_voices_cancel_exactly() {
    let mut e = engine();
    e.play(mono(48_000, &[0.6, 0.3]), PlayOptions::default())
        .unwrap();
    e.play(mono(48_000, &[-0.6, -0.3]), PlayOptions::default())
        .unwrap();
    assert_eq!(mix(&mut e, 2), [0.0; 4]);
}

#[test]
fn clamping_applies_per_channel_independently() {
    let mut e = engine();
    e.play(stereo(48_000, &[0.8, 0.1]), PlayOptions::default())
        .unwrap();
    e.play(stereo(48_000, &[0.8, 0.1]), PlayOptions::default())
        .unwrap();
    near_all(&mix(&mut e, 1), &[1.0, 0.2]);
}

#[test]
fn full_scale_signal_is_not_clamped_below_unity() {
    let mut e = engine();
    e.play(mono(48_000, &[1.0, -1.0]), PlayOptions::default())
        .unwrap();
    assert_eq!(mix(&mut e, 2), [1.0, 1.0, -1.0, -1.0]);
}

#[test]
fn looping_wraps_back_to_the_first_frame() {
    let mut e = engine();
    e.play(mono(48_000, &[0.2, 0.4]), looped()).unwrap();
    near_all(&left(&mut e, 6), &[0.2, 0.4, 0.2, 0.4, 0.2, 0.4]);
}

#[test]
fn looping_single_frame_clip_holds_its_value_forever() {
    let mut e = engine();
    e.play(mono(48_000, &[0.3]), looped()).unwrap();
    assert!(left(&mut e, 25).iter().all(|s| *s == 0.3));
    assert_eq!(e.voice_count(), 1);
}

#[test]
fn looping_at_half_speed_interpolates_across_the_seam() {
    let mut e = engine();
    e.play(
        mono(48_000, &[0.2, 0.4]),
        PlayOptions {
            pitch: 0.5,
            ..looped()
        },
    )
    .unwrap();
    near_all(&left(&mut e, 5), &[0.2, 0.3, 0.4, 0.3, 0.2]);
}

#[test]
fn looping_wraps_a_cursor_that_overshoots_the_end_by_a_fraction() {
    let mut e = engine();
    // Step 2.5 over three frames: cursors 0, 2.5, 5.0 -> 2.0, then 4.5 -> 1.5, 7.0 -> 1.0.
    e.play(
        mono(48_000, &[0.0, 0.3, 0.6]),
        PlayOptions {
            pitch: 2.5,
            ..looped()
        },
    )
    .unwrap();
    let out = left(&mut e, 4);
    near(out[0], 0.0);
    // cursor 2.5: frames 2 and (loop) 0 -> halfway from 0.6 to 0.0.
    near(out[1], 0.3);
    // cursor 5.0 wraps to 2.0 -> exactly frame 2.
    near(out[2], 0.6);
    // cursor 7.5 wraps to 1.5 -> halfway between 0.3 and 0.6.
    near(out[3], 0.45);
}

macro_rules! loop_cursor_stays_inside_clip {
    ($($name:ident: $pitch:expr, $frames:expr;)*) => {$(
        #[test]
        fn $name() {
            let mut e = engine();
            let v = e.play(
                mono(48_000, &[0.1, 0.2, 0.3, 0.4, 0.5]),
                PlayOptions { pitch: $pitch, ..looped() },
            ).unwrap();
            let out = left(&mut e, $frames);
            assert!(out.iter().all(|s| (0.1 - 1e-6..=0.5 + 1e-6).contains(s)), "{out:?}");
            assert!(e.voice(v).unwrap().cursor.is_finite());
            assert_eq!(e.voice_state(v), VoiceState::Playing);
        }
    )*};
}

loop_cursor_stays_inside_clip! {
    loop_at_pitch_eight_never_leaves_clip_range: 8.0f32, 401usize;
    loop_at_pitch_three_never_leaves_clip_range: 3.0f32, 333usize;
    loop_at_pitch_seven_tenths_never_leaves_clip_range: 0.7f32, 777usize;
    loop_at_pitch_one_eighth_never_leaves_clip_range: 0.125f32, 1000usize;
}

#[test]
fn looped_voice_survives_many_buffer_wraps_without_drifting_in_value() {
    let mut e = engine();
    e.play(mono(48_000, &[0.1, 0.2, 0.3]), looped()).unwrap();
    let out = left(&mut e, 3000);
    for (i, s) in out.iter().enumerate() {
        near(*s, [0.1, 0.2, 0.3][i % 3]);
    }
}

#[test]
fn looped_stereo_clip_wraps_both_channels_together() {
    let mut e = engine();
    e.play(stereo(48_000, &[0.1, -0.1, 0.2, -0.2]), looped())
        .unwrap();
    near_all(&mix(&mut e, 3), &[0.1, -0.1, 0.2, -0.2, 0.1, -0.1]);
}

#[test]
fn non_looped_last_frame_holds_instead_of_wrapping_during_interpolation() {
    let mut e = engine();
    e.play(mono(24_000, &[0.0, 1.0]), PlayOptions::default())
        .unwrap();
    // Cursor 1.5 interpolates frame 1 with itself, never with frame 0.
    near_all(&left(&mut e, 4), &[0.0, 0.5, 1.0, 1.0]);
}

#[test]
fn paused_voice_outputs_silence_and_keeps_its_cursor() {
    let mut e = engine();
    let v = e
        .play(mono(48_000, &[0.1, 0.2, 0.3, 0.4]), PlayOptions::default())
        .unwrap();
    mix(&mut e, 2);
    assert!(e.pause(v));
    assert_eq!(mix(&mut e, 10), [0.0; 20]);
    assert_eq!(e.voice(v).unwrap().cursor, 2.0);
    assert_eq!(e.voice_state(v), VoiceState::Paused);
}

#[test]
fn resume_continues_from_the_paused_position_without_skipping() {
    let mut e = engine();
    let v = e
        .play(mono(48_000, &[0.1, 0.2, 0.3, 0.4]), PlayOptions::default())
        .unwrap();
    near_all(&left(&mut e, 2), &[0.1, 0.2]);
    e.pause(v);
    mix(&mut e, 100);
    e.resume(v);
    near_all(&left(&mut e, 3), &[0.3, 0.4, 0.0]);
}

#[test]
fn pausing_twice_and_resuming_twice_is_idempotent() {
    let e = engine();
    let v = e
        .play(mono(48_000, &[0.5, 0.5]), PlayOptions::default())
        .unwrap();
    assert!(e.pause(v) && e.pause(v));
    assert_eq!(e.voice_state(v), VoiceState::Paused);
    assert!(e.resume(v) && e.resume(v));
    assert_eq!(e.voice_state(v), VoiceState::Playing);
}

#[test]
fn paused_voice_still_occupies_a_voice_slot() {
    let e = offline(48_000, 1);
    let v = e
        .play(mono(48_000, &[0.5; 4]), PlayOptions::default())
        .unwrap();
    e.pause(v);
    assert_eq!(
        e.play(mono(48_000, &[0.5]), PlayOptions::default())
            .unwrap_err()
            .code,
        "audio.voice.limit"
    );
}

#[test]
fn pausing_one_voice_does_not_affect_the_others() {
    let mut e = engine();
    let a = e
        .play(mono(48_000, &[0.25; 4]), PlayOptions::default())
        .unwrap();
    e.play(mono(48_000, &[0.5; 4]), PlayOptions::default())
        .unwrap();
    e.pause(a);
    assert_eq!(mix(&mut e, 1), [0.5, 0.5]);
}

#[test]
fn voice_gain_scales_the_signal() {
    let mut e = engine();
    e.play(
        mono(48_000, &[0.8]),
        PlayOptions {
            gain: 0.25,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(mix(&mut e, 1), [0.2, 0.2]);
}

#[test]
fn zero_gain_voice_is_silent_but_still_advances() {
    let mut e = engine();
    let v = e
        .play(
            mono(48_000, &[0.8; 8]),
            PlayOptions {
                gain: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(mix(&mut e, 3), [0.0; 6]);
    assert_eq!(e.voice(v).unwrap().cursor, 3.0);
}

#[test]
fn gain_chain_multiplies_voice_master_and_bus() {
    let mut e = engine();
    e.set_bus_gain(Bus::Master, 0.5).unwrap();
    e.set_bus_gain(Bus::Music, 0.5).unwrap();
    e.play(
        mono(48_000, &[1.0]),
        PlayOptions {
            bus: Bus::Music,
            gain: 0.5,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(mix(&mut e, 1), [0.125, 0.125]);
}

#[test]
fn master_gain_applies_to_every_bus() {
    let mut e = engine();
    e.set_bus_gain(Bus::Master, 0.5).unwrap();
    for bus in [Bus::Music, Bus::Effects, Bus::Dialogue] {
        e.play(
            mono(48_000, &[0.25]),
            PlayOptions {
                bus,
                ..Default::default()
            },
        )
        .unwrap();
    }
    assert_eq!(mix(&mut e, 1), [0.375, 0.375]);
}

#[test]
fn bus_gains_are_independent() {
    let mut e = engine();
    e.set_bus_gain(Bus::Music, 0.0).unwrap();
    e.set_bus_gain(Bus::Dialogue, 0.5).unwrap();
    for bus in [Bus::Music, Bus::Effects, Bus::Dialogue] {
        e.play(
            mono(48_000, &[0.25]),
            PlayOptions {
                bus,
                ..Default::default()
            },
        )
        .unwrap();
    }
    // music muted, effects unity, dialogue half
    assert_eq!(mix(&mut e, 1), [0.375, 0.375]);
}

#[test]
fn muting_master_silences_but_voices_keep_advancing() {
    let mut e = engine();
    e.set_bus_gain(Bus::Master, 0.0).unwrap();
    let v = e.play(mono(48_000, &[0.9; 8]), looped()).unwrap();
    assert_eq!(mix(&mut e, 4), [0.0; 8]);
    assert_eq!(e.voice(v).unwrap().cursor, 4.0);
    e.set_bus_gain(Bus::Master, 1.0).unwrap();
    assert_eq!(mix(&mut e, 1), [0.9, 0.9]);
}

#[test]
fn bus_gain_changes_apply_to_voices_already_playing() {
    let mut e = engine();
    e.play(mono(48_000, &[0.8]), looped()).unwrap();
    assert_eq!(mix(&mut e, 1), [0.8, 0.8]);
    e.set_bus_gain(Bus::Effects, 0.5).unwrap();
    assert_eq!(mix(&mut e, 1), [0.4, 0.4]);
}

#[test]
fn set_gain_on_a_live_voice_changes_the_next_buffer() {
    let mut e = engine();
    let v = e
        .play(mono(48_000, &[0.8; 4]), PlayOptions::default())
        .unwrap();
    assert_eq!(left(&mut e, 1), [0.8]);
    assert!(e.set_gain(v, 0.5));
    assert_eq!(left(&mut e, 1), [0.4]);
}

#[test]
fn mixing_into_a_larger_buffer_than_the_clip_zero_fills_the_rest() {
    let mut e = engine();
    e.play(mono(48_000, &[1.0]), PlayOptions::default())
        .unwrap();
    let out = mix(&mut e, 1024);
    assert_eq!(&out[..2], &[1.0, 1.0]);
    assert!(out[2..].iter().all(|s| *s == 0.0));
}

#[test]
fn eight_kilohertz_clip_on_192k_output_upsamples_by_24() {
    let mut e = offline(192_000, 2);
    let v = e
        .play(mono(8_000, &[0.0; 100]), PlayOptions::default())
        .unwrap();
    mix(&mut e, 240);
    let cursor = e.voice(v).unwrap().cursor;
    assert!((cursor - 10.0).abs() < 1e-9, "{cursor}");
}

#[test]
fn stereo_clip_through_pitch_and_resample_keeps_channels_distinct() {
    let mut e = engine();
    e.play(
        stereo(24_000, &[0.0, 1.0, 1.0, 0.0]),
        PlayOptions::default(),
    )
    .unwrap();
    near_all(&mix(&mut e, 2), &[0.0, 1.0, 0.5, 0.5]);
}
