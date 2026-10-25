use engine_net_predict::{
    ErrorSmoother, FixedTimestep, PredictError, PredictedSimulation, PredictionClient,
    PredictionConfig, ReconcileRejection, TickRing,
};
use glam::{Quat, Vec2, Vec3};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
struct Body {
    pos: Vec2,
    vel: Vec2,
}

/// Character controller with acceleration and drag. The server may apply knockback impulses
/// the client cannot predict.
struct Mover {
    impulses: Vec<(u64, Vec2)>,
}
impl PredictedSimulation for Mover {
    type State = Body;
    type Input = Vec2;
    fn step(&mut self, state: &mut Body, input: &Vec2, tick: u64) {
        let dt = 1.0 / 60.0;
        state.vel = state.vel * 0.9 + *input * 0.5;
        for (t, impulse) in &self.impulses {
            if *t == tick {
                state.vel += *impulse;
            }
        }
        state.pos += state.vel * dt;
    }
    fn divergence(&self, predicted: &Body, authoritative: &Body) -> f32 {
        predicted.pos.distance(authoritative.pos) + predicted.vel.distance(authoritative.vel)
    }
}

fn input(tick: u64) -> Vec2 {
    let t = tick as f32;
    Vec2::new((t * 0.37).sin(), (t * 0.11).cos()).clamp_length_max(1.0)
}

fn start() -> Body {
    Body {
        pos: Vec2::new(1.0, -2.0),
        vel: Vec2::ZERO,
    }
}

#[test]
fn misprediction_is_corrected_and_replay_matches_server_resimulation() {
    let mut client = PredictionClient::new(
        Mover { impulses: vec![] },
        PredictionConfig::default(),
        0,
        start(),
    )
    .unwrap();
    for _ in 0..30 {
        let tick = client.tick() + 1;
        client.predict(input(tick)).unwrap();
    }
    // Server: same inputs, plus a knockback at tick 15 the client never saw.
    let mut server = Mover {
        impulses: vec![(15, Vec2::new(3.0, 1.0))],
    };
    let mut authoritative = start();
    for tick in 1..=20 {
        server.step(&mut authoritative, &input(tick), tick);
    }
    let before = client.state().clone();
    let report = client.reconcile(20, &authoritative, Some(20)).unwrap();
    assert!(report.corrected);
    assert_eq!(report.replayed_ticks, 10);
    assert!(report.error > 0.01, "error {}", report.error);
    assert_ne!(*client.state(), before);

    // Server-side re-simulation of the same retained inputs from the authoritative state.
    let mut resim = authoritative.clone();
    for tick in 21..=30 {
        server.step(&mut resim, &input(tick), tick);
    }
    assert_eq!(
        *client.state(),
        resim,
        "replayed state must be bit-identical"
    );
    assert_eq!(client.state_at(20), Some(&authoritative));
    assert_eq!(client.confirmed_tick(), 20);
    println!(
        "reconcile: error {:.4}, replayed {} ticks",
        report.error, report.replayed_ticks
    );
}

#[test]
fn divergence_within_tolerance_keeps_prediction() {
    let mut client = PredictionClient::new(
        Mover { impulses: vec![] },
        PredictionConfig {
            history_ticks: 64,
            tolerance: 1.0e-3,
        },
        0,
        start(),
    )
    .unwrap();
    for _ in 0..12 {
        let tick = client.tick() + 1;
        client.predict(input(tick)).unwrap();
    }
    let mut almost = client.state_at(8).unwrap().clone();
    almost.pos.x += 2.0e-4;
    let before = client.state().clone();
    let report = client.reconcile(8, &almost, Some(8)).unwrap();
    assert!(!report.corrected);
    assert_eq!(report.replayed_ticks, 0);
    assert_eq!(*client.state(), before);
    assert_ne!(client.state_at(8), Some(&almost));
}

