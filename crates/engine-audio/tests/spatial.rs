//! Spatial emitters: distance attenuation curves, constant-power panning,
//! listener orientation and validation. Measured through the offline mixer with
//! a constant-amplitude mono clip so output equals the per-channel gain.

mod common;

use common::*;
use engine_audio::{Attenuation, AudioEngine, Listener, PlayOptions, SpatialOptions, Vec3};

/// Plays a constant 1.0 mono loop and returns the first output frame [L, R].
fn gains(listener: Listener, spatial: SpatialOptions) -> [f32; 2] {
    let mut e = engine();
    e.set_listener(listener).unwrap();
    e.play(
        mono(48_000, &[1.0]),
        PlayOptions {
            looped: true,
            spatial,
            ..Default::default()
        },
    )
    .unwrap();
    let out = mix(&mut e, 1);
    [out[0], out[1]]
}

fn emitter(position: Vec3, attenuation: Attenuation) -> SpatialOptions {
    SpatialOptions {
        enabled: true,
        position,
        min_distance: 1.0,
        max_distance: 11.0,
        rolloff: 1.0,
        attenuation,
    }
}

fn at_origin() -> Listener {
    Listener::default()
}

fn assert_gains(actual: [f32; 2], expected: [f32; 2]) {
    near(actual[0], expected[0]);
    near(actual[1], expected[1]);
}

const CENTER: f32 = std::f32::consts::FRAC_1_SQRT_2;

macro_rules! attenuation_cases {
    ($($name:ident: $kind:expr, $distance:expr, $volume:expr;)*) => {$(
        #[test]
        fn $name() {
            // Source straight ahead so pan is centred and both channels carry volume/sqrt(2).
            let g = gains(at_origin(), emitter([0.0, 0.0, -$distance], $kind));
            let expected = $volume * CENTER;
            assert_gains(g, [expected, expected]);
        }
    )*};
}

attenuation_cases! {
    inverse_within_min_distance_is_unity: Attenuation::Inverse, 0.5f32, 1.0f32;
    inverse_at_min_distance_is_unity: Attenuation::Inverse, 1.0f32, 1.0f32;
    inverse_at_three_units_is_one_third: Attenuation::Inverse, 3.0f32, 1.0f32 / 3.0;
    inverse_at_nine_units_is_one_ninth: Attenuation::Inverse, 9.0f32, 1.0f32 / 9.0;
    inverse_just_inside_max_distance_is_still_audible: Attenuation::Inverse, 10.5f32, 1.0f32 / 10.5;
    linear_within_min_distance_is_unity: Attenuation::Linear, 0.25f32, 1.0f32;
    linear_midway_between_min_and_max_is_half: Attenuation::Linear, 6.0f32, 0.5f32;
    linear_at_quarter_span: Attenuation::Linear, 3.5f32, 0.75f32;
    linear_close_to_max_is_nearly_silent: Attenuation::Linear, 10.0f32, 0.1f32;
    none_ignores_distance_inside_range: Attenuation::None, 7.0f32, 1.0f32;
    none_ignores_distance_beyond_max: Attenuation::None, 500.0f32, 1.0f32;
}

macro_rules! silent_beyond_max {
    ($($name:ident: $kind:expr, $distance:expr;)*) => {$(
        #[test]
        fn $name() {
            let g = gains(at_origin(), emitter([0.0, 0.0, -$distance], $kind));
            assert_eq!(g, [0.0, 0.0]);
        }
    )*};
}

silent_beyond_max! {
    inverse_is_silent_at_exactly_max_distance: Attenuation::Inverse, 11.0f32;
    inverse_is_silent_far_beyond_max_distance: Attenuation::Inverse, 1000.0f32;
    linear_is_silent_at_exactly_max_distance: Attenuation::Linear, 11.0f32;
    linear_is_silent_beyond_max_distance: Attenuation::Linear, 12.0f32;
}

#[test]
fn rolloff_zero_disables_inverse_attenuation_inside_max_distance() {
    let mut s = emitter([0.0, 0.0, -8.0], Attenuation::Inverse);
    s.rolloff = 0.0;
    assert_gains(gains(at_origin(), s), [CENTER, CENTER]);
}

