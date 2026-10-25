//! Client-side prediction with server reconciliation.
//!
//! Tick convention (shared with `engine_network::InputCommand::tick`): the input for tick `T`
//! advances the state after tick `T - 1` into the state after tick `T`. The server reports the
//! authoritative state after `server_tick` together with the newest client input tick it had
//! applied (`last_processed_input_tick`); ticks after that used the server's missing-input
//! policy instead of the client's input.
use crate::{PredictError, Result, TickRing};
use serde::{Deserialize, Serialize};

/// Deterministic simulation the client predicts and the server runs authoritatively.
pub trait PredictedSimulation {
    type State: Clone;
    type Input: Clone;
    /// Advances `state` by one tick using `input`. Must be deterministic for equal arguments.
    fn step(&mut self, state: &mut Self::State, input: &Self::Input, tick: u64);
    /// Non-negative error metric between a predicted and an authoritative state. NaN counts as
    /// divergence beyond any tolerance.
    fn divergence(&self, predicted: &Self::State, authoritative: &Self::State) -> f32;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionConfig {
    /// Retained input/state history in ticks. Prediction stops (returns `Full`) once this many
    /// ticks are unconfirmed, so it also bounds how far the client may run ahead.
    pub history_ticks: usize,
    /// Largest divergence accepted without a correction.
    pub tolerance: f32,
}
impl Default for PredictionConfig {
    fn default() -> Self {
        Self {
            history_ticks: 128,
            tolerance: 1.0e-3,
        }
    }
}
impl PredictionConfig {
    pub fn validate(&self) -> Result<()> {
        if self.history_ticks < 2 || self.history_ticks > crate::MAX_RING_CAPACITY {
            return Err(PredictError::Invalid("history_ticks must be 2..=65536"));
        }
        if !self.tolerance.is_finite() || self.tolerance < 0.0 {
            return Err(PredictError::Invalid("tolerance must be finite and >= 0"));
        }
        Ok(())
    }
}

/// Outcome of an accepted server state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReconcileReport {
    pub server_tick: u64,
    /// The prediction diverged beyond tolerance and was rewound and replayed.
    pub corrected: bool,
    /// Ticks re-simulated after the authoritative state (0 when not corrected).
    pub replayed_ticks: u32,
    /// Divergence between the prediction for `server_tick` and the authoritative state.
    pub error: f32,
    /// Echo of the server's newest applied client input tick.
    pub last_processed_input_tick: Option<u64>,
}

/// Why a server state was ignored. No client state changes in any of these cases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileRejection {
    /// At or before the newest already reconciled server tick (reordered or duplicated).
    Stale,
    /// Older than retained history; a newer server state is needed.
    OutsideHistory,
    /// Newer than the client's predicted tick; the caller decides whether to `reset`.
    FutureTick,
    /// The server claims to have applied an input after `server_tick`.
    InvalidAck,
}

