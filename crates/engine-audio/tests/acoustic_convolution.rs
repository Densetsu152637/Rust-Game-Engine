//! Test 6 (partitioned FFT convolution) through the mixer's convolution
//! renderer: listener kernel accuracy, kernel swap cross-fade and hero voices.

mod acoustic_util;

use acoustic_util::*;
use engine_audio::acoustic::{
    AcousticPublisher, CONVOLUTION_BLOCK, ConvolutionKernel, ReverbRenderer, SpatialRenderer,
    StereoImpulseResponse,
};
use engine_audio::{AudioEngine, VoiceHandle};

fn kernel(left: Vec<f32>, right: Vec<f32>) -> Box<ConvolutionKernel> {
    Box::new(
        ConvolutionKernel::prepare(
            &StereoImpulseResponse {
                sample_rate: RATE,
                left,
                right,
            },
            RATE,
        )
        .unwrap(),
    )
}

fn publish_listener_kernel(
    p: &mut AcousticPublisher,
    voice: VoiceHandle,
    k: Box<ConvolutionKernel>,
) {
    p.publish(|frame| {
        let mut v = engine_audio::acoustic::VoicePropagation::new(voice);
        v.direct = [0.0; 4];
        frame.voices.push(v);
        frame.listener_kernel = Some(k);
        Ok(())
    })
    .unwrap();
}

fn conv_setup(input: Vec<f32>) -> (AudioEngine, AcousticPublisher, VoiceHandle) {
    let (engine, control, publisher) = setup(environment(
        SpatialRenderer::StereoPan,
        ReverbRenderer::Convolution,
    ));
    let voice = control
        .play(clip(input), acoustic_options([0.0, 0.0, -1.0]))
        .unwrap();
    (engine, publisher, voice)
}

#[test]
fn listener_kernel_matches_direct_convolution() {
    let input = noise(24_000, 11, 0.3);
    let hl: Vec<f32> = noise(3000, 12, 0.01)
        .iter()
        .enumerate()
        .map(|(i, v)| v * (-(i as f32) / 800.0).exp())
        .collect();
    let hr: Vec<f32> = noise(3000, 13, 0.01);
    let (mut engine, mut publisher, voice) = conv_setup(input.clone());
    publish_listener_kernel(&mut publisher, voice, kernel(hl.clone(), hr.clone()));
    let (left, right) = render(&mut engine, 20_000, 300);
    // Send = source delayed by the one-sample read guard; wet path adds one block.
    let latency = CONVOLUTION_BLOCK + 1;
    let mut peak = 0.0f64;
    let mut worst = 0.0f64;
    // The first wet block fades the newly adopted kernel in; compare after it.
    for n in latency + CONVOLUTION_BLOCK..left.len() {
        let m = n - latency;
        let direct = |h: &[f32]| -> f64 {
            (0..h.len().min(m + 1))
                .map(|k| f64::from(h[k]) * f64::from(input[m - k]))
                .sum()
        };
        let (l, r) = (direct(&hl), direct(&hr));
        peak = peak.max(l.abs()).max(r.abs());
        worst = worst
            .max((f64::from(left[n]) - l).abs())
            .max((f64::from(right[n]) - r).abs());
    }
    println!("max relative error {:.3e} (peak {peak:.4})", worst / peak);
    assert!(worst / peak <= 1e-5, "{}", worst / peak);
}

#[test]
fn kernel_swap_cross_fades_within_one_block() {
    let (mut engine, mut publisher, voice) = conv_setup(vec![0.1; 4800]);
    publish_listener_kernel(&mut publisher, voice, kernel(vec![0.5], vec![0.5]));
    let (before, _) = render(&mut engine, 24_000, 512);
    assert!((before[before.len() - 1] - 0.05).abs() < 1e-6);
    publish_listener_kernel(&mut publisher, voice, kernel(vec![0.25], vec![0.25]));
    let (after, _) = render(&mut engine, 4_800, 512);
    let bound = (0.05 - 0.025) / CONVOLUTION_BLOCK as f64 + 1e-6;
    let mut series = vec![before[before.len() - 1]];
    series.extend(&after);
    let worst = series
        .windows(2)
        .map(|w| f64::from((w[1] - w[0]).abs()))
        .fold(0.0, f64::max);
    println!("largest step {worst:.3e} (bound {bound:.3e})");
    assert!(worst <= bound, "{worst}");
    assert!((after[after.len() - 1] - 0.025).abs() < 1e-6);
}

#[test]
fn identical_republished_kernel_does_not_restart_the_fade() {
    let (mut engine, mut publisher, voice) = conv_setup(vec![0.1; 4800]);
    publish_listener_kernel(&mut publisher, voice, kernel(vec![0.5], vec![0.5]));
    render(&mut engine, 4_800, 512);
    publish_listener_kernel(&mut publisher, voice, kernel(vec![0.5], vec![0.5]));
    let (after, _) = render(&mut engine, 2_048, 512);
    assert!(after.iter().all(|s| (s - 0.05).abs() < 1e-6));
}

#[test]
fn hero_voice_uses_its_own_kernel() {
    let (mut engine, control, mut publisher) = setup(environment(
        SpatialRenderer::StereoPan,
        ReverbRenderer::Convolution,
    ));
    let voice = control
        .play(clip(vec![0.2; 4800]), acoustic_options([0.0, 0.0, -1.0]))
        .unwrap();
    publisher
        .publish(|frame| {
            let mut v = engine_audio::acoustic::VoicePropagation::new(voice);
            v.direct = [0.0; 4];
            v.kernel = Some(kernel(vec![0.5], vec![-0.5]));
            frame.voices.push(v);
            Ok(())
        })
        .unwrap();
    let (left, right) = render(&mut engine, 4_800, 512);
    assert!((left[4_799] - 0.1).abs() < 1e-6 && (right[4_799] + 0.1).abs() < 1e-6);
    assert_eq!(control.acoustic_stats().unwrap().hero_voices, 1);
    // A kernel for another output rate is ignored and counted.
    publisher
        .publish(|frame| {
            let mut v = engine_audio::acoustic::VoicePropagation::new(voice);
            v.kernel = Some(Box::new(
                ConvolutionKernel::prepare(
                    &StereoImpulseResponse {
                        sample_rate: RATE,
                        left: vec![1.0],
                        right: vec![1.0],
                    },
                    44_100,
                )
                .unwrap(),
            ));
            frame.voices.push(v);
            Ok(())
        })
        .unwrap();
    render(&mut engine, 1024, 512);
    let stats = control.acoustic_stats().unwrap();
    assert_eq!((stats.hero_voices, stats.rejected_kernels), (0, 1));
}