#[test]
fn rolloff_two_doubles_the_inverse_falloff_rate() {
    let mut s = emitter([0.0, 0.0, -3.0], Attenuation::Inverse);
    s.rolloff = 2.0;
    // 1 / (1 + 2 * (3 - 1)) = 0.2
    assert_gains(gains(at_origin(), s), [0.2 * CENTER, 0.2 * CENTER]);
}

#[test]
fn linear_rolloff_above_one_reaches_silence_before_max_distance() {
    let mut s = emitter([0.0, 0.0, -6.0], Attenuation::Linear);
    s.rolloff = 2.0;
    assert_eq!(gains(at_origin(), s), [0.0, 0.0]);
}

#[test]
fn larger_min_distance_keeps_full_volume_for_longer() {
    let mut s = emitter([0.0, 0.0, -4.0], Attenuation::Inverse);
    s.min_distance = 5.0;
    s.max_distance = 50.0;
    assert_gains(gains(at_origin(), s), [CENTER, CENTER]);
}

#[test]
fn distance_is_three_dimensional() {
    // (2, 2, 1) has length 3, so inverse volume is 1/3 regardless of axis mix.
    let g = gains(at_origin(), emitter([0.0, 0.0, 3.0], Attenuation::Inverse));
    let diagonal = gains(at_origin(), emitter([2.0, 2.0, 1.0], Attenuation::Inverse));
    near(g[0].hypot(g[1]), diagonal[0].hypot(diagonal[1]));
    near(diagonal[0].hypot(diagonal[1]), 1.0 / 3.0);
}

#[test]
fn source_to_the_right_plays_only_in_the_right_channel() {
    let g = gains(at_origin(), emitter([1.0, 0.0, 0.0], Attenuation::None));
    assert_gains(g, [0.0, 1.0]);
}

#[test]
fn source_to_the_left_plays_only_in_the_left_channel() {
    let g = gains(at_origin(), emitter([-4.0, 0.0, 0.0], Attenuation::None));
    assert_gains(g, [1.0, 0.0]);
}

#[test]
fn source_straight_ahead_is_centred_with_constant_power() {
    let g = gains(at_origin(), emitter([0.0, 0.0, -5.0], Attenuation::None));
    assert_gains(g, [CENTER, CENTER]);
}

#[test]
fn source_directly_behind_is_centred() {
    let g = gains(at_origin(), emitter([0.0, 0.0, 5.0], Attenuation::None));
    assert_gains(g, [CENTER, CENTER]);
}

#[test]
fn source_above_the_listener_is_centred() {
    let g = gains(at_origin(), emitter([0.0, 5.0, 0.0], Attenuation::None));
    assert_gains(g, [CENTER, CENTER]);
}

#[test]
fn source_on_the_listener_is_centred_with_no_division_by_zero() {
    let g = gains(at_origin(), emitter([0.0; 3], Attenuation::Inverse));
    assert!(g.iter().all(|v| v.is_finite()));
    assert_gains(g, [CENTER, CENTER]);
}

macro_rules! constant_power_pan {
    ($($name:ident: $x:expr, $z:expr, $pan:expr;)*) => {$(
        #[test]
        fn $name() {
            let g = gains(at_origin(), emitter([$x, 0.0, $z], Attenuation::None));
            let pan: f32 = $pan;
            near(g[0], ((1.0 - pan) / 2.0f32).sqrt());
            near(g[1], ((1.0 + pan) / 2.0f32).sqrt());
            near(g[0] * g[0] + g[1] * g[1], 1.0);
        }
    )*};
}

constant_power_pan! {
    pan_forty_five_degrees_right_of_ahead: 1.0f32, -1.0f32, std::f32::consts::FRAC_1_SQRT_2;
    pan_forty_five_degrees_left_of_ahead: -1.0f32, -1.0f32, -std::f32::consts::FRAC_1_SQRT_2;
    pan_thirty_degrees_right_of_ahead: 0.5f32, -0.866_025_4f32, 0.5f32;
    pan_slightly_left_of_ahead: -0.1f32, -0.994_987_4f32, -0.1f32;
}

#[test]
fn panning_is_independent_of_distance_once_attenuation_is_disabled() {
    let near_g = gains(at_origin(), emitter([1.0, 0.0, -1.0], Attenuation::None));
    let far_g = gains(at_origin(), emitter([40.0, 0.0, -40.0], Attenuation::None));
    assert_gains(far_g, near_g);
}

