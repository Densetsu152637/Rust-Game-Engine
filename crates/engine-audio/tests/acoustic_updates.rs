//! Test 7 (click-free updates), test 8 (the mix never waits for the writer),
//! early-reflection taps, the per-bus effect hook, the acoustic voice cap and
//! publisher reclamation.

mod acoustic_util;

use acoustic_util::*;
use engine_audio::acoustic::{
    BinauralSettings, DspEffect, EffectContext, MixerEnvironment, PARAM_RAMP_FRAMES, ReflectionTap,
    ReverbParams, ReverbRenderer, SpatialRenderer,
};
use engine_audio::{Bus, PlayOptions};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Steps the published direct gain from 1 to 0.1 on a settled DC source and
/// returns (largest sample-to-sample jump, bound, final/initial ratio).
fn gain_step(env: MixerEnvironment) -> (f64, f64, f64) {
    let (mut engine, control, mut publisher) = setup(env);
    let voice = control
        .play(clip(vec![0.5; 4800]), acoustic_options([0.0, 0.0, -2.0]))
        .unwrap();
    publish_voices(&mut publisher, &[(voice, &|p| p.direct = [1.0; 4])], None);
    let (settled, _) = render(&mut engine, 24_000, 512);
    let peak = f64::from(settled[settled.len() - 1]);
    publish_voices(&mut publisher, &[(voice, &|p| p.direct = [0.1; 4])], None);
    let (after, _) = render(&mut engine, 4_800, 512);
    let mut series = settled[settled.len() - 2048..].to_vec();
    series.extend(&after);
    let worst = series
        .windows(2)
        .map(|w| f64::from((w[1] - w[0]).abs()))
        .fold(0.0, f64::max);
    let bound = (1.0 - 0.1) / PARAM_RAMP_FRAMES as f64 * peak + 1e-6;
    (worst, bound, f64::from(after[after.len() - 1]) / peak)
}

#[test]
fn direct_gain_steps_ramp_without_clicks() {
    let pan = environment(SpatialRenderer::StereoPan, ReverbRenderer::Off);
    let binaural = MixerEnvironment {
        binaural: BinauralSettings {
            pinna_echoes: false,
            ..Default::default()
        },
        ..environment(SpatialRenderer::Binaural, ReverbRenderer::Off)
    };
    // Binaural ears read the delay line at a fractional ITD; cubic Hermite
    // interpolation of the ramp's corners overshoots the slope by a few
    // percent for a couple of samples, so that variant gets a 5 % allowance.
    for (name, env, slack) in [("stereo pan", pan, 1.0), ("binaural", binaural, 1.05)] {
        let (worst, bound, ratio) = gain_step(env);
        println!("{name}: largest step {worst:.4e}, bound {bound:.4e}, final ratio {ratio:.5}");
        assert!(worst <= bound * slack, "{name}: {worst} > {bound}");
        assert!((ratio - 0.1).abs() < 1e-4, "{name}: {ratio}");
    }
}

#[test]
fn stalled_writer_never_blocks_the_mix() {
    let (mut engine, control, mut publisher) = setup(environment(
        SpatialRenderer::Binaural,
        ReverbRenderer::Parametric,
    ));
    let voice = control
        .play(
            clip(noise(4800, 5, 0.2)),
            acoustic_options([2.0, 0.0, -3.0]),
        )
        .unwrap();
    control
        .play(
            clip(noise(4800, 6, 0.2)),
            PlayOptions {
                looped: true,
                ..Default::default()
            },
        )
        .unwrap();
    let reverb = ReverbParams {
        enabled: true,
        rt60_s: [0.8; 4],
        level: [0.2; 4],
        predelay_s: 0.01,
        mean_free_path_m: 4.0,
    };
    publish_voices(
        &mut publisher,
        &[(voice, &|p| p.direct = [0.5; 4])],
        Some(reverb),
    );
    render(&mut engine, 512, 512);
    let adopted = control.acoustic_stats().unwrap().frames_adopted;
    // A newer frame is waiting while the writer holds the slot lock.
    publish_voices(
        &mut publisher,
        &[(voice, &|p| p.direct = [1.0; 4])],
        Some(reverb),
    );
    let slowest = publisher
        .stall_for_test(|| {
            let mut out = vec![0.0f32; 1024];
            let mut slowest = Duration::ZERO;
            for _ in 0..200 {
                let started = Instant::now();
                engine.mix(&mut out).unwrap();
                slowest = slowest.max(started.elapsed());
                assert!(out.iter().any(|s| *s != 0.0));
            }
            slowest
        })
        .unwrap();
    let misses = publisher.stats().audio_try_lock_misses;
    println!("slowest stalled mix {slowest:?}, try-lock misses {misses}");
    assert!(slowest < Duration::from_millis(5), "{slowest:?}");
    assert!(misses >= 200);
    assert_eq!(
        control.acoustic_stats().unwrap().frames_adopted,
        adopted,
        "stale parameters kept"
    );
    render(&mut engine, 512, 512);
    assert_eq!(
        control.acoustic_stats().unwrap().frames_adopted,
        adopted + 1,
        "adopted once free"
    );
}

