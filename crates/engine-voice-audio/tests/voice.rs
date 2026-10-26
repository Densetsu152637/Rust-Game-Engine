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
    let mut mixed_nonzero = false;
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
        mixed_nonzero |= output.iter().any(|v| v.abs() > 0.001);
    }
    for tick in 12..25 {
        playback.tick(f64::from(tick) * 20.).unwrap();
        audio.mix(&mut [0f32; FRAME_SAMPLES * 2]).unwrap();
    }
    assert!(mixed_nonzero);
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
    std::thread::sleep(std::time::Duration::from_millis(100));
    let _packets = capture.pump().unwrap();
    let _end_packet = capture.stop().unwrap();
    assert!(!capture.is_active());
}
