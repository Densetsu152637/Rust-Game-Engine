//! Test 5 (delay-line Doppler) and propagation latency.

mod acoustic_util;

use acoustic_util::*;
use engine_audio::acoustic::{MixerEnvironment, ReverbRenderer, SpatialRenderer};
use engine_audio::{Listener, PlayOptions};

const C: f64 = 343.0;
const SPEED: f64 = 20.0;
const UPDATE: usize = 256;

fn sine_1k() -> Vec<f32> {
    // 48 samples per period; 100 periods loop seamlessly.
    (0..4800)
        .map(|i| (2.0 * std::f64::consts::PI * i as f64 / 48.0).sin() as f32 * 0.5)
        .collect()
}

/// Moves a 1 kHz source along +X through the listener at `SPEED` m/s
/// (`moving_listener` moves the listener instead), updating positions every
/// `UPDATE` frames, and returns the sum of both channels.
fn fly_by(env: MixerEnvironment, moving_listener: bool) -> Vec<f32> {
    let (mut engine, control, _publisher) = setup(env);
    let mut options: PlayOptions = acoustic_options([-60.0, 0.0, 0.0]);
    options.acoustics.propagation_delay = true;
    let start = if moving_listener {
        [60.0, 0.0, 0.0]
    } else {
        [-60.0, 0.0, 0.0]
    };
    options.spatial.position = if moving_listener { [0.0; 3] } else { start };
    let voice = control.play(clip(sine_1k()), options.clone()).unwrap();
    let seconds = 6.0;
    let blocks = (seconds * FS) as usize / UPDATE;
    let mut left = Vec::with_capacity(blocks * UPDATE);
    let mut out = vec![0.0f32; UPDATE * 2];
    for k in 0..blocks {
        let t = (k * UPDATE) as f64 / FS;
        let x = if moving_listener {
            60.0 - SPEED * t
        } else {
            -60.0 + SPEED * t
        } as f32;
        if moving_listener {
            control
                .set_listener(Listener {
                    position: [x, 0.0, 0.0],
                    ..Default::default()
                })
                .unwrap();
        } else {
            options.spatial.position = [x, 0.0, 0.0];
            control.configure(voice, options.clone()).unwrap();
        }
        engine.mix(&mut out).unwrap();
        left.extend(out.chunks(2).map(|f| f[0] + f[1]));
    }
    left
}

fn window(x: &[f32], start_s: f64) -> &[f32] {
    let a = (start_s * FS) as usize;
    &x[a..a + (0.1 * FS) as usize]
}

fn check(name: &str, measured: f64, expected: f64) {
    let err = measured / expected - 1.0;
    println!(
        "{name}: {measured:.3} Hz (expected {expected:.3}) {:+.4} %",
        err * 100.0
    );
    assert!(err.abs() <= 0.005, "{name}");
}

#[test]
fn moving_source_shifts_pitch_by_c_over_c_minus_v() {
    for renderer in [SpatialRenderer::Binaural, SpatialRenderer::StereoPan] {
        let left = fly_by(environment(renderer, ReverbRenderer::Off), false);
        check(
            "approaching",
            zero_crossing_frequency(window(&left, 1.0)),
            1000.0 * C / (C - SPEED),
        );
        check(
            "receding",
            zero_crossing_frequency(window(&left, 5.0)),
            1000.0 * C / (C + SPEED),
        );
    }
}

#[test]
fn moving_listener_shifts_pitch_by_c_plus_v_over_c() {
    let left = fly_by(MixerEnvironment::default(), true);
    check(
        "listener approaching",
        zero_crossing_frequency(window(&left, 1.0)),
        1000.0 * (C + SPEED) / C,
    );
    check(
        "listener receding",
        zero_crossing_frequency(window(&left, 5.0)),
        1000.0 * (C - SPEED) / C,
    );
}

#[test]
fn propagation_delay_matches_distance_over_c() {
    let (mut engine, control, _publisher) =
        setup(environment(SpatialRenderer::StereoPan, ReverbRenderer::Off));
    let mut options = acoustic_options([0.0, 0.0, -34.3]);
    options.looped = false;
    options.acoustics.propagation_delay = true;
    control.play(clip(vec![1.0]), options).unwrap();
    let (left, _) = render(&mut engine, 9600, 512);
    let peak = left
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0;
    // 34.3 m at 343 m/s is 4800 samples, plus the one-sample interpolation guard.
    println!("arrival at sample {peak}");
    assert!((peak as i64 - 4801).abs() <= 2, "{peak}");
    // The one-shot voice survives until its propagation tail has played.
    assert_eq!(control.voice_count(), 0);
}
