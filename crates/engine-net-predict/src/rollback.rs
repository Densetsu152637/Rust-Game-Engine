//! GGPO-style rollback sessions for peer-to-peer or relayed games.
//!
//! Every peer runs the same deterministic [`RollbackGame`] with one local player. Local input
//! is scheduled `input_delay` frames ahead; missing remote input is predicted by repeating the
//! player's newest confirmed input. When a confirmed remote input differs from the input used
//! for an already simulated frame, the session loads the saved state for that frame and
//! re-simulates to the present. Prediction is bounded: once `max_prediction` frames would be
//! predicted for some remote player, [`RollbackSession::advance_frame`] returns
//! [`AdvanceStatus::WaitingForRemote`] without simulating.
//!
//! Wire format (little-endian, exact length):
//!
//! ```text
//! u8 version | u8 kind (4) | u8 flags (1 ack, 2 checksum report) | u8 count
//! u64 start_frame | u64 sender_frame | i32 sender_advantage
//! [u64 ack_frame] [u64 report_frame | u64 report_checksum] | count x Input
//! ```
//!
//! Each message carries every local input the recipient has not acknowledged (capped), the
//! sender's acknowledgement of the recipient's inputs, the sender's frame and frame advantage
//! for time synchronisation, and the sender's newest confirmed checksum report.
use crate::{MessageKind, NetCodec, PredictError, Reader, Result, TickRing, WIRE_VERSION, Writer};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Largest supported player count.
pub const MAX_PLAYERS: u8 = 16;
const HEADER_MAX: usize = 48;
const MAX_EVENTS: usize = 4096;
const RETAINED_REPORTS: usize = 8;

