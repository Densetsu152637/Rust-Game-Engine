use engine_net_voice::*;
use engine_network::PeerId;

fn codec() -> Box<dyn VoiceCodec> {
    Box::new(RawPcmCodec::new(16_000, 20).unwrap())
}
fn tone(seq: u32, samples: usize) -> Vec<i16> {
    (0..samples)
        .map(|i| (((seq as usize * samples + i) % 64) as i16 - 32) * 256)
        .collect()
}
/// Deterministic splitmix64.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn router(routing: VoiceRouting, peers: &[PeerId]) -> VoiceRouter {
    let mut r = VoiceRouter::new(VoiceLimits::default(), routing).unwrap();
    for &p in peers {
        r.add_peer(p);
    }
    r
}

#[test]
fn raw_pcm_round_trip_through_router_and_receiver() {
    let limits = VoiceLimits::default();
    let mut sender = VoiceSender::new(codec(), limits.clone(), 0).unwrap();
    let mut router = router(VoiceRouting::Global, &[1, 2]);
    let mut receiver =
        VoiceReceiver::new(limits, JitterConfig::default(), Box::new(codec)).unwrap();
    let samples = sender.frame_samples();
    assert_eq!(samples, 320);
    let mut played = Vec::new();
    for seq in 0..20u32 {
        let now = f64::from(seq) * 20.0;
        let up = sender
            .encode_frame(&tone(seq, samples), None, seq == 19)
            .unwrap();
        let out = router.route(1, &up, now).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 2);
        receiver.receive(&out[0].1, now).unwrap();
        played.extend(receiver.pull(now));
    }
    for step in 20..40 {
        played.extend(receiver.pull(f64::from(step) * 20.0));
    }
    assert_eq!(played.len(), 20);
    for (seq, frame) in played.iter().enumerate() {
        assert_eq!(frame.speaker, 1);
        assert!(!frame.concealed);
        assert_eq!(frame.pcm, tone(seq as u32, samples));
    }
    assert!(!receiver.pull(2000.0).iter().any(|f| f.speaker == 1));
}

#[test]
fn jitter_buffer_reorders_conceals_losses_and_adapts_delay() {
    let limits = VoiceLimits::default();
    let mut sender = VoiceSender::new(codec(), limits.clone(), 0).unwrap();
    let mut router = router(VoiceRouting::Global, &[1, 2]);
    let mut receiver =
        VoiceReceiver::new(limits, JitterConfig::default(), Box::new(codec)).unwrap();
    let samples = sender.frame_samples();
    let mut rng = Rng(7);
    let frames = 500u32;
    let mut arrivals = Vec::new();
    let mut lost = 0;
    for seq in 0..frames {
        let bytes = sender
            .encode_frame(&tone(seq, samples), None, false)
            .unwrap();
        if rng.unit() < 0.1 {
            lost += 1;
            continue;
        }
        // 0..60 ms of jitter reorders neighbouring frames.
        let arrival = f64::from(seq) * 20.0 + rng.unit() * 60.0;
        arrivals.push((arrival, bytes));
    }
    arrivals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut next = 0;
    let mut outputs = Vec::new();
    for tick in 0..(frames + 40) {
        let now = f64::from(tick) * 20.0;
        while next < arrivals.len() && arrivals[next].0 <= now {
            for (_, down) in router
                .route(1, &arrivals[next].1, arrivals[next].0)
                .unwrap()
            {
                receiver.receive(&down, arrivals[next].0).unwrap();
            }
            next += 1;
        }
        outputs.extend(receiver.pull(now));
    }
    let stats = receiver.stats(1).unwrap();
    println!("lost={lost} stats={stats:?}");
    let decoded = outputs.iter().filter(|f| !f.concealed).count() as u64;
    let concealed = outputs.iter().filter(|f| f.concealed).count() as u64;
    assert_eq!(stats.concealed + stats.stretched, concealed);
    assert_eq!(decoded, stats.played);
    // Adaptive delay keeps late frames rare: nearly every received frame is played.
    assert!(
        stats.late * 50 <= stats.received,
        "late frames {}",
        stats.late
    );
    assert!(decoded + stats.late + stats.discarded >= stats.received - 1);
    // Concealment covers losses and late frames, plus at most the trailing
    // max_missing_before_stop (10) slots after the stream ends without an end flag.
    assert!(
        stats.concealed + 2 >= lost && stats.concealed <= lost + stats.late + stats.discarded + 10
    );
    assert!(stats.jitter_ms > 5.0 && stats.target_delay_ms > 30.0);
}

