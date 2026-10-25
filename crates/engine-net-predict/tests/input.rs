mod common;

use common::Rng;
use engine_net_predict::{
    InputAck, InputMessage, InputReceiver, InputReceiverConfig, InputSender, InputSenderConfig,
    MessageKind, MissingInputPolicy, NetCodec, PredictError, Reader, Result, StoreOutcome,
    TakenInput, Writer,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Pad {
    move_x: i8,
    move_y: i8,
    buttons: u8,
}
impl NetCodec for Pad {
    const MAX_ENCODED_BYTES: usize = 3;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        w.put_bytes(&[self.move_x as u8, self.move_y as u8, self.buttons])
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let b = r.take(3)?;
        if b[2] & 0xF0 != 0 {
            return Err(PredictError::Malformed("unknown buttons"));
        }
        Ok(Self {
            move_x: b[0] as i8,
            move_y: b[1] as i8,
            buttons: b[2],
        })
    }
}

fn pad(tick: u64) -> Pad {
    Pad {
        move_x: ((tick % 200) as i16 - 100) as i8,
        move_y: (tick / 3 % 100) as i8,
        buttons: (tick % 16) as u8,
    }
}

const LEAD: u64 = 8;

/// Client sends one redundant message per tick over a lossy, jittery (0..=3 tick, reordering)
/// uplink; the server consumes tick `now - LEAD` and acks over a lossy downlink.
fn run_link(
    redundancy: u8,
    ticks: u64,
    seed: u64,
    mut drop_uplink: impl FnMut(u64, &mut Rng) -> bool,
) -> (u64, u64) {
    let mut rng = Rng::new(seed);
    let mut sender = InputSender::<Pad>::new(InputSenderConfig {
        redundancy,
        max_message_bytes: 1200,
    })
    .unwrap();
    let mut receiver = InputReceiver::<Pad>::new(
        InputReceiverConfig {
            max_inputs_per_message: 64,
            missing: MissingInputPolicy::Skip,
            ..InputReceiverConfig::default()
        },
        0,
    )
    .unwrap();
    let (mut up, mut down): (Vec<(u64, Vec<u8>)>, Vec<(u64, Vec<u8>)>) = (vec![], vec![]);
    let (mut sent, mut lost, mut missing) = (0u64, 0u64, 0u64);
    for now in 0..ticks + LEAD {
        // The client keeps playing after the measured window (pushes continue), so the
        // final measured ticks get their full redundancy too.
        sender.push(now, pad(now)).unwrap();
        assert!(sender.pending() <= usize::from(redundancy));
        {
            if let Some(message) = sender.encode_message().unwrap() {
                if drop_uplink(sent, &mut rng) {
                    lost += 1;
                } else {
                    up.push((now + rng.range(0, 3), message));
                }
                sent += 1;
            }
        }
        up.retain(|(at, bytes)| {
            if *at <= now {
                receiver.receive(bytes).unwrap();
                false
            } else {
                true
            }
        });
        down.retain(|(at, bytes)| {
            if *at <= now {
                let ack = InputAck::decode(bytes).unwrap();
                if let Some(t) = ack.received_through {
                    sender.acknowledge(t);
                }
                false
            } else {
                true
            }
        });
        if now >= LEAD && now - LEAD < ticks {
            let tick = now - LEAD;
            match receiver.take(tick).unwrap() {
                TakenInput::Received(input) => assert_eq!(input, pad(tick)),
                _ => missing += 1,
            }
            if !rng.chance(0.2) {
                down.push((now + rng.range(0, 3), receiver.ack().encode().unwrap()));
            }
        }
    }
    assert_eq!(receiver.stats().missing_ticks, missing);
    assert!(receiver.buffered() <= 257);
    (missing, lost * 1000 / sent.max(1))
}

#[test]
fn redundancy_three_survives_twenty_percent_loss_without_missing_ticks() {
    let ticks = 10_000;
    // Deterministic 20% loss: every fifth message is dropped (plus reordering jitter).
    let (missing, lost_permille) = run_link(3, ticks, 7, |i, _| i % 5 == 4);
    assert!((199..=200).contains(&lost_permille));
    assert_eq!(missing, 0);
    // Seeded random ~20% loss with bursts capped at two consecutive messages.
    let mut run = 0u32;
    let (missing_random, random_permille) = run_link(3, ticks, 11, |_, rng| {
        let drop = run < 2 && rng.chance(0.214);
        run = if drop { run + 1 } else { 0 };
        drop
    });
    assert!((190..=210).contains(&random_permille), "{random_permille}");
    assert_eq!(missing_random, 0);
    println!(
        "redundancy 3, {ticks} ticks: periodic 20.0% loss -> {missing} missing; \
         burst-capped random {:.1}% loss -> {missing_random} missing",
        random_permille as f64 / 10.0
    );
}

#[test]
fn independent_random_loss_matches_the_redundancy_model() {
    let ticks = 10_000;
    let mut report = Vec::new();
    for redundancy in 1..=5u8 {
        let (missing, lost_permille) =
            run_link(redundancy, ticks, 1234 + u64::from(redundancy), |_, rng| {
                rng.chance(0.2)
            });
        let rate = missing as f64 / ticks as f64;
        let expected = 0.2f64.powi(i32::from(redundancy));
        report.push(format!(
            "R={redundancy}: loss {lost_permille}/1000, missing {missing} ({rate:.4}, model {expected:.4})"
        ));
        // Within +-60% of the analytic rate (or a few ticks where the expectation is tiny).
        let lo = (expected * 0.4 * ticks as f64).floor() as u64;
        let hi = (expected * 1.6 * ticks as f64).ceil() as u64 + 3;
        assert!(
            (lo.saturating_sub(3)..=hi).contains(&missing),
            "R={redundancy}: {missing} not in {lo}..={hi}"
        );
    }
    println!(
        "independent 20% loss over {ticks} ticks:\n{}",
        report.join("\n")
    );
}

