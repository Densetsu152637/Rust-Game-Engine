//! Fuzz-style decoding of random and mutated bytes plus memory-bound checks under floods.
mod common;

use common::Rng;
use engine_net_predict::{
    InputAck, InputMessage, InputReceiver, InputReceiverConfig, InputSender, InputSenderConfig,
    KeyValidator, KeyValidatorConfig, KeyVerdict, LedgerConfig, MessageKind, PredictionKey,
    PredictionLedger, decode_value, decode_verdicts, encode_value, encode_verdicts,
};
use glam::{Quat, Vec3};

const ITERATIONS: usize = 20_000;

#[test]
fn random_and_mutated_bytes_never_panic_any_decoder() {
    let mut rng = Rng::new(0xF00D);
    let input_seed = InputMessage {
        newest_tick: 99,
        inputs: vec![[1u16, 2, 3]; 4],
    }
    .encode(1200)
    .unwrap();
    let ack_seed = InputAck {
        received_through: Some(5),
        last_processed: Some(4),
    }
    .encode()
    .unwrap();
    let verdict_seed = encode_verdicts(&[KeyVerdict {
        key: PredictionKey::new(9).unwrap(),
        accepted: true,
    }])
    .unwrap();
    let mut receiver = InputReceiver::<[u16; 3]>::new(InputReceiverConfig::default(), 0).unwrap();
    let mut decoded = [0usize; 6];
    for i in 0..ITERATIONS {
        let bytes = match i % 4 {
            0 => rng.bytes(300),
            1 => rng.mutate(&input_seed),
            2 => rng.mutate(&ack_seed),
            _ => rng.mutate(&verdict_seed),
        };
        let _ = MessageKind::peek(&bytes);
        decoded[0] += usize::from(InputMessage::<[u16; 3]>::decode(&bytes, 64, 1200).is_ok());
        decoded[1] += usize::from(InputMessage::<Option<f32>>::decode(&bytes, 64, 1200).is_ok());
        decoded[2] += usize::from(InputAck::decode(&bytes).is_ok());
        decoded[3] += usize::from(decode_verdicts(&bytes).is_ok());
        decoded[4] += usize::from(decode_value::<Quat>(&bytes).is_ok());
        decoded[5] += usize::from(receiver.receive(&bytes).is_ok());
        let _ = decode_value::<(Vec3, PredictionKey)>(&bytes);
    }
    assert!(receiver.buffered() <= 257);
    println!("fuzz: {ITERATIONS} buffers per decoder, accepted {decoded:?}");
}

#[test]
fn valid_values_round_trip_and_non_canonical_values_fail() {
    let mut rng = Rng::new(3);
    for _ in 0..1000 {
        let v = Vec3::new(
            rng.unit() as f32 * 100.0,
            -(rng.unit() as f32),
            rng.unit() as f32,
        );
        assert_eq!(decode_value::<Vec3>(&encode_value(&v).unwrap()).unwrap(), v);
        let q = Quat::from_rotation_y(rng.unit() as f32 * 6.0);
        assert_eq!(decode_value::<Quat>(&encode_value(&q).unwrap()).unwrap(), q);
        let o = rng.chance(0.5).then(|| rng.next_u64());
        assert_eq!(
            decode_value::<Option<u64>>(&encode_value(&o).unwrap()).unwrap(),
            o
        );
    }
    assert!(encode_value(&f32::NAN).is_err());
    assert!(decode_value::<f32>(&f32::INFINITY.to_le_bytes()).is_err());
    assert!(decode_value::<bool>(&[2]).is_err());
    assert!(decode_value::<Option<u8>>(&[2, 0]).is_err());
    assert!(decode_value::<u32>(&[0; 5]).is_err(), "trailing bytes");
    assert!(decode_value::<PredictionKey>(&[0; 4]).is_err());
}

#[test]
fn every_buffer_stays_bounded_under_hostile_floods() {
    let mut rng = Rng::new(17);
    // Server receiver: arbitrary tick claims never grow the buffer past the lead window.
    let mut rx = InputReceiver::<u8>::new(
        InputReceiverConfig {
            max_lead_ticks: 32,
            ..InputReceiverConfig::default()
        },
        1000,
    )
    .unwrap();
    for _ in 0..50_000 {
        let tick = match rng.below(3) {
            0 => rng.next_u64(),
            1 => 1000 + rng.below(64),
            _ => rng.below(1000),
        };
        rx.store(tick, rng.next_u64() as u8);
    }
    assert!(rx.buffered() <= 33);
    assert!(rx.stats().too_early > 0 && rx.stats().late > 0);

    // Client sender keeps only `redundancy` inputs no matter how long it runs unacknowledged.
    let mut tx = InputSender::<u8>::new(InputSenderConfig::default()).unwrap();
    for t in 0..10_000 {
        tx.push(t, t as u8).unwrap();
        assert!(tx.pending() <= 3);
    }

    // Ledger and validator caps.
    let mut ledger = PredictionLedger::new(LedgerConfig::default()).unwrap();
    while ledger.begin(0, None).is_ok() {}
    assert_eq!(ledger.pending_len(), 64);
    let mut validator = KeyValidator::new(KeyValidatorConfig::default()).unwrap();
    for k in 1..=10_000u32 {
        let _ = validator.admit(
            PredictionKey::new(rng.below(u64::from(k)) as u32 + 1).unwrap(),
            None,
        );
    }
    assert!(validator.outstanding_len() <= 64);
}