#[test]
fn fade_concealment_attenuates_then_silences() {
    let mut fade = FadeConcealment::default();
    let last = vec![1024i16; 4];
    let mut out = Vec::new();
    fade.conceal(&last, 1, 4, &mut out);
    assert_eq!(out, vec![512; 4]);
    out.clear();
    fade.conceal(&last, 4, 4, &mut out);
    assert_eq!(out, vec![0; 4]);
}

struct Native;
impl VoiceCodec for Native {
    fn sample_rate(&self) -> u32 {
        8000
    }
    fn frame_samples(&self) -> usize {
        160
    }
    fn encode(&mut self, _pcm: &[i16], out: &mut Vec<u8>) -> engine_net_voice::Result<()> {
        out.push(1);
        Ok(())
    }
    fn decode(&mut self, _payload: &[u8], out: &mut Vec<i16>) -> engine_net_voice::Result<()> {
        out.extend(std::iter::repeat_n(100, 160));
        Ok(())
    }
    fn conceal(&mut self, out: &mut Vec<i16>) -> bool {
        out.extend(std::iter::repeat_n(7, 160));
        true
    }
}

#[test]
fn codec_native_concealment_hook_is_preferred() {
    let limits = VoiceLimits::default();
    let mut sender = VoiceSender::new(Box::new(Native), limits.clone(), 0).unwrap();
    let mut receiver = VoiceReceiver::new(
        limits,
        JitterConfig::default(),
        Box::new(|| Box::new(Native) as Box<dyn VoiceCodec>),
    )
    .unwrap();
    let mut router = router(VoiceRouting::Global, &[1, 2]);
    let mut outputs = Vec::new();
    for seq in 0..6u32 {
        let now = f64::from(seq) * 20.0;
        let bytes = sender.encode_frame(&[0; 160], None, false).unwrap();
        if seq != 3 {
            for (_, down) in router.route(1, &bytes, now).unwrap() {
                receiver.receive(&down, now).unwrap();
            }
        }
        outputs.extend(receiver.pull(now));
    }
    let concealed: Vec<_> = outputs.iter().filter(|f| f.concealed).collect();
    assert_eq!(concealed.len(), 1);
    assert!(concealed[0].pcm.iter().all(|&s| s == 7));
}

#[test]
fn proximity_routing_uses_server_positions_not_client_claims() {
    let limits = VoiceLimits::default();
    let mut sender = VoiceSender::new(codec(), limits.clone(), 0).unwrap();
    let mut r = router(
        VoiceRouting::Proximity { max_distance: 10.0 },
        &[1, 2, 3, 4],
    );
    r.set_position(1, Some([0.0, 0.0, 0.0])).unwrap();
    r.set_position(2, Some([5.0, 0.0, 0.0])).unwrap();
    r.set_position(3, Some([50.0, 0.0, 0.0])).unwrap();
    // Peer 4 has no known position and is never in range.
    let samples = sender.frame_samples();
    let bytes = sender
        .encode_frame(&tone(0, samples), Some([50.0, 0.0, 0.0]), false)
        .unwrap();
    let out = r.route(1, &bytes, 0.0).unwrap();
    let listeners: Vec<_> = out.iter().map(|(p, _)| *p).collect();
    assert_eq!(listeners, vec![2]);
    let down = DownlinkPacket::decode(&out[0].1, &limits).unwrap();
    assert_eq!(down.speaker, 1);
    assert_eq!(down.position, Some([0.0, 0.0, 0.0]), "server position wins");

    // Without a server position the claim is dropped unless explicitly trusted.
    r.set_position(1, None).unwrap();
    assert!(r.route(1, &bytes, 0.0).unwrap().is_empty());
    r.set_trust_client_positions(true);
    let out = r.route(1, &bytes, 0.0).unwrap();
    assert_eq!(out.iter().map(|(p, _)| *p).collect::<Vec<_>>(), vec![3]);
}

#[test]
fn channels_mutes_moderation_and_rate_limits() {
    let limits = VoiceLimits::default();
    let mut sender = VoiceSender::new(codec(), limits.clone(), 2).unwrap();
    let mut r = router(VoiceRouting::Channel, &[1, 2, 3]);
    let samples = sender.frame_samples();
    for p in [1, 2, 3] {
        r.join_channel(p, 2).unwrap();
    }
    r.set_muted(3, 1, true).unwrap();
    let bytes = sender.encode_frame(&tone(0, samples), None, false).unwrap();
    let out = r.route(1, &bytes, 0.0).unwrap();
    assert_eq!(out.iter().map(|(p, _)| *p).collect::<Vec<_>>(), vec![2]);
    r.leave_channel(1, 2).unwrap();
    assert!(r.route(1, &bytes, 1.0).unwrap().is_empty());
    r.join_channel(1, 2).unwrap();

    r.set_speaking_allowed(1, false).unwrap();
    assert_eq!(r.route(1, &bytes, 2.0), Err(VoiceError::RateLimited));
    r.set_speaking_allowed(1, true).unwrap();

    let mut accepted = 0;
    for i in 0..100 {
        if r.route(1, &bytes, 2000.0 + f64::from(i)).is_ok() {
            accepted += 1;
        }
    }
    assert_eq!(accepted, limits.max_frames_per_second);
    assert!(r.route(1, &bytes, 3100.0).is_ok(), "window resets");
    assert!(r.route(99, &bytes, 0.0).is_err(), "unknown speaker");
}

