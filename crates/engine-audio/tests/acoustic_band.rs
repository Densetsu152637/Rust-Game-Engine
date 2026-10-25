//! Test 2 (band filter), plus occlusion/transmission and air absorption applied
//! end to end: rendered white noise through an acoustic voice, H1 analysis.

mod acoustic_util;

use acoustic_util::*;
use engine_audio::acoustic::{
    BAND_CENTERS_HZ, MixerEnvironment, ReverbRenderer, SpatialRenderer, iso9613_air_np_per_m,
};

const NOISE: usize = 1 << 17;
const SEG: usize = 8192;

/// Renders noise through a frontal StereoPan voice whose published direct
/// gains are `gains`; returns (input, left output aligned to input).
fn render_gains(
    gains: [f32; 4],
    env: MixerEnvironment,
    setup_voice: impl Fn(&mut engine_audio::PlayOptions),
) -> (Vec<f32>, Vec<f32>) {
    let (mut engine, control, mut publisher) = setup(env);
    let input = noise(NOISE, 7, 0.5);
    let mut options = acoustic_options([0.0, 0.0, -1.0]);
    setup_voice(&mut options);
    let voice = control.play(clip(input.clone()), options).unwrap();
    publish_voices(&mut publisher, &[(voice, &|p| p.direct = gains)], None);
    let (left, _) = render(&mut engine, NOISE, 512);
    (input, left)
}

fn pan_env() -> MixerEnvironment {
    environment(SpatialRenderer::StereoPan, ReverbRenderer::Off)
}

/// Measured gain relative to the frontal pan gain, in dB.
fn measured_db(input: &[f32], left: &[f32], freq: f64) -> f64 {
    db(transfer(input, left, freq, SEG) / std::f64::consts::FRAC_1_SQRT_2)
}

#[test]
fn requested_band_gains_hold_at_the_centres() {
    let sets: [[f32; 4]; 4] = [
        // Occlusion test 14 transmission levels: -5.2, -10, -15.2, -20 dB.
        [0.549, 0.316, 0.174, 0.1],
        [1.0, 0.25, 1.0, 0.1],
        [0.1, 1.0, 0.1, 1.0],
        [1.0, 0.5, 0.25, 0.125],
    ];
    for gains in sets {
        let (input, left) = render_gains(gains, pan_env(), |_| {});
        for b in 0..4 {
            let got = measured_db(&input, &left, f64::from(BAND_CENTERS_HZ[b]));
            let want = db(f64::from(gains[b]));
            println!("{gains:?} band {b}: {got:.3} dB (want {want:.3})");
            assert!((got - want).abs() <= 1.0, "band {b}: {got} vs {want}");
        }
    }
}

#[test]
fn unity_sum_is_flat() {
    let (input, left) = render_gains([1.0; 4], pan_env(), |_| {});
    let mut worst = 0.0f64;
    let mut f = 40.0;
    while f < 18_000.0 {
        let got = measured_db(&input, &left, f);
        worst = worst.max(got.abs());
        f *= 2f64.powf(1.0 / 6.0);
    }
    println!("unity worst deviation {worst:.4} dB");
    assert!(worst <= 0.5, "{worst}");
}

#[test]
fn air_absorption_follows_iso9613_over_distance() {
    let air = iso9613_air_np_per_m(20.0, 50.0, 101.325).unwrap();
    let env = MixerEnvironment {
        air_np_per_m: air,
        ..pan_env()
    };
    let distance = 200.0f32;
    let (input, left) = render_gains([1.0; 4], env, |o| {
        o.spatial.position = [0.0, 0.0, -distance];
        o.acoustics.air_absorption = true;
    });
    for b in 0..4 {
        let got = measured_db(&input, &left, f64::from(BAND_CENTERS_HZ[b]));
        let want = db((-f64::from(air[b]) * f64::from(distance)).exp());
        println!("air band {b}: {got:.3} dB (want {want:.3})");
        assert!((got - want).abs() <= 1.0, "band {b}: {got} vs {want}");
    }
}
