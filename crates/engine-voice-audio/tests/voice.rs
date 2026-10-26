use engine_audio::{AudioConfig, AudioEngine};
use engine_net_voice::{
    DownlinkPacket, JitterConfig, VoiceCodec, VoiceLimits, VoiceReceiver, VoiceRouter,
    VoiceRouting, VoiceSender,
};
use engine_voice_audio::{CaptureSession, FRAME_SAMPLES, OpusCodec, VoicePlayback};
use opusic_c::{Application, Channels, Encoder, SampleRate};

fn tone() -> [i16; FRAME_SAMPLES] {
    std::array::from_fn(|i| {
        ((i as f32 * 440. * std::f32::consts::TAU / 48_000.).sin() * 12_000.) as i16
    })
}

#[test]
fn real_opus_routes_to_ordinary_pcm_stream_and_offline_mixer() {
    let limits = VoiceLimits {
        max_frame_samples: FRAME_SAMPLES,
        ..VoiceLimits::default()
    };
    let mut sender = VoiceSender::new(
        Box::new(OpusCodec::encoder(1000).unwrap()),
        limits.clone(),
        0,
    )
    .unwrap();
    let mut router = VoiceRouter::new(limits.clone(), VoiceRouting::Global).unwrap();
    router.add_peer(1);
    router.add_peer(2);
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut playback = VoicePlayback::new(audio.control(), 1200, 2).unwrap();
    let samples = tone();
    let mut mixed = Vec::new();
    let mut max_source_frame: f64 = 0.;
    for seq in 0..12 {
        let now = f64::from(seq) * 20.;
        let up = sender.encode_frame(&samples, None, seq == 11).unwrap();
        let down = router.route(1, &up, now).unwrap();
        assert_eq!(down.len(), 1);
        let packet = DownlinkPacket::decode(&down[0].1, &limits).unwrap();
        assert_eq!(packet.timestamp, seq * FRAME_SAMPLES as u32);
        assert!(packet.payload.len() <= 1000);
        playback.receive_server_downlink(&down[0].1, now).unwrap();
        playback.tick(now).unwrap();
        let mut output = [0f32; FRAME_SAMPLES * 2];
        audio.mix(&mut output).unwrap();
        mixed.extend(output.chunks_exact(2).map(|stereo| stereo[0]));
        if let Some(snapshot) = playback.stream_snapshot(1).unwrap() {
            max_source_frame = max_source_frame.max(snapshot.source_frame);
            assert!(
                (snapshot.source_frame - snapshot.submitted_output_frames as f64).abs() < 0.001
            );
        }
    }
    for tick in 12..25 {
        playback.tick(f64::from(tick) * 20.).unwrap();
        audio.mix(&mut [0f32; FRAME_SAMPLES * 2]).unwrap();
    }
    assert!(max_source_frame >= FRAME_SAMPLES as f64);
    let expected: Vec<f32> = (0..mixed.len())
        .map(|i| f32::from(samples[i % FRAME_SAMPLES]) / 32768.)
        .collect();
    let best = (0..FRAME_SAMPLES)
        .map(|lag| {
            let pairs = mixed[lag..].iter().zip(&expected[..mixed.len() - lag]);
            let (dot, a2, b2) = pairs.fold((0f64, 0f64, 0f64), |(dot, a2, b2), (&a, &b)| {
                let (a, b) = (f64::from(a), f64::from(b));
                (dot + a * b, a2 + a * a, b2 + b * b)
            });
            dot / (a2 * b2).sqrt().max(1e-12)
        })
        .fold(0f64, f64::max);
    assert!(
        best > 0.2,
        "Opus and mixer output should correlate with the input tone: {best}"
    );
    assert_eq!(playback.active_streams(), 0);
}

#[test]
fn ended_speakers_release_decoder_capacity_and_mutes_are_bounded() {
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut playback = VoicePlayback::new(audio.control(), 1200, 1).unwrap();
    let mut encoder = OpusCodec::encoder(1000).unwrap();
    let mut payload = Vec::new();
    encoder.encode(&tone(), &mut payload).unwrap();
    let limits = VoiceLimits::default();
    for cycle in 0..4u64 {
        let now = cycle as f64 * 100.;
        let down = DownlinkPacket {
            speaker: cycle + 1,
            channel: 0,
            sequence: 0,
            timestamp: 0,
            position: None,
            end_of_talkspurt: true,
            payload: payload.clone(),
        }
        .encode(&limits)
        .unwrap();
        playback.receive_server_downlink(&down, now).unwrap();
        for step in 0..4 {
            playback.tick(now + f64::from(step) * 20.).unwrap();
            audio.mix(&mut [0.; FRAME_SAMPLES * 2]).unwrap();
        }
        assert_eq!(
            playback.active_streams(),
            0,
            "ended speaker retained its slot"
        );
    }
    playback.mute_speaker(100, true).unwrap();
    assert!(playback.mute_speaker(101, true).is_err());
    playback.disconnect();
    playback.mute_speaker(101, true).unwrap();
    playback.set_local_muted(true);
    assert_eq!(playback.active_streams(), 0);
}