#[test]
fn receiver_dedups_enforces_windows_and_repeats_last() {
    let mut rx = InputReceiver::<Pad>::new(
        InputReceiverConfig {
            max_lead_ticks: 4,
            missing: MissingInputPolicy::RepeatLast { max_ticks: 2 },
            ..InputReceiverConfig::default()
        },
        10,
    )
    .unwrap();
    assert_eq!(rx.store(9, pad(9)), StoreOutcome::Late);
    assert_eq!(rx.store(15, pad(15)), StoreOutcome::TooEarly);
    assert_eq!(rx.store(11, pad(11)), StoreOutcome::Stored);
    assert_eq!(rx.store(11, pad(99)), StoreOutcome::Duplicate);
    assert_eq!(rx.received_through(), Some(9));
    assert_eq!(
        rx.take(10).unwrap(),
        TakenInput::Missing,
        "nothing to repeat yet"
    );
    assert_eq!(rx.received_through(), Some(11));
    assert_eq!(rx.take(11).unwrap(), TakenInput::Received(pad(11)));
    assert_eq!(rx.last_processed_input_tick(), Some(11));
    assert_eq!(rx.take(12).unwrap(), TakenInput::Repeated(pad(11)));
    assert_eq!(rx.take(13).unwrap(), TakenInput::Repeated(pad(11)));
    assert_eq!(
        rx.take(14).unwrap(),
        TakenInput::Missing,
        "repeat cap reached"
    );
    assert_eq!(rx.last_processed_input_tick(), Some(11));
    assert!(rx.take(14).is_err(), "consumption strictly increases");
    assert_eq!(rx.store(14, pad(14)), StoreOutcome::Late);
    // Skipping ticks counts them missing.
    assert_eq!(rx.store(20, pad(20)), StoreOutcome::TooEarly);
    rx.store(19, pad(19));
    assert_eq!(rx.take(19).unwrap(), TakenInput::Received(pad(19)));
    let stats = rx.stats();
    assert_eq!(stats.skipped_ticks, 4);
    assert_eq!(stats.received_ticks, 2);
    assert_eq!(stats.repeated_ticks, 2);
    assert_eq!(rx.ack().last_processed, Some(19));

    let mut skip = InputReceiver::<Pad>::new(
        InputReceiverConfig {
            missing: MissingInputPolicy::Skip,
            ..InputReceiverConfig::default()
        },
        0,
    )
    .unwrap();
    skip.store(0, pad(0));
    skip.take(0).unwrap();
    assert_eq!(skip.take(1).unwrap(), TakenInput::Missing);
}

#[test]
fn messages_round_trip_and_malformed_ones_change_nothing() {
    let msg = InputMessage {
        newest_tick: 41,
        inputs: vec![pad(41), pad(40), pad(39)],
    };
    let bytes = msg.encode(1200).unwrap();
    assert_eq!(MessageKind::peek(&bytes), Some(MessageKind::Inputs));
    assert_eq!(bytes.len(), 11 + 9);
    let decoded = InputMessage::<Pad>::decode(&bytes, 16, 1200).unwrap();
    assert_eq!(decoded, msg);
    assert_eq!(
        decoded.ticks().map(|(t, _)| t).collect::<Vec<_>>(),
        [41, 40, 39]
    );
    assert!(
        InputMessage::<Pad>::decode(&bytes, 2, 1200).is_err(),
        "count above receiver limit"
    );
    assert!(InputMessage::<Pad>::decode(&bytes[..bytes.len() - 1], 16, 1200).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(InputMessage::<Pad>::decode(&trailing, 16, 1200).is_err());
    let underflow = InputMessage {
        newest_tick: 1,
        inputs: vec![pad(1), pad(0), pad(0)],
    };
    assert!(underflow.encode(1200).is_err());

    let mut rx = InputReceiver::<Pad>::new(InputReceiverConfig::default(), 0).unwrap();
    assert!(rx.receive(&trailing).is_err());
    assert_eq!(rx.buffered(), 0);
    assert_eq!(rx.stats().malformed, 1);
    let summary = rx.receive(&bytes).unwrap();
    assert_eq!(summary.stored, 3);

    for ack in [
        InputAck::default(),
        InputAck {
            received_through: Some(7),
            last_processed: Some(5),
        },
        InputAck {
            received_through: None,
            last_processed: Some(u64::MAX),
        },
    ] {
        assert_eq!(InputAck::decode(&ack.encode().unwrap()).unwrap(), ack);
    }
    assert!(InputAck::decode(&[1, 2, 4]).is_err(), "unknown flag");
}

#[test]
fn sender_trims_to_byte_budget_and_acknowledgements() {
    let mut sender = InputSender::<Pad>::new(InputSenderConfig {
        redundancy: 8,
        max_message_bytes: 11 + 3 * 4,
    })
    .unwrap();
    for t in 0..8 {
        sender.push(t, pad(t)).unwrap();
    }
    let decoded =
        InputMessage::<Pad>::decode(&sender.encode_message().unwrap().unwrap(), 64, 1200).unwrap();
    assert_eq!(decoded.newest_tick, 7);
    assert_eq!(decoded.inputs, [pad(7), pad(6), pad(5), pad(4)]);
    sender.acknowledge(6);
    assert_eq!(sender.pending(), 1);
    sender.acknowledge(7);
    assert!(sender.encode_message().unwrap().is_none());
    assert!(sender.push(7, pad(7)).is_err());
    sender.push(20, pad(20)).unwrap();
    assert_eq!(sender.pending(), 1, "gap restarts the contiguous run");
}
