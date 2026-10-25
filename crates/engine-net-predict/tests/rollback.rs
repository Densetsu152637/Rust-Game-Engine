mod common;

use common::Rng;
use engine_net_predict::{
    AdvanceStatus, NetCodec, PredictError, Reader, Result, RollbackConfig, RollbackEvent,
    RollbackGame, RollbackSession, Writer,
};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Pad {
    dx: i8,
    dy: i8,
    fire: bool,
}
impl NetCodec for Pad {
    const MAX_ENCODED_BYTES: usize = 3;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        w.put_bytes(&[self.dx as u8, self.dy as u8])?;
        w.put_bool(self.fire)
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let dx = r.u8()? as i8;
        let dy = r.u8()? as i8;
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) {
            return Err(PredictError::Malformed("pad axis outside -1..=1"));
        }
        Ok(Self {
            dx,
            dy,
            fire: r.bool()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ArenaState {
    players: Vec<(i32, i32)>,
    ball: (i32, i32, i32, i32),
    score: Vec<u32>,
}

/// Integer arena: players move, "fire" kicks the ball when close, the ball bounces and scores.
struct Arena {
    state: ArenaState,
    /// Injected nondeterminism: this peer adds a point at the given frame.
    glitch_at: Option<u64>,
}
impl Arena {
    fn new(players: usize) -> Self {
        Self {
            state: ArenaState {
                players: (0..players as i32).map(|p| (100 + 200 * p, 300)).collect(),
                ball: (500, 500, 7, -5),
                score: vec![0; players],
            },
            glitch_at: None,
        }
    }
}
impl RollbackGame for Arena {
    type Input = Pad;
    type Snapshot = ArenaState;
    fn save_state(&self, _frame: u64) -> ArenaState {
        self.state.clone()
    }
    fn load_state(&mut self, snapshot: &ArenaState, _frame: u64) {
        self.state = snapshot.clone();
    }
    fn advance(&mut self, frame: u64, inputs: &[Pad]) {
        let s = &mut self.state;
        for (p, input) in inputs.iter().enumerate() {
            let (x, y) = &mut s.players[p];
            *x = (*x + i32::from(input.dx) * 6).clamp(0, 1000);
            *y = (*y + i32::from(input.dy) * 6).clamp(0, 1000);
            let (bx, by, vx, vy) = &mut s.ball;
            if input.fire && (*x - *bx).abs() + (*y - *by).abs() < 120 {
                *vx += (*bx - *x).signum() * 3;
                *vy += (*by - *y).signum() * 3;
                s.score[p] += 1;
            }
        }
        let (bx, by, vx, vy) = &mut s.ball;
        *vx = (*vx).clamp(-20, 20);
        *vy = (*vy).clamp(-20, 20);
        *bx += *vx;
        *by += *vy;
        if !(0..=1000).contains(bx) {
            *vx = -*vx;
            *bx = (*bx).clamp(0, 1000);
        }
        if !(0..=1000).contains(by) {
            *vy = -*vy;
            *by = (*by).clamp(0, 1000);
        }
        if self.glitch_at == Some(frame) {
            s.score[0] += 1;
        }
    }
    fn checksum(&self) -> u64 {
        let s = &self.state;
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |v: i64| {
            for b in v.to_le_bytes() {
                h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
        };
        for &(x, y) in &s.players {
            mix(x.into());
            mix(y.into());
        }
        let (bx, by, vx, vy) = s.ball;
        [bx, by, vx, vy].into_iter().for_each(|v| mix(v.into()));
        s.score.iter().for_each(|&v| mix(v.into()));
        h
    }
}

/// Deterministic human-like input: held for 3..10 frame stretches, so repeat-last prediction
/// is usually right and sometimes wrong.
fn pad_for(player: usize, frame: u64) -> Pad {
    let segment = (frame + player as u64 * 5) / (3 + (player as u64 * 7 + frame / 37) % 8);
    let mut rng = Rng::new(segment * 31 + player as u64 * 1_000_003);
    Pad {
        dx: rng.below(3) as i8 - 1,
        dy: rng.below(3) as i8 - 1,
        fire: rng.below(4) == 0,
    }
}

fn reference_checksums(players: usize, delay: u64, frames: u64) -> Vec<u64> {
    let mut game = Arena::new(players);
    (0..frames)
        .map(|f| {
            let inputs: Vec<Pad> = (0..players)
                .map(|p| {
                    if f < delay {
                        Pad::default()
                    } else {
                        pad_for(p, f)
                    }
                })
                .collect();
            game.advance(f, &inputs);
            game.checksum()
        })
        .collect()
}

struct Link {
    rng: Rng,
    loss: f64,
    min_delay: u64,
    max_delay: u64,
    in_flight: Vec<(u64, u8, u8, Vec<u8>)>,
    sent: u64,
    dropped: u64,
}
impl Link {
    fn send(&mut self, now: u64, from: u8, to: u8, bytes: Vec<u8>) {
        self.sent += 1;
        if self.rng.chance(self.loss) {
            self.dropped += 1;
            return;
        }
        let at = now + self.rng.range(self.min_delay, self.max_delay);
        self.in_flight.push((at, from, to, bytes));
    }
    fn due(&mut self, now: u64) -> Vec<(u8, u8, Vec<u8>)> {
        let mut due = Vec::new();
        self.in_flight.retain(|(at, from, to, bytes)| {
            if *at <= now {
                due.push((*from, *to, bytes.clone()));
                false
            } else {
                true
            }
        });
        due
    }
}

struct Outcome {
    confirmed: Vec<BTreeMap<u64, u64>>,
    desyncs: Vec<Vec<RollbackEvent>>,
    sessions: Vec<RollbackSession<Arena>>,
    stalls: u64,
    link_loss_permille: u64,
}

fn run(players: usize, frames: u64, seed: u64, glitch: Option<(usize, u64)>) -> Outcome {
    let base = RollbackConfig {
        players: players as u8,
        input_delay: 2,
        max_prediction: 8,
        checksum_interval: 30,
        emit_confirmed_frames: true,
        ..RollbackConfig::default()
    };
    let mut sessions: Vec<RollbackSession<Arena>> = (0..players)
        .map(|p| {
            RollbackSession::new(RollbackConfig {
                local_player: p as u8,
                ..base.clone()
            })
            .unwrap()
        })
        .collect();
    let mut games: Vec<Arena> = (0..players).map(|_| Arena::new(players)).collect();
    if let Some((peer, frame)) = glitch {
        games[peer].glitch_at = Some(frame);
    }
    let mut link = Link {
        rng: Rng::new(seed),
        loss: 0.15,
        min_delay: 1,
        max_delay: 4,
        in_flight: vec![],
        sent: 0,
        dropped: 0,
    };
    let mut confirmed = vec![BTreeMap::new(); players];
    let mut desyncs = vec![Vec::new(); players];
    let mut stalls = 0;
    let run_to = frames + 40;
    for now in 0..run_to * 4 {
        for (from, to, bytes) in link.due(now) {
            sessions[usize::from(to)].receive(from, &bytes).unwrap();
        }
        for p in 0..players {
            let s = &mut sessions[p];
            if s.current_frame() < run_to {
                if s.next_local_input_frame() <= s.current_frame() + 2 {
                    let frame = s.next_local_input_frame();
                    s.add_local_input(pad_for(p, frame)).unwrap();
                }
                match s.advance_frame(&mut games[p]).unwrap() {
                    AdvanceStatus::Advanced { .. } => {}
                    AdvanceStatus::WaitingForRemote {
                        predicted_frames, ..
                    } => {
                        assert_eq!(predicted_frames, 8);
                        stalls += 1;
                    }
                }
            }
            for event in s.drain_events() {
                match event {
                    RollbackEvent::Confirmed { frame, checksum } => {
                        assert!(confirmed[p].insert(frame, checksum).is_none());
                    }
                    desync @ RollbackEvent::Desync { .. } => desyncs[p].push(desync),
                }
            }
            for (to, bytes) in s.outgoing().unwrap() {
                link.send(now, p as u8, to, bytes);
            }
        }
        if sessions.iter().all(|s| s.current_frame() >= run_to) {
            break;
        }
    }
    Outcome {
        confirmed,
        desyncs,
        sessions,
        stalls,
        link_loss_permille: link.dropped * 1000 / link.sent.max(1),
    }
}

fn check_against_lockstep(players: usize, seed: u64) {
    let frames = 600;
    let outcome = run(players, frames, seed, None);
    let reference = reference_checksums(players, 2, frames + 40);
    for (p, confirmed) in outcome.confirmed.iter().enumerate() {
        for f in 0..frames {
            let got = confirmed.get(&f).copied();
            assert_eq!(got, Some(reference[f as usize]), "peer {p} frame {f}");
        }
        for (&f, &checksum) in confirmed {
            assert_eq!(checksum, reference[f as usize], "peer {p} frame {f}");
        }
        assert!(outcome.desyncs[p].is_empty());
    }
    let stats: Vec<_> = outcome.sessions.iter().map(|s| s.stats()).collect();
    let rollbacks: u64 = stats.iter().map(|s| s.rollbacks).sum();
    let max_depth = stats.iter().map(|s| s.max_rollback_depth).max().unwrap();
    let resimulated: u64 = stats.iter().map(|s| s.resimulated_frames).sum();
    assert!(rollbacks > 0, "the lossy link must cause mispredictions");
    assert!(max_depth <= 8);
    println!(
        "rollback {players}p seed {seed}: link loss {}/1000, delay 1-4; {} confirmed frames/peer match lockstep; \
         rollbacks {rollbacks}, max depth {max_depth}, resimulated {resimulated}, stalls {}",
        outcome.link_loss_permille,
        outcome.confirmed.iter().map(BTreeMap::len).min().unwrap(),
        outcome.stalls
    );
}

#[test]
fn two_peers_over_lossy_delayed_link_match_lockstep_at_every_confirmed_frame() {
    check_against_lockstep(2, 42);
    check_against_lockstep(2, 4242);
}

#[test]
fn three_peers_match_lockstep() {
    check_against_lockstep(3, 77);
}

#[test]
fn injected_nondeterminism_raises_desync_on_both_peers() {
    let outcome = run(2, 300, 9, Some((1, 100)));
    for p in 0..2 {
        let first = outcome.desyncs[p].first().copied();
        match first {
            Some(RollbackEvent::Desync {
                frame,
                player,
                local_checksum,
                remote_checksum,
            }) => {
                assert_eq!(frame, 120, "first checksum frame after the glitch");
                assert_eq!(usize::from(player), 1 - p);
                assert_ne!(local_checksum, remote_checksum);
            }
            other => panic!("peer {p} missed the desync: {other:?}"),
        }
    }
}

#[test]
fn prediction_window_stalls_explicitly_and_resumes() {
    let config = RollbackConfig {
        input_delay: 0,
        max_prediction: 4,
        ..RollbackConfig::default()
    };
    let mut a = RollbackSession::<Arena>::new(config.clone()).unwrap();
    let mut b = RollbackSession::<Arena>::new(RollbackConfig {
        local_player: 1,
        ..config
    })
    .unwrap();
    let (mut ga, mut gb) = (Arena::new(2), Arena::new(2));
    for frame in 0..4 {
        a.add_local_input(pad_for(0, frame)).unwrap();
        assert_eq!(
            a.advance_frame(&mut ga).unwrap(),
            AdvanceStatus::Advanced { frame }
        );
    }
    a.add_local_input(pad_for(0, 4)).unwrap();
    for _ in 0..3 {
        assert_eq!(
            a.advance_frame(&mut ga).unwrap(),
            AdvanceStatus::WaitingForRemote {
                player: 1,
                predicted_frames: 4
            }
        );
    }
    assert_eq!(a.current_frame(), 4);
    assert_eq!(a.stats().stalls, 3);
    assert!(
        a.add_local_input(Pad::default()).is_err(),
        "one local input per frame"
    );

    for frame in 0..4 {
        b.add_local_input(pad_for(1, frame)).unwrap();
        b.advance_frame(&mut gb).unwrap();
    }
    for (_, bytes) in b.outgoing().unwrap() {
        a.receive(1, &bytes).unwrap();
    }
    assert_eq!(
        a.advance_frame(&mut ga).unwrap(),
        AdvanceStatus::Advanced { frame: 4 }
    );
    assert_eq!(a.confirmed_frame(), Some(3));
    // b ran 4 frames while a was ahead and stalled: a should wait less than b.
    assert_eq!(b.time_sync().recommended_wait_frames, 0);
    assert!(
        a.receive(0, &[1, 4]).is_err(),
        "local player is not a remote sender"
    );
}

#[test]
fn faster_peer_is_told_to_wait() {
    let config = RollbackConfig {
        input_delay: 1,
        max_prediction: 12,
        ..RollbackConfig::default()
    };
    let mut a = RollbackSession::<Arena>::new(config.clone()).unwrap();
    let mut b = RollbackSession::<Arena>::new(RollbackConfig {
        local_player: 1,
        ..config
    })
    .unwrap();
    let (mut ga, mut gb) = (Arena::new(2), Arena::new(2));
    for _ in 0..60 {
        // a runs two frames for every frame of b.
        for _ in 0..2 {
            if a.next_local_input_frame() <= a.current_frame() + 1 {
                let f = a.next_local_input_frame();
                a.add_local_input(pad_for(0, f)).unwrap();
            }
            a.advance_frame(&mut ga).unwrap();
        }
        let f = b.next_local_input_frame();
        if f <= b.current_frame() + 1 {
            b.add_local_input(pad_for(1, f)).unwrap();
        }
        b.advance_frame(&mut gb).unwrap();
        for (_, bytes) in a.outgoing().unwrap() {
            b.receive(0, &bytes).unwrap();
        }
        for (_, bytes) in b.outgoing().unwrap() {
            a.receive(1, &bytes).unwrap();
        }
    }
    let wait_a = a.time_sync().recommended_wait_frames;
    let wait_b = b.time_sync().recommended_wait_frames;
    let (local, remote) = a.frame_advantage(1).unwrap();
    println!("time sync: a advantage {local}, b advantage {remote}; wait a {wait_a}, b {wait_b}");
    assert!(wait_a >= 1, "faster peer waits");
    assert_eq!(wait_b, 0);
}

#[test]
fn malformed_rollback_messages_change_nothing() {
    let mut a = RollbackSession::<Arena>::new(RollbackConfig::default()).unwrap();
    let mut b = RollbackSession::<Arena>::new(RollbackConfig {
        local_player: 1,
        ..RollbackConfig::default()
    })
    .unwrap();
    let mut gb = Arena::new(2);
    b.add_local_input(pad_for(1, 2)).unwrap();
    b.advance_frame(&mut gb).unwrap();
    let valid = b.outgoing().unwrap().remove(0).1;
    let mut rng = Rng::new(5);
    for _ in 0..20_000 {
        let bytes = if rng.chance(0.5) {
            rng.mutate(&valid)
        } else {
            rng.bytes(96)
        };
        let _ = a.receive(1, &bytes);
    }
    assert!(a.stats().malformed_messages > 0);
    // Valid inputs that survived mutation are only accepted within the acceptance window.
    assert!(a.receive(1, &valid).is_ok());
}