#[test]
fn listener_translation_moves_the_pan_origin() {
    let listener = Listener {
        position: [10.0, 0.0, 0.0],
        ..Default::default()
    };
    // Source at x=12 is 2 units to the listener's right; x=8 is to its left.
    assert_gains(
        gains(listener, emitter([12.0, 0.0, 0.0], Attenuation::None)),
        [0.0, 1.0],
    );
    assert_gains(
        gains(listener, emitter([8.0, 0.0, 0.0], Attenuation::None)),
        [1.0, 0.0],
    );
}

#[test]
fn listener_translation_changes_distance_attenuation() {
    let listener = Listener {
        position: [0.0, 0.0, -7.0],
        ..Default::default()
    };
    // The source is now 2 units ahead of the listener instead of 9.
    let g = gains(listener, emitter([0.0, 0.0, -9.0], Attenuation::Inverse));
    assert_gains(g, [0.5 * CENTER, 0.5 * CENTER]);
}

#[test]
fn rotating_the_listener_ninety_degrees_swaps_left_and_right() {
    // Facing +X with up=Y: right is +Z (forward x up).
    let listener = Listener {
        forward: [1.0, 0.0, 0.0],
        ..Default::default()
    };
    assert_gains(
        gains(listener, emitter([0.0, 0.0, -3.0], Attenuation::None)),
        [1.0, 0.0],
    );
    assert_gains(
        gains(listener, emitter([0.0, 0.0, 3.0], Attenuation::None)),
        [0.0, 1.0],
    );
    assert_gains(
        gains(listener, emitter([3.0, 0.0, 0.0], Attenuation::None)),
        [CENTER, CENTER],
    );
}

#[test]
fn listener_facing_backwards_mirrors_the_stereo_image() {
    let listener = Listener {
        forward: [0.0, 0.0, 1.0],
        ..Default::default()
    };
    assert_gains(
        gains(listener, emitter([2.0, 0.0, 0.0], Attenuation::None)),
        [1.0, 0.0],
    );
}

#[test]
fn rolling_the_listener_upside_down_mirrors_the_stereo_image() {
    let listener = Listener {
        up: [0.0, -1.0, 0.0],
        ..Default::default()
    };
    assert_gains(
        gains(listener, emitter([2.0, 0.0, 0.0], Attenuation::None)),
        [1.0, 0.0],
    );
}

#[test]
fn unnormalised_listener_vectors_pan_the_same_as_unit_vectors() {
    let scaled = Listener {
        forward: [0.0, 0.0, -50.0],
        up: [0.0, 0.25, 0.0],
        ..Default::default()
    };
    let source = emitter([1.0, 0.0, -1.0], Attenuation::None);
    assert_gains(gains(scaled, source), gains(at_origin(), source));
}

#[test]
fn non_orthogonal_up_vector_still_derives_the_same_right_axis() {
    let tilted = Listener {
        up: [0.0, 1.0, -0.5],
        ..Default::default()
    };
    assert_gains(
        gains(tilted, emitter([3.0, 0.0, 0.0], Attenuation::None)),
        [0.0, 1.0],
    );
}

#[test]
fn spatial_stereo_clip_is_downmixed_to_a_mono_emitter() {
    let mut e = engine();
    e.play(
        stereo(48_000, &[1.0, 0.0]),
        PlayOptions {
            looped: true,
            spatial: emitter([0.0, 0.0, -2.0], Attenuation::None),
            ..Default::default()
        },
    )
    .unwrap();
    // (1.0 + 0.0) / 2 = 0.5 per channel before centred panning.
    near_all(&mix(&mut e, 1), &[0.5 * CENTER, 0.5 * CENTER]);
}

#[test]
fn spatial_stereo_hard_left_clip_follows_the_pan_not_the_channel() {
    let mut e = engine();
    e.play(
        stereo(48_000, &[1.0, 1.0]),
        PlayOptions {
            looped: true,
            spatial: emitter([5.0, 0.0, 0.0], Attenuation::None),
            ..Default::default()
        },
    )
    .unwrap();
    near_all(&mix(&mut e, 1), &[0.0, 1.0]);
}

