use engine_audio::*;
use std::sync::Arc;

fn pcm() -> Vec<u8> {
    vec![
        b'R', b'I', b'F', b'F', 40, 0, 0, 0, b'W', b'A', b'V', b'E', b'f', b'm', b't', b' ', 16, 0,
        0, 0, 1, 0, 1, 0, 0x80, 0xbb, 0, 0, 0, 0x77, 1, 0, 2, 0, 16, 0, b'd', b'a', b't', b'a', 4,
        0, 0, 0, 0, 0x40, 0, 0xc0,
    ]
}
fn clip() -> Arc<Clip> {
    decode_wav(&pcm()).unwrap()
}
fn looping() -> PlayOptions {
    PlayOptions {
        looped: true,
        ..Default::default()
    }
}
fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 0.0001, "{a} != {b}");
}
fn frame(engine: &mut AudioEngine) -> [f32; 2] {
    let mut f = [99.0; 2];
    engine.mix(&mut f).unwrap();
    f
}

#[test]
fn strict_wav_decode_bounds_formats_chunks_and_pcm() {
    let wav = pcm();
    let c = decode_wav(&wav).unwrap();
    assert_eq!((c.sample_rate(), c.channels(), c.frames()), (48_000, 1, 2));
    assert_eq!(c.samples(), &[0.5, -0.5]);
    // Every truncated prefix and inconsistent RIFF size rejects without panic.
    for end in 0..wav.len() {
        assert!(decode_wav(&wav[..end]).is_err());
    }
    let mut bad = wav.clone();
    bad[20] = 3;
    assert_eq!(decode_wav(&bad).unwrap_err().code, "audio.wav.unsupported");
    for offset in [22, 24, 28, 32, 34] {
        let mut bad = wav.clone();
        bad[offset] ^= 1;
        assert!(decode_wav(&bad).is_err(), "invalid metadata at {offset}");
    }
    let mut max = wav.clone();
    max[44] = 0xff;
    max[45] = 0x7f;
    near(decode_wav(&max).unwrap().samples()[0], 32767.0 / 32768.0);
    let mut duplicate = wav.clone();
    duplicate.extend_from_slice(&wav[36..]);
    let size = (duplicate.len() - 8) as u32;
    duplicate[4..8].copy_from_slice(&size.to_le_bytes());
    assert_eq!(
        decode_wav(&duplicate).unwrap_err().code,
        "audio.wav.invalid"
    );
    let mut junk = wav.clone();
    junk.extend_from_slice(b"JUNK\x01\0\0\0x\0");
    let size = (junk.len() - 8) as u32;
    junk[4..8].copy_from_slice(&size.to_le_bytes());
    assert_eq!(decode_wav(&junk).unwrap().samples(), &[0.5, -0.5]);
    let mut reordered = wav[..12].to_vec();
    reordered.extend_from_slice(&wav[36..]);
    reordered.extend_from_slice(&wav[12..36]);
    assert_eq!(decode_wav(&reordered).unwrap().samples(), &[0.5, -0.5]);
    assert!(load_wav("missing-service-audio.wav").is_err());
    assert!(Clip::new(0, 1, vec![0.0]).is_err());
    assert!(Clip::new(48_000, 2, vec![0.0]).is_err());
    assert!(Clip::new(48_000, 1, vec![f32::NAN]).is_err());
    assert!(Clip::new(48_000, 1, vec![1.01]).is_err());
    assert!(Clip::new(48_000, 1, vec![]).is_err());
}

