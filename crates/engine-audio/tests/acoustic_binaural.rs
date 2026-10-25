//! Test 4 (binaural ITD/ILD) on the default renderer, which is binaural.

mod acoustic_util;

use acoustic_util::*;
use engine_audio::acoustic::{MixerEnvironment, SpatialRenderer};

const WOODWORTH_MS: f64 = 0.0875 / 343.0 * (std::f64::consts::FRAC_PI_2 + 1.0) * 1000.0;

/// Left and right ear signals for white noise from `position`.
fn ears(position: [f32; 3]) -> (Vec<f32>, Vec<f32>) {
    let (mut engine, control, _publisher) = setup(MixerEnvironment::default());
    assert_eq!(
        control.environment().unwrap().renderer,
        SpatialRenderer::Binaural
    );
    control
        .play(clip(noise(1 << 15, 3, 0.3)), acoustic_options(position))
        .unwrap();
    let (l, r) = render(&mut engine, 48_000, 512);
    (l[4800..].to_vec(), r[4800..].to_vec())
}

/// (ITD in ms, positive = right ear leads; ILD in dB at 8 kHz and 125 Hz,
/// positive = right louder).
fn cues(position: [f32; 3]) -> (f64, f64, f64) {
    let (l, r) = ears(position);
    let itd = lag(&l, &r, 60) / FS * 1000.0;
    let third = 2f64.powf(1.0 / 6.0);
    let ild = |f: f64| {
        10.0 * (band_energy(&r, f / third, f * third) / band_energy(&l, f / third, f * third))
            .log10()
    };
    (itd, ild(8000.0), ild(125.0))
}

#[test]
fn source_at_ninety_degrees_has_woodworth_itd_and_head_shadow() {
    let (itd, ild_hi, ild_lo) = cues([10.0, 0.0, 0.0]);
    println!(
        "+90 deg: ITD {itd:.4} ms (Woodworth {WOODWORTH_MS:.4}), ILD 8k {ild_hi:.2} dB, 125 {ild_lo:.2} dB"
    );
    assert!((itd - 0.656).abs() <= 0.05, "{itd}");
    assert!(ild_hi >= 6.0, "{ild_hi}");
    assert!(ild_lo.abs() < 3.0, "{ild_lo}");
}

#[test]
fn frontal_source_has_no_interaural_difference() {
    let (itd, ild_hi, _) = cues([0.0, 0.0, -10.0]);
    println!("front: ITD {itd:.5} ms, ILD 8k {ild_hi:.3} dB");
    assert!(itd.abs() < 0.02, "{itd}");
    assert!(ild_hi.abs() < 0.5, "{ild_hi}");
}

#[test]
fn mirrored_source_mirrors_the_cues() {
    let right = cues([10.0, 0.0, 0.0]);
    let left = cues([-10.0, 0.0, 0.0]);
    println!("-90 deg: ITD {:.4} ms, ILD 8k {:.2} dB", left.0, left.1);
    assert!((left.0 + right.0).abs() < 0.01, "{left:?} {right:?}");
    assert!((left.1 + right.1).abs() < 0.5, "{left:?} {right:?}");
    assert!(left.0 < 0.0 && left.1 < 0.0);
}