/// Records predicted inputs and states per tick and reconciles them with server states.
pub struct PredictionClient<S: PredictedSimulation> {
    sim: S,
    config: PredictionConfig,
    tick: u64,
    confirmed_tick: u64,
    state: S::State,
    states: TickRing<S::State>,
    inputs: TickRing<S::Input>,
    corrections: u64,
    replayed_ticks: u64,
}
impl<S: PredictedSimulation> PredictionClient<S> {
    /// `state` is the authoritative state after `tick`.
    pub fn new(sim: S, config: PredictionConfig, tick: u64, state: S::State) -> Result<Self> {
        config.validate()?;
        let mut states = TickRing::new(config.history_ticks)?;
        states.insert(tick, state.clone())?;
        Ok(Self {
            sim,
            inputs: TickRing::new(config.history_ticks)?,
            config,
            tick,
            confirmed_tick: tick,
            state,
            states,
            corrections: 0,
            replayed_ticks: 0,
        })
    }
    pub fn simulation(&self) -> &S {
        &self.sim
    }
    pub fn simulation_mut(&mut self) -> &mut S {
        &mut self.sim
    }
    pub fn config(&self) -> &PredictionConfig {
        &self.config
    }
    /// Tick of the current predicted state.
    pub fn tick(&self) -> u64 {
        self.tick
    }
    /// Newest server tick accepted by `reconcile` (or the construction/reset tick).
    pub fn confirmed_tick(&self) -> u64 {
        self.confirmed_tick
    }
    pub fn unconfirmed_ticks(&self) -> u64 {
        self.tick - self.confirmed_tick
    }
    pub fn state(&self) -> &S::State {
        &self.state
    }
    pub fn state_at(&self, tick: u64) -> Option<&S::State> {
        self.states.get(tick)
    }
    pub fn input_at(&self, tick: u64) -> Option<&S::Input> {
        self.inputs.get(tick)
    }
    /// Total corrections and replayed ticks since construction.
    pub fn correction_totals(&self) -> (u64, u64) {
        (self.corrections, self.replayed_ticks)
    }
    /// Predicts the next tick with `input` and returns that tick. Fails with `Full` when the
    /// history window holds only unconfirmed ticks; the caller should stall input until the
    /// server catches up (or `reset`).
    pub fn predict(&mut self, input: S::Input) -> Result<u64> {
        if self.unconfirmed_ticks() + 1 >= self.config.history_ticks as u64 {
            return Err(PredictError::Full(
                "prediction history holds only unconfirmed ticks",
            ));
        }
        let tick = self
            .tick
            .checked_add(1)
            .ok_or(PredictError::Invalid("tick overflow"))?;
        self.sim.step(&mut self.state, &input, tick);
        self.inputs.insert(tick, input)?;
        self.states.insert(tick, self.state.clone())?;
        self.tick = tick;
        Ok(tick)
    }
    /// Compares the authoritative state after `server_tick` with the prediction for that tick.
    /// Beyond tolerance, rewinds to the authoritative state and re-simulates every retained
    /// input after it, so the new current state equals the server's re-simulation of the same
    /// inputs.
    pub fn reconcile(
        &mut self,
        server_tick: u64,
        authoritative: &S::State,
        last_processed_input_tick: Option<u64>,
    ) -> std::result::Result<ReconcileReport, ReconcileRejection> {
        if server_tick <= self.confirmed_tick {
            return Err(ReconcileRejection::Stale);
        }
        if server_tick > self.tick {
            return Err(ReconcileRejection::FutureTick);
        }
        if last_processed_input_tick.is_some_and(|t| t > server_tick) {
            return Err(ReconcileRejection::InvalidAck);
        }
        let predicted = self
            .states
            .get(server_tick)
            .ok_or(ReconcileRejection::OutsideHistory)?;
        if (server_tick + 1..=self.tick).any(|t| !self.inputs.contains(t)) {
            return Err(ReconcileRejection::OutsideHistory);
        }
        let error = self.sim.divergence(predicted, authoritative);
        let corrected = !(error <= self.config.tolerance);
        let mut replayed = 0u32;
        if corrected {
            let mut state = authoritative.clone();
            // Inserts below cannot fail: every tick is inside the window checked above.
            let _ = self.states.insert(server_tick, state.clone());
            for t in server_tick + 1..=self.tick {
                if let Some(input) = self.inputs.get(t) {
                    self.sim.step(&mut state, input, t);
                }
                let _ = self.states.insert(t, state.clone());
                replayed += 1;
            }
            self.state = state;
            self.corrections += 1;
            self.replayed_ticks += u64::from(replayed);
        }
        self.confirmed_tick = server_tick;
        Ok(ReconcileReport {
            server_tick,
            corrected,
            replayed_ticks: replayed,
            error,
            last_processed_input_tick,
        })
    }
    /// Discards all history and restarts prediction from an authoritative state (join,
    /// teleport, or recovery after `FutureTick`/`OutsideHistory`).
    pub fn reset(&mut self, tick: u64, state: S::State) {
        self.states.clear();
        self.inputs.clear();
        let _ = self.states.insert(tick, state.clone());
        self.state = state;
        self.tick = tick;
        self.confirmed_tick = tick;
    }
}
