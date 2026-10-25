//! Test 23 (DSP cost): 32 binaural acoustic voices with 8 reflection taps each
//! plus the FDN, at 48 kHz, must render 1 s of audio in at most 150 ms in a
//! release build. Measured for a static scene and with every source moving
//! (re-configured before every 512-frame mix). Debug builds skip the timing
//! (run with `--release`).

mod acoustic_util;

use acoustic_util::*;
use engine_audio::acoustic::{MixerEnvironment, ReflectionTap, ReverbParams, VoicePropagation};
use engine_audio::{AudioControl, AudioEngine, PlayOptions, VoiceHandle};
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_millis(150);

type Scene = (
    AudioEngine,
    AudioControl,
    Vec<(VoiceHandle, PlayOptions)>,
    engine_audio::acoustic::AcousticPublisher,
);

fn scene() -> Scene {
    let (engine, control, mut publisher) = setup(MixerEnvironment::default());
    let mut voices = Vec::new();
    for i in 0..32u32 {
        let angle = i as f32 * 0.7;
        let mut options =
            acoustic_options([angle.cos() * 8.0, (i % 3) as f32 - 1.0, angle.sin() * 8.0]);
        options.acoustics.propagation_delay = true;
        options.acoustics.air_absorption = true;
        options.gain = 0.03;
        let voice = control
            .play(clip(noise(9600, i, 0.5)), options.clone())
            .unwrap();
        voices.push((voice, options));
    }
    let mut handles: Vec<VoiceHandle> = voices.iter().map(|(v, _)| *v).collect();
    handles.sort();
    publisher
        .publish(|frame| {
            for (i, voice) in handles.iter().enumerate() {
                let mut p = VoicePropagation::new(*voice);
                p.direct = [0.9, 0.8, 0.6, 0.4];
                for t in 0..8 {
                    let a = (i * 8 + t) as f32 * 0.37;
                    p.taps[t] = ReflectionTap {
                        delay_s: 0.002 + 0.004 * t as f32,
                        gains: [0.3, 0.25, 0.2, 0.1],
                        direction: [a.cos(), 0.3 * a.sin(), a.sin()],
                    };
                }
                p.tap_count = 8;
                frame.voices.push(p);
            }
            frame.reverb = ReverbParams {
                enabled: true,
                rt60_s: [1.8, 1.5, 1.2, 0.8],
                level: [0.3; 4],
                predelay_s: 0.02,
                mean_free_path_m: 6.0,
            };
            Ok(())
        })
        .unwrap();
    (engine, control, voices, publisher)
}

/// Best of three 1 s renders; `moving` moves every source before each mix.
fn measure(moving: bool) -> Duration {
    let (mut engine, control, mut voices, _publisher) = scene();
    let mut out = vec![0.0f32; 1024];
    // Warm up: adopt the frame and let the taps fade in.
    for _ in 0..20 {
        engine.mix(&mut out).unwrap();
    }
    let stats = control.acoustic_stats().unwrap();
    assert_eq!(stats.acoustic_voices, 32);
    assert!(stats.reverb_active);
    let mut best = Duration::MAX;
    for _ in 0..3 {
        let started = Instant::now();
        for k in 0..(48_000 / 512) {
            if moving {
                for (voice, options) in &mut voices {
                    options.spatial.position[0] += 0.05 + 0.0005 * k as f32;
                    control.configure(*voice, options.clone()).unwrap();
                }
            }
            engine.mix(&mut out).unwrap();
        }
        best = best.min(started.elapsed());
    }
    best
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing is only meaningful in a release build"
)]
fn thirty_two_binaural_voices_with_taps_and_fdn_fit_the_budget() {
    let still = measure(false);
    let moving = measure(true);
    println!(
        "1 s of 32 binaural voices x 8 taps + FDN: static {:.1} ms, moving {:.1} ms (best of 3, budget 150 ms)",
        still.as_secs_f64() * 1000.0,
        moving.as_secs_f64() * 1000.0
    );
    assert!(still <= BUDGET, "static {still:?}");
    assert!(moving <= BUDGET, "moving {moving:?}");
}