#[test]
fn reflection_taps_arrive_at_their_delay_from_their_direction() {
    let (mut engine, control, mut publisher) =
        setup(environment(SpatialRenderer::StereoPan, ReverbRenderer::Off));
    let mut options = acoustic_options([0.0, 0.0, -1.0]);
    options.looped = false;
    let voice = control.play(clip(vec![1.0]), options).unwrap();
    publish_voices(
        &mut publisher,
        &[(voice, &|p| {
            p.direct = [0.0; 4];
            p.taps[0] = ReflectionTap {
                delay_s: 0.01,
                gains: [0.5; 4],
                direction: [-1.0, 0.0, 0.0],
            };
            p.tap_count = 1;
        })],
        None,
    );
    let (left, right) = render(&mut engine, 4800, 512);
    // The tap fades in over the parameter ramp, so measure a delayed copy:
    // energy lands at 480 samples, in the left channel only.
    let energy =
        |x: &[f32], a: usize, b: usize| x[a..b].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
    let gain = (480.0 / PARAM_RAMP_FRAMES as f64) * 0.5;
    let el = energy(&left, 470, 1200);
    println!(
        "tap energy L {el:.5} (expected {:.5}), R {:.2e}",
        gain * gain,
        energy(&right, 0, 4800)
    );
    assert!((el / (gain * gain) - 1.0).abs() < 0.05, "{el}");
    assert!(energy(&left, 0, 470) < 1e-12);
    assert!(energy(&right, 0, 4800) < 1e-12);
}

struct Invert(Arc<Mutex<Vec<(Bus, usize, u64)>>>);
impl DspEffect for Invert {
    fn process(&mut self, frames: &mut [[f32; 2]], context: &EffectContext) {
        if let Ok(mut log) = self.0.try_lock()
            && log.len() < log.capacity()
        {
            log.push((context.bus, frames.len(), context.frame_cursor));
        }
        for f in frames {
            f[0] = -f[0];
            f[1] = -f[1];
        }
    }
}

#[test]
fn bus_effects_process_their_bus_and_restore_legacy_output_when_removed() {
    let mut engine = engine();
    let control = engine.control();
    let music = PlayOptions {
        looped: true,
        bus: Bus::Music,
        ..Default::default()
    };
    control.play(clip(vec![0.25; 100]), music).unwrap();
    control
        .play(
            clip(vec![0.125; 100]),
            PlayOptions {
                looped: true,
                ..Default::default()
            },
        )
        .unwrap();
    let (legacy, _) = render(&mut engine, 600, 600);
    let log = Arc::new(Mutex::new(Vec::with_capacity(16)));
    assert!(
        control
            .set_bus_effect(Bus::Music, Some(Box::new(Invert(log.clone()))))
            .unwrap()
            .is_none()
    );
    let (with, _) = render(&mut engine, 600, 600);
    assert!(with.iter().all(|s| (s - (-0.25 + 0.125)).abs() < 1e-7));
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log,
        vec![
            (Bus::Music, 256, 600),
            (Bus::Music, 256, 856),
            (Bus::Music, 88, 1112)
        ]
    );
    let old = control.set_bus_effect(Bus::Music, None).unwrap();
    assert!(old.is_some());
    let (after, _) = render(&mut engine, 600, 600);
    assert_eq!(after, legacy);
}

#[test]
fn voices_over_the_acoustic_cap_use_the_legacy_path() {
    let engine = engine();
    let control = engine.control();
    control.set_max_acoustic_voices(1).unwrap();
    for _ in 0..3 {
        control
            .play(clip(vec![0.1; 100]), acoustic_options([1.0, 0.0, 0.0]))
            .unwrap();
    }
    // Acoustics need a spatial voice; non-spatial ones stay legacy silently.
    let mut flat = acoustic_options([0.0; 3]);
    flat.spatial.enabled = false;
    control.play(clip(vec![0.1; 100]), flat).unwrap();
    let stats = control.acoustic_stats().unwrap();
    assert_eq!((stats.acoustic_voices, stats.over_cap_fallbacks), (1, 2));
    assert_eq!(
        control.set_max_acoustic_voices(300).unwrap_err().code,
        "audio.options.invalid"
    );
}

#[test]
fn dropped_publisher_goes_dry_and_can_be_obtained_again() {
    let (mut engine, control, mut publisher) =
        setup(environment(SpatialRenderer::StereoPan, ReverbRenderer::Off));
    let voice = control
        .play(clip(vec![0.5; 4800]), acoustic_options([0.0, 0.0, -1.0]))
        .unwrap();
    assert_eq!(
        control.acoustic_publisher().unwrap_err().code,
        "audio.acoustic.publisher_taken"
    );
    publish_voices(&mut publisher, &[(voice, &|p| p.direct = [0.0; 4])], None);
    let (muted, _) = render(&mut engine, 4800, 512);
    assert!(muted[4799].abs() < 1e-6);
    drop(publisher);
    let (dry, _) = render(&mut engine, 4800, 512);
    assert!(
        (dry[4799] - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4,
        "{}",
        dry[4799]
    );
    let mut again = control.acoustic_publisher().unwrap();
    publish_voices(&mut again, &[(voice, &|p| p.direct = [0.0; 4])], None);
    let (muted, _) = render(&mut engine, 4800, 512);
    assert!(muted[4799].abs() < 1e-6);
}
