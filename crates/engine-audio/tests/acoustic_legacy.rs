//! Test 1, legacy bit-identity: with acoustics disabled the mixer output must
//! equal the pre-acoustics golden render exactly, even with the acoustic
//! plumbing attached and whichever spatial renderer is selected. The default
//! renderer is binaural (project decision), and it applies only to voices with
//! acoustics enabled.

mod common;

use engine_audio::acoustic::{MixerEnvironment, ReverbRenderer, SpatialRenderer, VoiceAcoustics};
use engine_audio::{
    Attenuation, AudioConfig, AudioEngine, Bus, Clip, Listener, PlayOptions, SpatialOptions,
};

/// Hash of the bit patterns of the golden render, captured from the mixer as it
/// was before `PlayOptions::acoustics` existed.
const GOLDEN_FNV1A: u64 = 0x7909_6455_f27a_abbe;
const GOLDEN_FRAMES: usize = 4800;

fn fnv1a(samples: &[f32]) -> u64 {
    samples.iter().fold(0xcbf2_9ce4_8422_2325, |hash, s| {
        s.to_bits().to_le_bytes().iter().fold(hash, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
        })
    })
}

fn saw(period: usize, len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| ((i % period) as f32 / period as f32) * 1.6 - 0.8)
        .collect()
}

fn render(acoustics: Option<VoiceAcoustics>, attach_plumbing: bool) -> Vec<f32> {
    render_with(acoustics, attach_plumbing.then(MixerEnvironment::default))
}

fn render_with(
    acoustics: Option<VoiceAcoustics>,
    environment: Option<MixerEnvironment>,
) -> Vec<f32> {
    let mut engine = AudioEngine::offline(AudioConfig::default()).unwrap();
    let control = engine.control();
    let _publisher = environment.map(|environment| {
        control.set_environment(environment).unwrap();
        let mut publisher = control.acoustic_publisher().unwrap();
        publisher.publish(|_| Ok(())).unwrap();
        publisher
    });
    let extra = |options: PlayOptions| PlayOptions {
        acoustics: acoustics.unwrap_or_default(),
        ..options
    };
    control
        .play(
            Clip::new(44_100, 1, saw(37, 3000)).unwrap(),
            extra(PlayOptions {
                looped: true,
                pitch: 1.3,
                gain: 0.7,
                bus: Bus::Music,
                ..Default::default()
            }),
        )
        .unwrap();
    let mut stereo = saw(23, 4000);
    stereo.extend(saw(41, 4000));
    control
        .play(
            Clip::new(48_000, 2, stereo.chunks(2).flatten().copied().collect()).unwrap(),
            extra(PlayOptions {
                looped: true,
                ..Default::default()
            }),
        )
        .unwrap();
    control
        .play(
            Clip::new(32_000, 1, saw(29, 2500)).unwrap(),
            extra(PlayOptions {
                looped: true,
                gain: 0.9,
                spatial: SpatialOptions {
                    enabled: true,
                    position: [4.0, 1.0, -3.0],
                    min_distance: 1.0,
                    max_distance: 50.0,
                    rolloff: 1.0,
                    attenuation: Attenuation::Inverse,
                },
                ..Default::default()
            }),
        )
        .unwrap();
    control.set_bus_gain(Bus::Music, 0.8).unwrap();
    control
        .set_listener(Listener {
            position: [0.5, 0.0, 0.5],
            forward: [1.0, 0.0, -1.0],
            ..Default::default()
        })
        .unwrap();
    let mut out = vec![0.0; GOLDEN_FRAMES * 2];
    for block in out.chunks_mut(512) {
        engine.mix(block).unwrap();
    }
    out
}

#[test]
fn default_play_options_leave_acoustics_disabled() {
    let options = PlayOptions::default();
    assert!(!options.acoustics.enabled);
    assert_eq!(options.acoustics, VoiceAcoustics::default());
    assert_eq!(
        MixerEnvironment::default().renderer,
        SpatialRenderer::Binaural
    );
}

#[test]
fn disabled_acoustics_match_the_pre_change_golden_render() {
    let out = render(None, false);
    assert!(out.iter().any(|s| *s != 0.0));
    assert_eq!(fnv1a(&out), GOLDEN_FNV1A, "{:#x}", fnv1a(&out));
}

#[test]
fn explicit_defaults_and_attached_publisher_do_not_change_output() {
    let golden = render(None, false);
    assert_eq!(render(Some(VoiceAcoustics::default()), false), golden);
    assert_eq!(render(None, true), golden);
}

#[test]
fn every_renderer_and_reverb_mode_keeps_disabled_voices_bit_identical() {
    let golden = render(None, false);
    for renderer in [SpatialRenderer::Binaural, SpatialRenderer::StereoPan] {
        for reverb in [
            ReverbRenderer::Off,
            ReverbRenderer::Parametric,
            ReverbRenderer::Convolution,
        ] {
            let environment = MixerEnvironment {
                renderer,
                reverb,
                ..Default::default()
            };
            assert_eq!(
                render_with(None, Some(environment)),
                golden,
                "{renderer:?} {reverb:?}"
            );
        }
    }
}

#[test]
fn invalid_acoustic_options_are_rejected_before_playing() {
    let engine = AudioEngine::offline(AudioConfig::default()).unwrap();
    let error = engine
        .play(
            Clip::new(48_000, 1, vec![0.1]).unwrap(),
            PlayOptions {
                acoustics: VoiceAcoustics {
                    doppler: 9.0,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.code, "audio.play.invalid");
    assert_eq!(engine.voice_count(), 0);
}

#[test]
fn second_publisher_is_refused_until_the_first_is_dropped() {
    let engine = AudioEngine::offline(AudioConfig::default()).unwrap();
    let control = engine.control();
    let first = control.acoustic_publisher().unwrap();
    assert_eq!(
        control.acoustic_publisher().unwrap_err().code,
        "audio.acoustic.publisher_taken"
    );
    drop(first);
    let _second = control.acoustic_publisher().unwrap();
    assert_eq!(
        control.acoustic_publisher().unwrap_err().code,
        "audio.acoustic.publisher_taken"
    );
}