#[test]
fn stale_future_and_invalid_server_states_are_ignored() {
    let mut client = PredictionClient::new(
        Mover { impulses: vec![] },
        PredictionConfig {
            history_ticks: 8,
            tolerance: 0.0,
        },
        100,
        start(),
    )
    .unwrap();
    for _ in 0..5 {
        let tick = client.tick() + 1;
        client.predict(input(tick)).unwrap();
    }
    let state = client.state().clone();
    let snapshot = client.state_at(103).unwrap().clone();
    assert_eq!(
        client.reconcile(106, &state, Some(106)),
        Err(ReconcileRejection::FutureTick)
    );
    assert_eq!(
        client.reconcile(103, &snapshot, Some(104)),
        Err(ReconcileRejection::InvalidAck)
    );
    assert!(client.reconcile(103, &snapshot, Some(103)).is_ok());
    assert_eq!(
        client.reconcile(103, &snapshot, Some(103)),
        Err(ReconcileRejection::Stale)
    );
    assert_eq!(
        client.reconcile(101, &snapshot, None),
        Err(ReconcileRejection::Stale)
    );
    assert_eq!(*client.state(), state, "rejections change nothing");

    // History bound: at most history_ticks - 1 unconfirmed ticks.
    while client.predict(Vec2::ZERO).is_ok() {}
    assert_eq!(client.unconfirmed_ticks(), 7);
    assert!(matches!(
        client.predict(Vec2::ZERO),
        Err(PredictError::Full(_))
    ));
    client.reset(500, start());
    assert_eq!((client.tick(), client.confirmed_tick()), (500, 500));
    assert!(client.predict(Vec2::X).is_ok());
}

#[test]
fn smoothing_offset_decays_below_epsilon_within_k_ticks() {
    let half_life = 4.0;
    let epsilon = 1.0e-3;
    let mut smoother = ErrorSmoother::new(half_life, 5.0)
        .unwrap()
        .with_epsilon(epsilon)
        .unwrap();
    let old_t = Vec3::new(1.0, 0.0, 0.0);
    let new_t = Vec3::ZERO;
    let old_r = Quat::from_rotation_y(0.5);
    let new_r = Quat::IDENTITY;
    smoother.add_correction(old_t, old_r, new_t, new_r);
    // Continuity: the rendered pose is unchanged at the moment of correction.
    let (vt, vr) = smoother.apply(new_t, new_r);
    assert!(vt.distance(old_t) < 1.0e-6);
    assert!(vr.dot(old_r).abs() > 1.0 - 1.0e-6);

    let k = ((epsilon as f64).ln() / (smoother.retention() as f64).ln()).ceil() as u32;
    let mut previous = smoother.translation_offset().length();
    for i in 0..k {
        smoother.tick();
        let now = smoother.translation_offset().length();
        assert!(now <= previous, "monotone decay at tick {i}");
        previous = now;
        if i == k / 2 {
            assert!(!smoother.is_settled(), "settled too early");
        }
    }
    assert!(smoother.translation_offset().length() < epsilon);
    assert!(smoother.rotation_error() < epsilon);
    println!(
        "smoothing: half-life {half_life} ticks, 1 m / 0.5 rad settle below {epsilon} in K = {k} ticks"
    );

    let mut snap = ErrorSmoother::new(half_life, 5.0).unwrap();
    snap.add_correction(
        Vec3::new(10.0, 0.0, 0.0),
        Quat::IDENTITY,
        Vec3::ZERO,
        Quat::IDENTITY,
    );
    assert!(snap.is_settled(), "teleport-sized corrections snap");
}

#[test]
fn fixed_timestep_accumulates_exactly_and_caps_catch_up() {
    let mut ts = FixedTimestep::from_hz(60, 5, 10).unwrap();
    let tick = ts.tick_duration();
    let step = ts.advance(tick * 2 + tick / 2);
    assert_eq!(step.ticks(), 10..12);
    assert_eq!(step.dropped, 0);
    assert!((ts.alpha() - 0.5).abs() < 1.0e-6);
    let step = ts.advance(Duration::from_secs(10));
    assert_eq!(step.count, 5);
    assert!(step.dropped >= 590);
    assert_eq!(ts.next_tick(), 17);
    assert!(ts.alpha() < 1.0);
    ts.advance(tick / 2);
    assert!(ts.delay(3) <= tick);
    assert_eq!(ts.alpha(), 0.0);
    assert!(FixedTimestep::new(Duration::ZERO, 1, 0).is_err());
    assert!(FixedTimestep::new(tick, 0, 0).is_err());
}

#[test]
fn tick_ring_is_bounded_and_windowed() {
    let mut ring = TickRing::new(4).unwrap();
    for t in 10..20u64 {
        ring.insert(t, t * 2).unwrap();
    }
    assert_eq!(ring.len(), 4);
    assert_eq!(ring.get(19), Some(&38));
    assert_eq!(ring.get(16), Some(&32));
    assert_eq!(ring.get(15), None);
    assert!(ring.insert(15, 0).is_err(), "below window");
    ring.insert(17, 1).unwrap();
    assert_eq!(ring.get(17), Some(&1));
    ring.insert(100, 5).unwrap();
    assert_eq!(ring.len(), 1, "sparse jump evicts the old window");
    assert_eq!(ring.remove(100), Some(5));
    assert!(ring.is_empty());
    assert!(TickRing::<u8>::new(0).is_err());
}