#[test]
fn non_spatial_stereo_clip_is_unaffected_by_listener_and_position() {
    let mut e = engine();
    e.set_listener(Listener {
        position: [100.0, 0.0, 0.0],
        forward: [1.0, 0.0, 0.0],
        ..Default::default()
    })
    .unwrap();
    let mut spatial = emitter([-50.0, 0.0, 0.0], Attenuation::Linear);
    spatial.enabled = false;
    e.play(
        stereo(48_000, &[0.3, -0.6]),
        PlayOptions {
            spatial,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(mix(&mut e, 1), [0.3, -0.6]);
}

#[test]
fn moving_the_listener_updates_playing_voices_on_the_next_mix() {
    let mut e = engine();
    e.play(
        mono(48_000, &[1.0]),
        PlayOptions {
            looped: true,
            spatial: emitter([5.0, 0.0, 0.0], Attenuation::None),
            ..Default::default()
        },
    )
    .unwrap();
    near_all(&mix(&mut e, 1), &[0.0, 1.0]);
    e.set_listener(Listener {
        position: [10.0, 0.0, 0.0],
        ..Default::default()
    })
    .unwrap();
    near_all(&mix(&mut e, 1), &[1.0, 0.0]);
}

#[test]
fn reconfiguring_a_voice_moves_its_emitter_live() {
    let mut e = engine();
    let options = |x: f32| PlayOptions {
        looped: true,
        spatial: emitter([x, 0.0, 0.0], Attenuation::None),
        ..Default::default()
    };
    let v = e.play(mono(48_000, &[1.0]), options(-5.0)).unwrap();
    near_all(&mix(&mut e, 1), &[1.0, 0.0]);
    assert!(e.configure(v, options(5.0)).unwrap());
    near_all(&mix(&mut e, 1), &[0.0, 1.0]);
}

#[test]
fn spatial_gain_composes_with_voice_and_bus_gain() {
    let mut e = engine();
    e.set_bus_gain(engine_audio::Bus::Effects, 0.5).unwrap();
    e.play(
        mono(48_000, &[1.0]),
        PlayOptions {
            looped: true,
            gain: 0.5,
            spatial: emitter([2.0, 0.0, 0.0], Attenuation::None),
            ..Default::default()
        },
    )
    .unwrap();
    near_all(&mix(&mut e, 1), &[0.0, 0.25]);
}

#[test]
fn reset_listener_restores_the_default_pose() {
    let e = engine();
    e.set_listener(Listener {
        position: [3.0, 4.0, 5.0],
        forward: [1.0, 0.0, 0.0],
        ..Default::default()
    })
    .unwrap();
    assert_ne!(e.listener().unwrap(), Listener::default());
    e.reset_listener().unwrap();
    assert_eq!(e.listener().unwrap(), Listener::default());
}

#[test]
fn rejected_listener_leaves_the_previous_pose_untouched() {
    let e = engine();
    let good = Listener {
        position: [1.0, 2.0, 3.0],
        ..Default::default()
    };
    e.set_listener(good).unwrap();
    let bad = Listener {
        forward: [0.0, 0.0, 0.0],
        ..good
    };
    assert_eq!(
        e.set_listener(bad).unwrap_err().code,
        "audio.listener.invalid"
    );
    assert_eq!(e.listener().unwrap(), good);
}

macro_rules! invalid_listeners {
    ($($name:ident: $listener:expr;)*) => {$(
        #[test]
        fn $name() {
            let listener: Listener = $listener;
            assert_eq!(listener.validate().unwrap_err().code, "audio.listener.invalid");
        }
    )*};
}

invalid_listeners! {
    listener_rejects_nan_position: Listener { position: [f32::NAN, 0.0, 0.0], ..Default::default() };
    listener_rejects_infinite_position: Listener { position: [0.0, f32::INFINITY, 0.0], ..Default::default() };
    listener_rejects_nan_forward: Listener { forward: [0.0, f32::NAN, -1.0], ..Default::default() };
    listener_rejects_infinite_up: Listener { up: [0.0, f32::NEG_INFINITY, 0.0], ..Default::default() };
    listener_rejects_zero_forward: Listener { forward: [0.0; 3], ..Default::default() };
    listener_rejects_tiny_forward: Listener { forward: [0.0, 0.0, -1e-7], ..Default::default() };
    listener_rejects_zero_up: Listener { up: [0.0; 3], ..Default::default() };
    listener_rejects_parallel_forward_and_up: Listener { forward: [0.0, 2.0, 0.0], ..Default::default() };
    listener_rejects_antiparallel_forward_and_up: Listener { forward: [0.0, -3.0, 0.0], ..Default::default() };
}

#[test]
fn listener_accepts_perpendicular_oblique_and_far_positions() {
    for listener in [
        Listener::default(),
        Listener {
            position: [1e6, -1e6, 1e6],
            ..Default::default()
        },
        Listener {
            forward: [1.0, 0.0, 0.0],
            up: [0.0, 0.0, 1.0],
            ..Default::default()
        },
        Listener {
            forward: [1.0, 1.0, 0.0],
            up: [0.0, 1.0, 0.0],
            ..Default::default()
        },
        Listener {
            forward: [0.0, 0.0, -1e-3],
            up: [0.0, 1e3, 0.0],
            ..Default::default()
        },
    ] {
        listener.validate().unwrap();
    }
}

#[test]
fn nearly_parallel_listener_axes_are_rejected_but_slightly_skewed_ones_pass() {
    let skewed = Listener {
        forward: [0.01, 0.0, -1.0],
        ..Default::default()
    };
    skewed.validate().unwrap();
    let almost_parallel = Listener {
        forward: [1e-4, 1.0, 0.0],
        up: [0.0, 1.0, 0.0],
        ..Default::default()
    };
    assert!(almost_parallel.validate().is_err());
}

macro_rules! invalid_play_spatial {
    ($($name:ident: $edit:expr;)*) => {$(
        #[test]
        fn $name() {
            let e = engine();
            let mut s = emitter([0.0, 0.0, -1.0], Attenuation::Inverse);
            let edit: fn(&mut SpatialOptions) = $edit;
            edit(&mut s);
            let error = e.play(mono(48_000, &[1.0]), PlayOptions { spatial: s, ..Default::default() }).unwrap_err();
            assert_eq!(error.code, "audio.play.invalid");
            assert_eq!(e.voice_count(), 0);
        }
    )*};
}

invalid_play_spatial! {
    spatial_rejects_nan_position: |s| s.position[1] = f32::NAN;
    spatial_rejects_infinite_position: |s| s.position[2] = f32::INFINITY;
    spatial_rejects_zero_min_distance: |s| s.min_distance = 0.0;
    spatial_rejects_negative_min_distance: |s| s.min_distance = -1.0;
    spatial_rejects_nan_min_distance: |s| s.min_distance = f32::NAN;
    spatial_rejects_max_equal_to_min: |s| s.max_distance = s.min_distance;
    spatial_rejects_max_below_min: |s| s.max_distance = 0.5;
    spatial_rejects_infinite_max_distance: |s| s.max_distance = f32::INFINITY;
    spatial_rejects_negative_rolloff: |s| s.rolloff = -0.1;
    spatial_rejects_nan_rolloff: |s| s.rolloff = f32::NAN;
}

#[test]
fn spatial_validation_applies_even_when_the_emitter_is_disabled() {
    let e = engine();
    let mut s = emitter([0.0; 3], Attenuation::Inverse);
    s.enabled = false;
    s.min_distance = -3.0;
    let error = e
        .play(
            mono(48_000, &[1.0]),
            PlayOptions {
                spatial: s,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.code, "audio.play.invalid");
}

#[test]
fn default_spatial_options_are_disabled_inverse_with_unit_min_distance() {
    let s = SpatialOptions::default();
    assert!(!s.enabled);
    assert_eq!(s.attenuation, Attenuation::Inverse);
    assert_eq!(
        (s.min_distance, s.max_distance, s.rolloff),
        (1.0, 100.0, 1.0)
    );
    assert_eq!(s.position, [0.0; 3]);
}

#[test]
fn engine_handle_is_shared_so_listener_set_via_control_is_visible_on_the_engine() {
    let e: AudioEngine = engine();
    let control = e.control();
    control
        .set_listener(Listener {
            position: [9.0, 0.0, 0.0],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(e.listener().unwrap().position, [9.0, 0.0, 0.0]);
}