#[test]
fn backpressure_drops_bounded_frames_then_resume_uses_fresh_stream() {
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut playback = VoicePlayback::new(audio.control(), 1200, 1).unwrap();
    let mut sender = VoiceSender::new(
        Box::new(OpusCodec::encoder(1000).unwrap()),
        VoiceLimits::default(),
        0,
    )
    .unwrap();
    let limits = VoiceLimits::default();
    for seq in 0..16 {
        let now = f64::from(seq) * 20.;
        let up = engine_net_voice::UplinkPacket::decode(
            &sender.encode_frame(&tone(), None, seq == 15).unwrap(),
            &limits,
        )
        .unwrap();
        let down = DownlinkPacket {
            speaker: 7,
            channel: 0,
            sequence: up.sequence,
            timestamp: up.timestamp,
            position: None,
            end_of_talkspurt: up.end_of_talkspurt,
            payload: up.payload,
        }
        .encode(&limits)
        .unwrap();
        playback.receive_server_downlink(&down, now).unwrap();
        playback.tick(now).unwrap();
    }
    assert!(playback.stats().frames_dropped > 0);
    for tick in 16..40 {
        audio.mix(&mut [0.; FRAME_SAMPLES * 2]).unwrap();
        playback.tick(f64::from(tick) * 20.).unwrap();
    }
    assert_eq!(playback.active_streams(), 0);
    let up = engine_net_voice::UplinkPacket::decode(
        &sender.encode_frame(&tone(), None, true).unwrap(),
        &limits,
    )
    .unwrap();
    let down = DownlinkPacket {
        speaker: 7,
        channel: 0,
        sequence: up.sequence,
        timestamp: up.timestamp,
        position: None,
        end_of_talkspurt: true,
        payload: up.payload,
    }
    .encode(&limits)
    .unwrap();
    playback.receive_server_downlink(&down, 800.).unwrap();
    playback.tick(800.).unwrap();
    playback.tick(820.).unwrap();
    assert_eq!(playback.active_streams(), 1);
    playback.mute_speaker(7, true).unwrap();
    assert_eq!(playback.active_streams(), 0);
}

#[test]
fn wrong_opus_duration_and_malformed_payload_do_not_decode() {
    let mut encoder = OpusCodec::encoder(1000).unwrap();
    let mut decoder = OpusCodec::decoder(1000).unwrap();
    let mut packet = Vec::new();
    encoder.encode(&tone(), &mut packet).unwrap();
    let mut pcm = Vec::new();
    assert!(decoder.decode(&packet, &mut pcm).is_ok());
    assert_eq!(pcm.len(), FRAME_SAMPLES);
    pcm.clear();
    let mut ten_ms = Encoder::new(Channels::Mono, SampleRate::Hz48000, Application::Voip).unwrap();
    let mut short_packet = [0u8; 1000];
    let short_len = ten_ms
        .encode_float_to_slice(&[0.0; 480], &mut short_packet)
        .unwrap();
    assert!(
        decoder
            .decode(&short_packet[..short_len], &mut pcm)
            .is_err()
    );
    assert!(decoder.decode(&[0xff; 1001], &mut pcm).is_err());
    assert!(decoder.decode(&[], &mut pcm).is_err());
    assert!(pcm.is_empty());
    assert!(decoder.conceal(&mut pcm));
    assert_eq!(pcm.len(), FRAME_SAMPLES);
}

#[test]
fn receiver_uses_opus_plc_for_missing_slot() {
    let limits = VoiceLimits {
        max_frame_samples: FRAME_SAMPLES,
        ..VoiceLimits::default()
    };
    let mut sender = VoiceSender::new(
        Box::new(OpusCodec::encoder(1000).unwrap()),
        limits.clone(),
        0,
    )
    .unwrap();
    let mut receiver = VoiceReceiver::new(
        limits.clone(),
        JitterConfig::default(),
        Box::new(|| Ok(Box::new(OpusCodec::decoder(1000)?))),
    )
    .unwrap();
    let mut router = VoiceRouter::new(limits, VoiceRouting::Global).unwrap();
    router.add_peer(1);
    router.add_peer(2);
    let mut concealed = 0;
    for seq in 0..8 {
        let now = f64::from(seq) * 20.;
        let up = sender.encode_frame(&tone(), None, false).unwrap();
        if seq != 3 {
            let down = router.route(1, &up, now).unwrap();
            receiver.receive(&down[0].1, now).unwrap();
        }
        concealed += receiver.pull(now).iter().filter(|f| f.concealed).count();
    }
    assert!(concealed >= 1);
}

#[test]
#[ignore = "requires explicit ENGINE_VOICE_MIC_OPT_IN=1 and an available microphone"]
fn optional_microphone_callback_progress_and_stop() {
    if std::env::var("ENGINE_VOICE_MIC_OPT_IN").as_deref() != Ok("1") {
        return;
    }
    let mut capture = CaptureSession::new(1200, 0).unwrap();
    assert!(!capture.is_active());
    capture.start().unwrap();
    for _ in 0..50 {
        if capture.stats().captured_input_frames > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(capture.stats().captured_input_frames > 0);
    let _packets = capture.pump().unwrap();
    let _end_packet = capture.stop().unwrap();
    assert!(!capture.is_active());
    let after_stop = capture.stats().captured_input_frames;
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(capture.stats().captured_input_frames, after_stop);
    assert!(capture.pump().unwrap().is_empty());
}