#[test]
fn malformed_packets_are_rejected_without_panics() {
    let limits = VoiceLimits::default();
    let mut sender = VoiceSender::new(codec(), limits.clone(), 0).unwrap();
    let good = sender
        .encode_frame(&tone(0, 320), Some([1.0, 2.0, 3.0]), false)
        .unwrap();
    assert!(UplinkPacket::decode(&good, &limits).is_ok());
    assert!(DownlinkPacket::decode(&good, &limits).is_err(), "direction");
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(UplinkPacket::decode(&trailing, &limits).is_err());
    assert!(UplinkPacket::decode(&good[..good.len() - 1], &limits).is_err());
    let mut nan = good.clone();
    nan[12..16].copy_from_slice(&f32::NAN.to_le_bytes());
    assert!(UplinkPacket::decode(&nan, &limits).is_err());
    let mut flags = good.clone();
    flags[2] |= 0x80;
    assert!(UplinkPacket::decode(&flags, &limits).is_err());
    let small = VoiceLimits {
        max_payload_bytes: 100,
        ..VoiceLimits::default()
    };
    assert!(UplinkPacket::decode(&good, &small).is_err(), "payload cap");

    let mut receiver =
        VoiceReceiver::new(limits.clone(), JitterConfig::default(), Box::new(codec)).unwrap();
    let mut r = router(VoiceRouting::Global, &[1, 2]);
    let mut rng = Rng(42);
    for i in 0..20_000 {
        let mut bytes = good.clone();
        let mutations = 1 + rng.next() % 4;
        for _ in 0..mutations {
            let at = (rng.next() as usize) % bytes.len();
            bytes[at] = rng.next() as u8;
        }
        if rng.next().is_multiple_of(5) {
            bytes.truncate((rng.next() as usize) % bytes.len());
        }
        let _ = UplinkPacket::decode(&bytes, &limits);
        let _ = DownlinkPacket::decode(&bytes, &limits);
        let _ = r.route(1, &bytes, f64::from(i) * 20.0);
        let _ = receiver.receive(&bytes, f64::from(i));
        let _ = receiver.pull(f64::from(i));
    }
    assert!(r.stats().malformed > 0);
}

#[test]
fn receiver_bounds_speakers_and_buffers() {
    let limits = VoiceLimits {
        max_speakers: 2,
        max_buffered_frames: 4,
        ..VoiceLimits::default()
    };
    let mut receiver =
        VoiceReceiver::new(limits.clone(), JitterConfig::default(), Box::new(codec)).unwrap();
    let payload = vec![0u8; 640];
    let packet = |speaker, sequence| DownlinkPacket {
        speaker,
        channel: 0,
        sequence,
        timestamp: sequence * 320,
        position: None,
        end_of_talkspurt: false,
        payload: payload.clone(),
    };
    receiver
        .receive(&packet(1, 0).encode(&limits).unwrap(), 0.0)
        .unwrap();
    receiver
        .receive(&packet(2, 0).encode(&limits).unwrap(), 0.0)
        .unwrap();
    assert!(
        receiver
            .receive(&packet(3, 0).encode(&limits).unwrap(), 0.0)
            .is_err()
    );
    for seq in 1..100 {
        receiver
            .receive(&packet(1, seq).encode(&limits).unwrap(), 0.0)
            .unwrap();
    }
    assert!(receiver.stats(1).unwrap().buffered_frames <= 4);
    // A restarted sender (sequence jumps back far) resynchronises instead of being "late" forever.
    for seq in 0..3 {
        receiver
            .receive(&packet(2, 1_000_000 + seq).encode(&limits).unwrap(), 0.0)
            .unwrap();
    }
    for seq in 0..3 {
        receiver
            .receive(&packet(2, seq).encode(&limits).unwrap(), 1.0)
            .unwrap();
    }
    assert_eq!(receiver.stats(2).unwrap().late, 0);
}