/// Deterministic game driven by a rollback session.
pub trait RollbackGame {
    /// Per-player input for one frame; `Default` is the neutral input used before the first
    /// real input and as the initial prediction.
    type Input: NetCodec + Clone + PartialEq + Default;
    type Snapshot: Clone;
    /// Captures the state at the start of `frame`.
    fn save_state(&self, frame: u64) -> Self::Snapshot;
    fn load_state(&mut self, snapshot: &Self::Snapshot, frame: u64);
    /// Simulates `frame` with one input per player (index = player). Must be deterministic.
    fn advance(&mut self, frame: u64, inputs: &[Self::Input]);
    /// Hash of the current state, compared between peers to detect desyncs.
    fn checksum(&self) -> u64;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackConfig {
    /// Players in the session (2..=16). Every peer must use the same value.
    pub players: u8,
    pub local_player: u8,
    /// Frames between sampling local input and applying it (0..=16). Use the same value on
    /// every peer.
    pub input_delay: u8,
    /// Frames that may be predicted for a remote player before the session stalls (1..=64).
    pub max_prediction: u8,
    /// Confirmed frames between checksum reports; 0 disables desync detection (max 3600).
    pub checksum_interval: u32,
    /// Inputs per outgoing message (1..=64).
    pub max_inputs_per_message: u8,
    pub max_message_bytes: usize,
    /// Emit [`RollbackEvent::Confirmed`] for every confirmed frame.
    pub emit_confirmed_frames: bool,
}
impl Default for RollbackConfig {
    fn default() -> Self {
        Self {
            players: 2,
            local_player: 0,
            input_delay: 2,
            max_prediction: 8,
            checksum_interval: 30,
            max_inputs_per_message: 32,
            max_message_bytes: 1200,
            emit_confirmed_frames: false,
        }
    }
}

/// Result of [`RollbackSession::advance_frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdvanceStatus {
    /// `frame` was simulated.
    Advanced { frame: u64 },
    /// Too many frames would be predicted for `player`; nothing was simulated. Keep polling
    /// the transport and sending messages, then retry.
    WaitingForRemote { player: u8, predicted_frames: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RollbackEvent {
    /// Every input up to `frame` is confirmed; `checksum` is the state after `frame`.
    Confirmed { frame: u64, checksum: u64 },
    /// A peer reported a different checksum for a confirmed frame.
    Desync {
        frame: u64,
        player: u8,
        local_checksum: u64,
        remote_checksum: u64,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RollbackStats {
    pub advanced_frames: u64,
    pub rollbacks: u64,
    pub max_rollback_depth: u64,
    pub resimulated_frames: u64,
    pub stalls: u64,
    pub messages_received: u64,
    pub malformed_messages: u64,
    /// Remote inputs that contradicted an already stored input for the same frame (ignored).
    pub conflicting_inputs: u64,
    /// Remote inputs beyond the acceptance window (ignored).
    pub too_far_ahead_inputs: u64,
    pub desyncs: u64,
    pub dropped_events: u64,
}

/// Frame-advantage based time synchronisation hint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeSync {
    /// Frames this peer should skip (e.g. [`crate::FixedTimestep::delay`]) so peers converge.
    pub recommended_wait_frames: u32,
}

struct DecodedMessage<I> {
    start_frame: u64,
    sender_frame: u64,
    advantage: i32,
    ack: Option<u64>,
    report: Option<(u64, u64)>,
    inputs: Vec<I>,
}

/// One peer's rollback state machine. Sans-IO: feed [`RollbackSession::receive`], send
/// [`RollbackSession::outgoing`], and call [`RollbackSession::advance_frame`] once per frame.
pub struct RollbackSession<G: RollbackGame> {
    config: RollbackConfig,
    players: usize,
    local: usize,
    current_frame: u64,
    local_next: u64,
    inputs: Vec<TickRing<G::Input>>,
    used: Vec<TickRing<G::Input>>,
    confirmed_through: Vec<Option<u64>>,
    basis: Vec<G::Input>,
    first_incorrect: Option<u64>,
    states: TickRing<G::Snapshot>,
    checksums: TickRing<u64>,
    emitted_through: Option<u64>,
    reports: VecDeque<(u64, u64)>,
    pending_report: Vec<Option<(u64, u64)>>,
    checked_through: Vec<Option<u64>>,
    acked_by: Vec<Option<u64>>,
    remote_frame: Vec<Option<u64>>,
    remote_advantage: Vec<i32>,
    scratch: Vec<G::Input>,
    events: VecDeque<RollbackEvent>,
    stats: RollbackStats,
}

impl<G: RollbackGame> RollbackSession<G> {
    pub fn new(config: RollbackConfig) -> Result<Self> {
        if config.players < 2 || config.players > MAX_PLAYERS {
            return Err(PredictError::Invalid("players must be 2..=16"));
        }
        if config.local_player >= config.players {
            return Err(PredictError::Invalid("local_player out of range"));
        }
        if config.input_delay > 16 {
            return Err(PredictError::Invalid("input_delay must be 0..=16"));
        }
        if config.max_prediction == 0 || config.max_prediction > 64 {
            return Err(PredictError::Invalid("max_prediction must be 1..=64"));
        }
        if config.checksum_interval > 3600 {
            return Err(PredictError::Invalid("checksum_interval must be 0..=3600"));
        }
        if config.max_inputs_per_message == 0 || config.max_inputs_per_message > 64 {
            return Err(PredictError::Invalid(
                "max_inputs_per_message must be 1..=64",
            ));
        }
        if config.max_message_bytes < HEADER_MAX + G::Input::MAX_ENCODED_BYTES {
            return Err(PredictError::Invalid(
                "max_message_bytes cannot hold one input",
            ));
        }
        let players = usize::from(config.players);
        let local = usize::from(config.local_player);
        let window =
            2 * usize::from(config.max_prediction) + 2 * usize::from(config.input_delay) + 16;
        let rings = |n| {
            (0..n)
                .map(|_| TickRing::new(window))
                .collect::<Result<Vec<_>>>()
        };
        let mut inputs = rings(players)?;
        let delay = u64::from(config.input_delay);
        for frame in 0..delay {
            inputs[local].insert(frame, G::Input::default())?;
        }
        let mut confirmed_through = vec![None; players];
        confirmed_through[local] = delay.checked_sub(1);
        Ok(Self {
            players,
            local,
            current_frame: 0,
            local_next: delay,
            inputs,
            used: rings(players)?,
            confirmed_through,
            basis: vec![G::Input::default(); players],
            first_incorrect: None,
            states: TickRing::new(window)?,
            checksums: TickRing::new(window)?,
            emitted_through: None,
            reports: VecDeque::with_capacity(RETAINED_REPORTS),
            pending_report: vec![None; players],
            checked_through: vec![None; players],
            acked_by: vec![None; players],
            remote_frame: vec![None; players],
            remote_advantage: vec![0; players],
            scratch: Vec::with_capacity(players),
            events: VecDeque::new(),
            stats: RollbackStats::default(),
            config,
        })
    }
    pub fn config(&self) -> &RollbackConfig {
        &self.config
    }
    pub fn stats(&self) -> RollbackStats {
        self.stats
    }
    /// Next frame to simulate.
    pub fn current_frame(&self) -> u64 {
        self.current_frame
    }
    /// Frame the next `add_local_input` call schedules.
    pub fn next_local_input_frame(&self) -> u64 {
        self.local_next
    }
    /// Newest frame for which every player's input is confirmed.
    pub fn confirmed_frame(&self) -> Option<u64> {
        self.confirmed_through
            .iter()
            .try_fold(u64::MAX, |acc, c| c.map(|c| acc.min(c)))
    }
    /// Checksum of the state after `frame`, if still retained.
    pub fn checksum_at(&self, frame: u64) -> Option<u64> {
        self.checksums.get(frame).copied()
    }
    pub fn drain_events(&mut self) -> Vec<RollbackEvent> {
        self.events.drain(..).collect()
    }
    /// `(local_advantage, remote_advantage)` in frames for a remote player.
    pub fn frame_advantage(&self, player: u8) -> Option<(i64, i64)> {
        let p = usize::from(player);
        if p >= self.players || p == self.local {
            return None;
        }
        let rf = self.remote_frame[p]?;
        Some((
            frame_difference(self.current_frame, rf),
            i64::from(self.remote_advantage[p]),
        ))
    }
    pub fn time_sync(&self) -> TimeSync {
        let wait = (0..self.players)
            .filter_map(|p| self.frame_advantage(p as u8))
            .map(|(local, remote)| local.saturating_sub(remote) / 2)
            .max()
            .unwrap_or(0)
            .clamp(0, i64::from(self.config.max_prediction));
        TimeSync {
            recommended_wait_frames: wait as u32,
        }
    }
    /// Schedules the local player's input for frame `current_frame + input_delay` (one call
    /// per simulated frame). Returns that frame.
    pub fn add_local_input(&mut self, input: G::Input) -> Result<u64> {
        if self.local_next > self.current_frame + u64::from(self.config.input_delay) {
            return Err(PredictError::Invalid(
                "local input for this frame already added",
            ));
        }
        let frame = self.local_next;
        self.inputs[self.local].insert(frame, input)?;
        self.local_next += 1;
        self.confirmed_through[self.local] = Some(frame);
        Ok(frame)
    }
    fn accept_limit(&self) -> u64 {
        self.current_frame
            + u64::from(self.config.max_prediction)
            + 2 * u64::from(self.config.input_delay)
            + 8
    }
    fn decode(&self, bytes: &[u8]) -> Result<DecodedMessage<G::Input>> {
        if bytes.len() > self.config.max_message_bytes {
            return Err(PredictError::Malformed("rollback message above byte limit"));
        }
        let mut r = Reader::new(bytes);
        if r.u8()? != WIRE_VERSION || r.u8()? != MessageKind::Rollback.byte() {
            return Err(PredictError::Malformed("not a rollback message"));
        }
        let flags = r.u8()?;
        if flags & !3 != 0 {
            return Err(PredictError::Malformed("unknown rollback flags"));
        }
        let count = r.u8()?;
        if count > self.config.max_inputs_per_message {
            return Err(PredictError::Malformed(
                "too many inputs in rollback message",
            ));
        }
        let bound = HEADER_MAX
            .saturating_add(usize::from(count).saturating_mul(G::Input::MAX_ENCODED_BYTES));
        if bytes.len() > bound {
            return Err(PredictError::Malformed(
                "rollback message larger than its count allows",
            ));
        }
        let start_frame = r.u64()?;
        start_frame
            .checked_add(u64::from(count))
            .ok_or(PredictError::Malformed("frame overflow"))?;
        let sender_frame = r.u64()?;
        // A legitimate peer stalls within `max_prediction` frames of our inputs, so its frame
        // never exceeds the input acceptance window.
        if sender_frame > self.accept_limit() {
            return Err(PredictError::Malformed(
                "sender frame beyond the acceptance window",
            ));
        }
        let advantage = r.i32()?;
        let ack = if flags & 1 != 0 { Some(r.u64()?) } else { None };
        let report = if flags & 2 != 0 {
            Some((r.u64()?, r.u64()?))
        } else {
            None
        };
        let mut inputs = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            inputs.push(G::Input::decode(&mut r)?);
        }
        r.finish()?;
        if let Some(ack) = ack
            && self
                .local_next
                .checked_sub(1)
                .is_none_or(|newest| ack > newest)
        {
            return Err(PredictError::Malformed("ack beyond inputs ever sent"));
        }
        Ok(DecodedMessage {
            start_frame,
            sender_frame,
            advantage,
            ack,
            report,
            inputs,
        })
    }
    /// Applies one untrusted message from remote player `from` (identity comes from the
    /// transport). Malformed messages change nothing.
    pub fn receive(&mut self, from: u8, bytes: &[u8]) -> Result<()> {
        let p = usize::from(from);
        if p >= self.players || p == self.local {
            return Err(PredictError::Invalid(
                "message sender is not a remote player",
            ));
        }
        self.stats.messages_received += 1;
        let msg = match self.decode(bytes) {
            Ok(m) => m,
            Err(e) => {
                self.stats.malformed_messages += 1;
                return Err(e);
            }
        };
        if let Some(ack) = msg.ack {
            self.acked_by[p] = Some(self.acked_by[p].map_or(ack, |a| a.max(ack)));
        }
        if self.remote_frame[p].is_none_or(|f| msg.sender_frame >= f) {
            self.remote_frame[p] = Some(msg.sender_frame);
            self.remote_advantage[p] = msg.advantage;
        }
        let limit = self.accept_limit();
        for (i, input) in msg.inputs.into_iter().enumerate() {
            let frame = msg.start_frame + i as u64;
            if self.confirmed_through[p].is_some_and(|c| frame <= c) {
                continue;
            }
            if frame > limit {
                self.stats.too_far_ahead_inputs += 1;
                break;
            }
            if let Some(existing) = self.inputs[p].get(frame) {
                if *existing != input {
                    self.stats.conflicting_inputs += 1;
                }
                continue;
            }
            if frame < self.current_frame && self.used[p].get(frame) != Some(&input) {
                self.first_incorrect = Some(self.first_incorrect.map_or(frame, |f| f.min(frame)));
            }
            if self.inputs[p].insert(frame, input).is_err() {
                self.stats.too_far_ahead_inputs += 1;
            }
        }
        let mut next = self.confirmed_through[p].map_or(0, |c| c + 1);
        while let Some(input) = self.inputs[p].get(next) {
            self.basis[p] = input.clone();
            self.confirmed_through[p] = Some(next);
            next += 1;
        }
        if let Some((frame, checksum)) = msg.report {
            self.remote_report(p, frame, checksum);
        }
        Ok(())
    }
    fn push_event(&mut self, event: RollbackEvent) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
            self.stats.dropped_events += 1;
        }
        self.events.push_back(event);
    }
    fn remote_report(&mut self, p: usize, frame: u64, checksum: u64) {
        if self.checked_through[p].is_some_and(|c| frame <= c) {
            return;
        }
        if self.emitted_through.is_some_and(|e| frame <= e) {
            if let Some(&(_, local)) = self.reports.iter().find(|(f, _)| *f == frame)
                && local != checksum
            {
                self.stats.desyncs += 1;
                self.push_event(RollbackEvent::Desync {
                    frame,
                    player: p as u8,
                    local_checksum: local,
                    remote_checksum: checksum,
                });
            }
            self.checked_through[p] = Some(frame);
        } else if self.pending_report[p].is_none_or(|(f, _)| frame > f) {
            self.pending_report[p] = Some((frame, checksum));
        }
    }
    fn process_confirmed(&mut self) {
        let (Some(confirmed), Some(simulated)) =
            (self.confirmed_frame(), self.current_frame.checked_sub(1))
        else {
            return;
        };
        let mut limit = confirmed.min(simulated);
        if let Some(fi) = self.first_incorrect {
            let Some(before) = fi.checked_sub(1) else {
                return;
            };
            limit = limit.min(before);
        }
        let start = self.emitted_through.map_or(0, |e| e + 1);
        if start > limit {
            return;
        }
        let interval = u64::from(self.config.checksum_interval);
        for frame in start..=limit {
            let Some(&checksum) = self.checksums.get(frame) else {
                continue;
            };
            if self.config.emit_confirmed_frames {
                self.push_event(RollbackEvent::Confirmed { frame, checksum });
            }
            if interval > 0 && frame % interval == 0 {
                if self.reports.len() >= RETAINED_REPORTS {
                    self.reports.pop_front();
                }
                self.reports.push_back((frame, checksum));
            }
        }
        self.emitted_through = Some(limit);
        for p in 0..self.players {
            if let Some((frame, checksum)) = self.pending_report[p]
                && frame <= limit
            {
                self.pending_report[p] = None;
                self.remote_report(p, frame, checksum);
            }
        }
    }
    fn simulate_frame(&mut self, game: &mut G, frame: u64) -> Result<()> {
        self.states.insert(frame, game.save_state(frame))?;
        let mut inputs = std::mem::take(&mut self.scratch);
        inputs.clear();
        for p in 0..self.players {
            let input = self.inputs[p].get(frame).unwrap_or(&self.basis[p]).clone();
            if p != self.local {
                self.used[p].insert(frame, input.clone())?;
            }
            inputs.push(input);
        }
        game.advance(frame, &inputs);
        self.scratch = inputs;
        self.checksums.insert(frame, game.checksum())
    }
    fn resimulate(&mut self, game: &mut G, from: u64) -> Result<()> {
        let Some(snapshot) = self.states.get(from) else {
            return Err(PredictError::Invalid("rollback state no longer retained"));
        };
        game.load_state(snapshot, from);
        let depth = self.current_frame - from;
        self.stats.rollbacks += 1;
        self.stats.max_rollback_depth = self.stats.max_rollback_depth.max(depth);
        self.stats.resimulated_frames += depth;
        for frame in from..self.current_frame {
            self.simulate_frame(game, frame)?;
        }
        Ok(())
    }
    /// Rolls back if needed, then simulates `current_frame` unless the prediction window is
    /// exhausted. The local input for the current frame must have been added.
    pub fn advance_frame(&mut self, game: &mut G) -> Result<AdvanceStatus> {
        if let Some(from) = self.first_incorrect.take()
            && from < self.current_frame
        {
            self.resimulate(game, from)?;
        }
        self.process_confirmed();
        let window = u64::from(self.config.max_prediction);
        for p in (0..self.players).filter(|&p| p != self.local) {
            let first_unconfirmed = self.confirmed_through[p].map_or(0, |c| c + 1);
            if self.current_frame >= first_unconfirmed + window {
                self.stats.stalls += 1;
                return Ok(AdvanceStatus::WaitingForRemote {
                    player: p as u8,
                    predicted_frames: self.current_frame - first_unconfirmed,
                });
            }
        }
        if !self.inputs[self.local].contains(self.current_frame) {
            return Err(PredictError::Invalid(
                "local input missing for the current frame",
            ));
        }
        let frame = self.current_frame;
        self.simulate_frame(game, frame)?;
        self.current_frame += 1;
        self.stats.advanced_frames += 1;
        self.process_confirmed();
        Ok(AdvanceStatus::Advanced { frame })
    }
    /// One message per remote player: unacknowledged local inputs (oldest first, capped),
    /// acknowledgement, frame advantage and the newest checksum report.
    pub fn outgoing(&self) -> Result<Vec<(u8, Vec<u8>)>> {
        let per_input = G::Input::MAX_ENCODED_BYTES.max(1);
        let byte_cap = (self.config.max_message_bytes - HEADER_MAX) / per_input;
        let cap = usize::from(self.config.max_inputs_per_message).min(byte_cap) as u64;
        let report = self.reports.back().copied();
        let mut out = Vec::with_capacity(self.players - 1);
        for p in (0..self.players).filter(|&p| p != self.local) {
            let start = self.acked_by[p].map_or(0, |a| a + 1);
            let mut count = self.local_next.saturating_sub(start).min(cap);
            if let Some(missing) = (0..count).find(|i| !self.inputs[self.local].contains(start + i))
            {
                count = missing;
            }
            let ack = self.confirmed_through[p];
            let advantage = self.remote_frame[p].map_or(0, |rf| {
                frame_difference(self.current_frame, rf).clamp(i32::MIN as i64, i32::MAX as i64)
                    as i32
            });
            let mut w = Writer::with_limit(self.config.max_message_bytes);
            w.put_u8(WIRE_VERSION)?;
            w.put_u8(MessageKind::Rollback.byte())?;
            w.put_u8(u8::from(ack.is_some()) | (u8::from(report.is_some()) << 1))?;
            w.put_u8(count as u8)?;
            w.put_u64(start)?;
            w.put_u64(self.current_frame)?;
            w.put_i32(advantage)?;
            if let Some(ack) = ack {
                w.put_u64(ack)?;
            }
            if let Some((frame, checksum)) = report {
                w.put_u64(frame)?;
                w.put_u64(checksum)?;
            }
            for i in 0..count {
                if let Some(input) = self.inputs[self.local].get(start + i) {
                    w.put(input)?;
                }
            }
            out.push((p as u8, w.into_bytes()));
        }
        Ok(out)
    }
}

/// `a - b` in frames, saturated to `i64` (frames are untrusted `u64`s).
fn frame_difference(a: u64, b: u64) -> i64 {
    (i128::from(a) - i128::from(b)).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}
