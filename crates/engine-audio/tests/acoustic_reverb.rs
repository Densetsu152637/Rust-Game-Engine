//! Test 3 (FDN RT60 per octave band) and the late-level normalisation.

mod acoustic_util;

use acoustic_util::*;
use engine_audio::acoustic::{BAND_CENTERS_HZ, ReverbParams, ReverbRenderer, SpatialRenderer};

/// Impulse response of the late field alone (direct path muted).
fn late_ir(rt60: [f32; 4], level: [f32; 4], seconds: f64) -> (Vec<f32>, Vec<f32>) {
    let (mut engine, control, mut publisher) = setup(environment(
        SpatialRenderer::StereoPan,
        ReverbRenderer::Parametric,
    ));
    let mut options = acoustic_options([0.0, 0.0, -1.0]);
    options.looped = false;
    let voice = control.play(clip(vec![1.0]), options).unwrap();
    publish_voices(
        &mut publisher,
        &[(voice, &|p| p.direct = [0.0; 4])],
        Some(ReverbParams {
            enabled: true,
            rt60_s: rt60,
            level,
            predelay_s: 0.0,
            mean_free_path_m: 5.0,
        }),
    );
    render(&mut engine, (seconds * FS) as usize, 512)
}

#[test]
fn fdn_decay_matches_target_rt60_per_octave() {
    let target = [2.0f32, 1.5, 1.0, 0.5];
    let (left, right) = late_ir(target, [1.0; 4], 4.5);
    for (name, channel) in [("left", &left), ("right", &right)] {
        for b in 0..4 {
            let band = octave_band(channel, f64::from(BAND_CENTERS_HZ[b]));
            let t = t30(&band);
            let err = t / f64::from(target[b]) - 1.0;
            println!(
                "{name} {} Hz: T30 {t:.3} s (target {}) {:+.1} %",
                BAND_CENTERS_HZ[b],
                target[b],
                err * 100.0
            );
            assert!(err.abs() <= 0.10, "{name} band {b}: {t}");
        }
    }
}

#[test]
fn late_energy_follows_level_and_channels_decorrelate() {
    let (left, right) = late_ir([1.0; 4], [0.5; 4], 3.0);
    let energy = |x: &[f32]| x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
    let (el, er) = (energy(&left), energy(&right));
    println!("late energy L {el:.3} R {er:.3} (level 0.5)");
    // The FDN normalisation is an equipartition estimate: within 2 dB.
    for e in [el, er] {
        assert!((10.0 * (e / 0.5).log10()).abs() < 2.0, "{e}");
    }
    let cross: f64 = left
        .iter()
        .zip(&right)
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum();
    assert!(
        (cross / (el * er).sqrt()).abs() < 0.3,
        "outputs are decorrelated"
    );
}

#[test]
fn disabled_reverb_is_silent_and_returns_to_legacy_mixing() {
    let (mut engine, control, mut publisher) = setup(environment(
        SpatialRenderer::StereoPan,
        ReverbRenderer::Parametric,
    ));
    let mut options = acoustic_options([0.0, 0.0, -1.0]);
    options.looped = false;
    let voice = control.play(clip(vec![1.0]), options).unwrap();
    publish_voices(&mut publisher, &[(voice, &|p| p.direct = [0.0; 4])], None);
    let (left, _) = render(&mut engine, 48_000, 512);
    assert!(left.iter().all(|s| *s == 0.0));
    assert_eq!(control.voice_count(), 0);
    assert!(!control.acoustic_stats().unwrap().reverb_active);
}