#[test]
fn voice_lifecycle_limits_loop_bus_gains_and_shutdown() {
    let mut engine = AudioEngine::offline(AudioConfig {
        max_voices: 2,
        ..Default::default()
    })
    .unwrap();
    let one = engine.play(clip(), PlayOptions::default()).unwrap();
    let mut output = [0.0; 6];
    engine.mix(&mut output).unwrap();
    assert_eq!(output, [0.5, 0.5, -0.5, -0.5, 0.0, 0.0]);
    assert_eq!(engine.voice_state(one), VoiceState::Stopped);
    assert_eq!(engine.voice_count(), 0);
    assert!(!engine.stop(one));
    let options = PlayOptions {
        bus: Bus::Music,
        gain: 0.5,
        owner: "room:a".into(),
        ..looping()
    };
    let voice = engine.play(clip(), options).unwrap();
    assert!(engine.pause(voice));
    assert_eq!(frame(&mut engine), [0.0; 2]);
    assert_eq!(engine.voice(voice).unwrap().cursor, 0.0);
    assert!(engine.resume(voice));
    engine.set_bus_gain(Bus::Music, 0.5).unwrap();
    engine.set_bus_gain(Bus::Master, 0.5).unwrap();
    engine.mix(&mut output).unwrap();
    assert_eq!(output, [0.0625, 0.0625, -0.0625, -0.0625, 0.0625, 0.0625]);
    engine.set_bus_gain(Bus::Master, 0.0).unwrap();
    assert_eq!(frame(&mut engine), [0.0; 2]);
    engine.set_bus_gain(Bus::Master, 99.0).unwrap();
    engine.set_bus_gain(Bus::Music, 1.0).unwrap();
    assert!(engine.set_gain(voice, f32::NAN));
    assert_eq!(frame(&mut engine), [0.0; 2]);
    engine.set_gain(voice, 1.0);
    let other = engine
        .play(
            clip(),
            PlayOptions {
                owner: "room:b".into(),
                ..looping()
            },
        )
        .unwrap();
    assert_eq!(
        engine.play(clip(), looping()).unwrap_err().code,
        "audio.voice.limit"
    );
    engine.stop_owner("room:a");
    assert_eq!(engine.voice_count(), 1);
    assert!(!engine.pause(voice));
    assert_eq!(engine.voice_state(other), VoiceState::Playing);
    engine.stop_all();
    assert_eq!(engine.voice_count(), 0);
    let control = engine.control();
    engine.shutdown();
    assert_eq!(
        control.play(clip(), looping()).unwrap_err().code,
        "audio.not_initialized"
    );
    assert!(engine.mix(&mut output).is_err());
    assert!(!engine.playback_enabled());
    assert!(
        AudioEngine::offline(AudioConfig {
            sample_rate: 0,
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        AudioEngine::offline(AudioConfig {
            max_voices: 0,
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        AudioEngine::offline(AudioConfig {
            max_voices: 1025,
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn resampling_pitch_clipping_and_atomic_reconfiguration() {
    let mut engine = AudioEngine::offline(AudioConfig::default()).unwrap();
    let c = Clip::new(24_000, 1, vec![0.0, 1.0]).unwrap();
    engine.play(c, PlayOptions::default()).unwrap();
    let mut output = [0.0; 10];
    engine.mix(&mut output).unwrap();
    assert_eq!(output, [0.0, 0.0, 0.5, 0.5, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0]);
    engine
        .play(
            Clip::new(48_000, 1, vec![0.0, 0.25, 0.5, 0.75]).unwrap(),
            PlayOptions {
                pitch: 2.0,
                ..Default::default()
            },
        )
        .unwrap();
    let mut pitched = [0.0; 6];
    engine.mix(&mut pitched).unwrap();
    assert_eq!(pitched, [0.0, 0.0, 0.5, 0.5, 0.0, 0.0]);
    let c = Clip::new(48_000, 1, vec![1.0]).unwrap();
    engine.play(c.clone(), looping()).unwrap();
    engine.play(c, looping()).unwrap();
    assert_eq!(frame(&mut engine), [1.0; 2]);
    engine.stop_all();
    let voice = engine.play(clip(), looping()).unwrap();
    frame(&mut engine);
    engine.pause(voice);
    let before = engine.voice(voice).unwrap();
    let invalid = PlayOptions {
        pitch: 0.0,
        ..looping()
    };
    assert!(engine.configure(voice, invalid).is_err());
    assert_eq!(engine.voice(voice).unwrap(), before);
    engine
        .configure(
            voice,
            PlayOptions {
                gain: 0.5,
                pitch: 0.5,
                ..looping()
            },
        )
        .unwrap();
    let after = engine.voice(voice).unwrap();
    assert_eq!(after.cursor, before.cursor);
    assert_eq!(after.state, VoiceState::Paused);
    engine.resume(voice);
    assert_eq!(frame(&mut engine), [-0.25; 2]);
    let before = engine.voice(voice).unwrap().cursor;
    let mut odd = [9.0; 3];
    assert!(engine.mix(&mut odd).is_err());
    assert_eq!(odd, [9.0; 3]);
    assert_eq!(engine.voice(voice).unwrap().cursor, before);
}

#[test]
fn spatial_attenuation_orientation_stereo_downmix_and_validation() {
    let mut engine = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut options = looping();
    options.spatial.enabled = true;
    options.spatial.position = [1.0, 0.0, 0.0];
    let voice = engine
        .play(Clip::new(48_000, 1, vec![1.0; 4]).unwrap(), options.clone())
        .unwrap();
    assert_eq!(frame(&mut engine), [0.0, 1.0]);
    options.spatial.position = [-2.0, 0.0, 0.0];
    engine.configure(voice, options.clone()).unwrap();
    assert_eq!(frame(&mut engine), [0.5, 0.0]);
    let mut listener = Listener {
        forward: [0.0, 0.0, 1.0],
        ..Default::default()
    };
    engine.set_listener(listener).unwrap();
    assert_eq!(frame(&mut engine), [0.0, 0.5]);
    listener.position = [-2.0, 0.0, 0.0];
    engine.set_listener(listener).unwrap();
    let centered = frame(&mut engine);
    near(centered[0], 0.5_f32.sqrt());
    near(centered[1], centered[0]);
    options.spatial.attenuation = Attenuation::Linear;
    options.spatial.max_distance = 3.0;
    options.spatial.position = [0.0; 3];
    engine.configure(voice, options.clone()).unwrap();
    near(frame(&mut engine)[0], 0.5);
    options.spatial.position = [1.0, 0.0, 0.0];
    engine.configure(voice, options.clone()).unwrap();
    assert_eq!(frame(&mut engine), [0.0; 2]);
    options.spatial.attenuation = Attenuation::None;
    engine.configure(voice, options.clone()).unwrap();
    near(frame(&mut engine)[0], 1.0);
    listener.up = listener.forward;
    assert!(engine.set_listener(listener).is_err());
    listener = Listener::default();
    listener.position[0] = f32::INFINITY;
    assert!(engine.set_listener(listener).is_err());
    for value in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        options.spatial.min_distance = value;
        assert!(options.validate().is_err());
    }
    engine.stop_all();
    engine.reset_listener().unwrap();
    let mut options = PlayOptions::default();
    options.spatial.enabled = true;
    options.spatial.position = [1.0, 0.0, 0.0];
    engine
        .play(Clip::new(48_000, 2, vec![1.0, 0.0]).unwrap(), options)
        .unwrap();
    assert_eq!(frame(&mut engine), [0.0, 0.5]);
    engine
        .play(
            Clip::new(48_000, 2, vec![1.0, 0.25]).unwrap(),
            PlayOptions::default(),
        )
        .unwrap();
    assert_eq!(frame(&mut engine), [1.0, 0.25]);
}

#[test]
fn sources_observers_order_restarts_configuration_and_owner_cleanup() {
    let mut engine = AudioEngine::offline(AudioConfig {
        max_voices: 1,
        ..Default::default()
    })
    .unwrap();
    let stale;
    {
        let mut sources = AudioSources::new(engine.control(), "room").unwrap();
        let source = sources.create(clip(), looping()).unwrap();
        stale = source;
        assert_eq!(engine.voice_count(), 0);
        let observer = sources.observe("door", source, SourceAction::Play).unwrap();
        assert_eq!(sources.notify("door").unwrap(), 1);
        assert_eq!(sources.state(source), VoiceState::Playing);
        assert_eq!(sources.notify("door").unwrap(), 1);
        assert_eq!(engine.voice_count(), 1);
        sources.control(source, SourceAction::Pause).unwrap();
        assert_eq!(frame(&mut engine), [0.0; 2]);
        sources.control(source, SourceAction::Resume).unwrap();
        assert_eq!(frame(&mut engine), [0.5; 2]);
        let options = PlayOptions {
            gain: 0.5,
            owner: "attempt-to-reassign".into(),
            ..looping()
        };
        sources.configure(source, options).unwrap();
        assert_eq!(frame(&mut engine), [-0.25; 2]);
        assert_eq!(
            engine
                .voice(sources.voice(source).unwrap())
                .unwrap()
                .options
                .owner,
            "room"
        );
        let before = sources.options(source).unwrap().clone();
        assert!(
            sources
                .configure(
                    source,
                    PlayOptions {
                        pitch: 0.0,
                        ..looping()
                    }
                )
                .is_err()
        );
        assert_eq!(sources.options(source).unwrap(), &before);
        assert!(sources.unobserve(observer));
        assert_eq!(sources.notify("door").unwrap(), 0);
        let remove = sources.observe("door", source, SourceAction::Stop).unwrap();
        assert!(sources.remove(source));
        assert_eq!(engine.voice_count(), 0);
        assert!(!sources.unobserve(remove));
        assert!(sources.control(source, SourceAction::Play).is_err());
        let one = sources.create(clip(), PlayOptions::default()).unwrap();
        assert_ne!(one, stale);
        sources.control(one, SourceAction::Play).unwrap();
        let mut done = [0.0; 6];
        engine.mix(&mut done).unwrap();
        assert_eq!(sources.state(one), VoiceState::Stopped);
        sources.control(one, SourceAction::Play).unwrap();
    }
    assert_eq!(engine.voice_count(), 0);
    let mut next = AudioSources::new(engine.control(), "next").unwrap();
    assert!(!next.contains(stale));
    let source = next.create(clip(), looping()).unwrap();
    next.observe("sequence", source, SourceAction::Play)
        .unwrap();
    next.observe("sequence", source, SourceAction::Pause)
        .unwrap();
    assert_eq!(next.notify("sequence").unwrap(), 2);
    assert_eq!(next.state(source), VoiceState::Paused);
    for event in ["".to_owned(), "a\0b".to_owned(), "x".repeat(513)] {
        assert!(
            next.observe(event.clone(), source, SourceAction::Play)
                .is_err()
        );
        assert!(next.notify(&event).is_err());
    }
    let long = "x".repeat(512);
    next.observe(long.clone(), source, SourceAction::Stop)
        .unwrap();
    assert_eq!(next.notify(&long).unwrap(), 1);
    assert_eq!(next.notify("unknown").unwrap(), 0);
}

#[test]
fn notifications_preserve_earlier_effects_on_failure_and_drop_is_scoped() {
    let engine = AudioEngine::offline(AudioConfig {
        max_voices: 1,
        ..Default::default()
    })
    .unwrap();
    let mut sources = AudioSources::new(engine.control(), "same-owner").unwrap();
    let a = sources.create(clip(), looping()).unwrap();
    let b = sources.create(clip(), looping()).unwrap();
    sources.observe("event", a, SourceAction::Play).unwrap();
    sources.observe("event", b, SourceAction::Play).unwrap();
    assert_eq!(
        sources.notify("event").unwrap_err().code,
        "audio.voice.limit"
    );
    assert_eq!(sources.state(a), VoiceState::Playing);
    assert_eq!(sources.state(b), VoiceState::Stopped);
    sources.clear();
    let direct = engine
        .play(
            clip(),
            PlayOptions {
                owner: "same-owner".into(),
                ..looping()
            },
        )
        .unwrap();
    drop(sources);
    assert_eq!(engine.voice_state(direct), VoiceState::Playing);
    let mut sources = AudioSources::new(engine.control(), "new").unwrap();
    let c = sources.create(clip(), looping()).unwrap();
    drop(engine);
    assert!(sources.control(c, SourceAction::Play).is_err());
    assert_eq!(sources.state(c), VoiceState::Stopped);
}

#[test]
fn controls_are_thread_safe_and_engine_drop_invalidates_clones() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<AudioControl>();
    let engine = AudioEngine::offline(AudioConfig::default()).unwrap();
    let control = engine.control();
    let mut workers = vec![];
    for n in 0..8 {
        let control = control.clone();
        workers.push(std::thread::spawn(move || {
            let owner = format!("worker-{n}");
            for _ in 0..100 {
                let id = control
                    .play(
                        clip(),
                        PlayOptions {
                            owner: owner.clone(),
                            ..looping()
                        },
                    )
                    .unwrap();
                assert!(control.pause(id));
                assert!(control.resume(id));
                assert!(control.stop(id));
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(control.voice_count(), 0);
    drop(engine);
    assert_eq!(
        control.play(clip(), looping()).unwrap_err().code,
        "audio.not_initialized"
    );
}

#[test]
fn source_observer_bounds_and_configuration_boundaries() {
    let engine = AudioEngine::offline(AudioConfig::default()).unwrap();
    assert!(AudioSources::new(engine.control(), "x".repeat(257)).is_err());
    let mut sources = AudioSources::new(engine.control(), "x".repeat(256)).unwrap();
    let clip = clip();
    let mut first = None;
    for _ in 0..1024 {
        let source = sources
            .create(clip.clone(), PlayOptions::default())
            .unwrap();
        first.get_or_insert(source);
    }
    let first = first.unwrap();
    assert_eq!(
        sources
            .create(clip.clone(), PlayOptions::default())
            .unwrap_err()
            .code,
        "audio.source.limit"
    );
    let mut observer = None;
    for _ in 0..1024 {
        observer.get_or_insert(
            sources
                .observe("bounded", first, SourceAction::Stop)
                .unwrap(),
        );
    }
    assert_eq!(
        sources
            .observe("bounded", first, SourceAction::Stop)
            .unwrap_err()
            .code,
        "audio.observer.limit"
    );
    sources.unobserve(observer.unwrap());
    sources
        .observe("bounded", first, SourceAction::Stop)
        .unwrap();
    assert_eq!(sources.notify("bounded").unwrap(), 1024);
    sources.clear();
    assert!(!sources.contains(first));
    assert_eq!(sources.notify("bounded").unwrap(), 0);
    for pitch in [0.0, 0.124, 8.001, f32::NAN, f32::INFINITY] {
        assert!(
            engine
                .play(
                    clip.clone(),
                    PlayOptions {
                        pitch,
                        ..Default::default()
                    }
                )
                .is_err()
        );
    }
    for pitch in [0.125, 8.0] {
        let voice = engine
            .play(
                clip.clone(),
                PlayOptions {
                    pitch,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(engine.stop(voice));
    }
    assert!(
        engine
            .play(
                clip.clone(),
                PlayOptions {
                    bus: Bus::Master,
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert!(
        engine
            .play(
                clip.clone(),
                PlayOptions {
                    owner: "x".repeat(257),
                    ..Default::default()
                }
            )
            .is_err()
    );
    for bad in [
        Listener {
            forward: [0.0; 3],
            ..Default::default()
        },
        Listener {
            up: [0.0; 3],
            ..Default::default()
        },
    ] {
        assert!(engine.set_listener(bad).is_err());
    }
    engine
        .set_listener(Listener {
            forward: [0.0, 0.0, -10.0],
            up: [0.0, 20.0, 0.0],
            ..Default::default()
        })
        .unwrap();
}

#[test]
fn looping_interpolates_across_seam_and_bus_mutes_retain_cursor() {
    let mut engine = AudioEngine::offline(AudioConfig::default()).unwrap();
    let voice = engine
        .play(
            clip(),
            PlayOptions {
                pitch: 0.5,
                ..looping()
            },
        )
        .unwrap();
    let mut output = [0.0; 10];
    engine.mix(&mut output).unwrap();
    assert_eq!(output, [0.5, 0.5, 0.0, 0.0, -0.5, -0.5, 0.0, 0.0, 0.5, 0.5]);
    engine.set_bus_gain(Bus::Effects, f32::INFINITY).unwrap();
    let before = engine.voice(voice).unwrap().cursor;
    assert_eq!(frame(&mut engine), [0.0; 2]);
    assert!(engine.voice(voice).unwrap().cursor > before);
}

/// Explicit opt-in real device check. A missing device fails this test; it does
/// not silently pass by treating offline mixing as playback.
#[test]
#[ignore = "requires a native output device; run explicitly with --ignored --nocapture"]
fn native_device_opens_submits_pcm_and_closes() {
    let mut engine = AudioEngine::device(AudioConfig::default()).expect("native device open/start");
    let tone = (0..4800)
        .map(|n| 0.02 * (n as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin())
        .collect();
    let voice = engine
        .play(Clip::new(48_000, 1, tone).unwrap(), looping())
        .unwrap();
    let before = engine.device_frames();
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(engine.playback_enabled());
    assert_eq!(engine.device_error(), None);
    assert!(
        engine.device_frames() > before,
        "native output callback ran"
    );
    assert!(
        engine.voice(voice).unwrap().cursor > 0.0,
        "callback consumed playing PCM"
    );
    assert!(engine.mix(&mut [0.0; 2]).is_err());
    assert!(engine.stop(voice));
    println!(
        "DEVICE PASS: {} stereo frames submitted through CPAL; audibility unverified",
        engine.device_frames()
    );
    engine.shutdown();
    assert!(!engine.playback_enabled());
    assert_eq!(engine.device_error(), None);
    assert_eq!(engine.voice_count(), 0);
}
