//! Redundant client input transport and the server-side per-peer input receiver.
//!
//! Wire formats (little-endian, exact length):
//!
//! ```text
//! inputs: u8 version | u8 kind (1) | u8 count (1..=64) | u64 newest_tick | count x Input
//!         inputs are newest first: element i belongs to tick newest_tick - i
//! ack:    u8 version | u8 kind (2) | u8 flags (1 received, 2 processed)
//!         [u64 received_through] [u64 last_processed]
//! ```
use crate::{MessageKind, NetCodec, PredictError, Reader, Result, WIRE_VERSION, Writer};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Hard cap on inputs carried by one message.
pub const MAX_INPUTS_PER_MESSAGE: u8 = 64;
const INPUT_HEADER: usize = 11;

fn message_bound<I: NetCodec>(count: usize) -> usize {
    INPUT_HEADER.saturating_add(count.saturating_mul(I::MAX_ENCODED_BYTES))
}

/// Decoded client input message.
#[derive(Clone, Debug, PartialEq)]
pub struct InputMessage<I> {
    pub newest_tick: u64,
    /// Newest first: `inputs[i]` is the input for `newest_tick - i`.
    pub inputs: Vec<I>,
}
impl<I: NetCodec> InputMessage<I> {
    pub fn ticks(&self) -> impl Iterator<Item = (u64, &I)> {
        self.inputs
            .iter()
            .enumerate()
            .map(|(i, v)| (self.newest_tick - i as u64, v))
    }
    pub fn encode(&self, max_bytes: usize) -> Result<Vec<u8>> {
        let count = self.inputs.len();
        if count == 0 || count > usize::from(MAX_INPUTS_PER_MESSAGE) {
            return Err(PredictError::Invalid("input message needs 1..=64 inputs"));
        }
        if self.newest_tick < count as u64 - 1 {
            return Err(PredictError::Invalid("input ticks would precede tick 0"));
        }
        let mut w = Writer::with_limit(max_bytes.min(message_bound::<I>(count)));
        w.put_u8(WIRE_VERSION)?;
        w.put_u8(MessageKind::Inputs.byte())?;
        w.put_u8(count as u8)?;
        w.put_u64(self.newest_tick)?;
        for input in &self.inputs {
            w.put(input)?;
        }
        Ok(w.into_bytes())
    }
    /// Decodes untrusted bytes; `max_inputs` and `max_bytes` are the receiver's limits.
    pub fn decode(bytes: &[u8], max_inputs: u8, max_bytes: usize) -> Result<Self> {
        if bytes.len() > max_bytes {
            return Err(PredictError::Malformed("input message above byte limit"));
        }
        let mut r = Reader::new(bytes);
        if r.u8()? != WIRE_VERSION {
            return Err(PredictError::Malformed("unsupported version"));
        }
        if r.u8()? != MessageKind::Inputs.byte() {
            return Err(PredictError::Malformed("not an input message"));
        }
        let count = r.u8()?;
        if count == 0 || count > max_inputs.min(MAX_INPUTS_PER_MESSAGE) {
            return Err(PredictError::Malformed("input count outside limits"));
        }
        if bytes.len() > message_bound::<I>(usize::from(count)) {
            return Err(PredictError::Malformed(
                "input message larger than its count allows",
            ));
        }
        let newest_tick = r.u64()?;
        if newest_tick < u64::from(count) - 1 {
            return Err(PredictError::Malformed("input ticks precede tick 0"));
        }
        let mut inputs = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            inputs.push(I::decode(&mut r)?);
        }
        r.finish()?;
        Ok(Self {
            newest_tick,
            inputs,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSenderConfig {
    /// Newest unacknowledged inputs repeated in every message (1..=64). Each input is sent in
    /// up to `redundancy` consecutive messages, so up to `redundancy - 1` consecutive lost
    /// messages lose nothing.
    pub redundancy: u8,
    /// Byte budget per message (keep below the transport's unreliable payload limit). Older
    /// inputs are trimmed first when the budget is exceeded.
    pub max_message_bytes: usize,
}
impl Default for InputSenderConfig {
    fn default() -> Self {
        Self {
            redundancy: 3,
            max_message_bytes: 1200,
        }
    }
}

/// Client side: keeps the newest unacknowledged inputs and encodes redundant messages.
#[derive(Clone, Debug)]
pub struct InputSender<I> {
    config: InputSenderConfig,
    pending: VecDeque<(u64, I)>,
    newest: Option<u64>,
    acked: Option<u64>,
}
impl<I: NetCodec> InputSender<I> {
    pub fn new(config: InputSenderConfig) -> Result<Self> {
        if config.redundancy == 0 || config.redundancy > MAX_INPUTS_PER_MESSAGE {
            return Err(PredictError::Invalid("redundancy must be 1..=64"));
        }
        if config.max_message_bytes < message_bound::<I>(1) {
            return Err(PredictError::Invalid(
                "max_message_bytes cannot hold one input",
            ));
        }
        Ok(Self {
            pending: VecDeque::with_capacity(usize::from(config.redundancy)),
            config,
            newest: None,
            acked: None,
        })
    }
    /// Records the input for `tick`; ticks must strictly increase. A gap discards older
    /// pending inputs because a message carries contiguous ticks.
    pub fn push(&mut self, tick: u64, input: I) -> Result<()> {
        if self.newest.is_some_and(|n| tick <= n) {
            return Err(PredictError::Invalid("input tick must increase"));
        }
        if self.newest.is_some_and(|n| tick != n + 1) {
            self.pending.clear();
        }
        self.newest = Some(tick);
        if self.acked.is_some_and(|a| tick <= a) {
            return Ok(());
        }
        self.pending.push_back((tick, input));
        while self.pending.len() > usize::from(self.config.redundancy) {
            self.pending.pop_front();
        }
        Ok(())
    }
    /// Drops inputs the server reported as received (or already consumed).
    pub fn acknowledge(&mut self, received_through: u64) {
        if self.acked.is_some_and(|a| received_through <= a) {
            return;
        }
        self.acked = Some(received_through);
        while self
            .pending
            .front()
            .is_some_and(|(t, _)| *t <= received_through)
        {
            self.pending.pop_front();
        }
    }
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
    /// Encodes the pending inputs, newest first, or `None` when everything is acknowledged.
    pub fn encode_message(&self) -> Result<Option<Vec<u8>>> {
        let Some(&(newest_tick, _)) = self.pending.back() else {
            return Ok(None);
        };
        let budget = (self.config.max_message_bytes.saturating_sub(INPUT_HEADER))
            / I::MAX_ENCODED_BYTES.max(1);
        let count = self.pending.len().min(budget.max(1));
        let mut w = Writer::with_limit(self.config.max_message_bytes);
        w.put_u8(WIRE_VERSION)?;
        w.put_u8(MessageKind::Inputs.byte())?;
        w.put_u8(count as u8)?;
        w.put_u64(newest_tick)?;
        for (_, input) in self.pending.iter().rev().take(count) {
            w.put(input)?;
        }
        Ok(Some(w.into_bytes()))
    }
}

/// Server -> client input acknowledgement (also embeddable in application state messages).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputAck {
    /// Every tick up to this one has been received or consumed; the client stops resending.
    pub received_through: Option<u64>,
    /// Newest client input tick the server applied; pass it to `PredictionClient::reconcile`.
    pub last_processed: Option<u64>,
}
impl InputAck {
    pub const MAX_BYTES: usize = 19;
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = Writer::with_limit(Self::MAX_BYTES);
        w.put_u8(WIRE_VERSION)?;
        w.put_u8(MessageKind::InputAck.byte())?;
        let flags = u8::from(self.received_through.is_some())
            | (u8::from(self.last_processed.is_some()) << 1);
        w.put_u8(flags)?;
        if let Some(t) = self.received_through {
            w.put_u64(t)?;
        }
        if let Some(t) = self.last_processed {
            w.put_u64(t)?;
        }
        Ok(w.into_bytes())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > Self::MAX_BYTES {
            return Err(PredictError::Malformed("ack above byte limit"));
        }
        let mut r = Reader::new(bytes);
        if r.u8()? != WIRE_VERSION || r.u8()? != MessageKind::InputAck.byte() {
            return Err(PredictError::Malformed("not an input ack"));
        }
        let flags = r.u8()?;
        if flags & !3 != 0 {
            return Err(PredictError::Malformed("unknown ack flags"));
        }
        let received_through = if flags & 1 != 0 { Some(r.u64()?) } else { None };
        let last_processed = if flags & 2 != 0 { Some(r.u64()?) } else { None };
        r.finish()?;
        Ok(Self {
            received_through,
            last_processed,
        })
    }
}

/// What the server does for a tick whose input never arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MissingInputPolicy {
    /// Report `Missing`; the simulation applies its neutral input.
    Skip,
    /// Repeat the last consumed input for up to `max_ticks` consecutive missing ticks, then
    /// report `Missing` (stops a lost "release" from firing forever).
    RepeatLast { max_ticks: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputReceiverConfig {
    /// Inputs may lead the next consumed tick by at most this many ticks (matches
    /// `engine_network::Limits::max_input_tick_lead`, default 256). Also sizes the buffer.
    pub max_lead_ticks: u64,
    /// Largest accepted input count per message.
    pub max_inputs_per_message: u8,
    pub max_message_bytes: usize,
    pub missing: MissingInputPolicy,
}
impl Default for InputReceiverConfig {
    fn default() -> Self {
        Self {
            max_lead_ticks: 256,
            max_inputs_per_message: 16,
            max_message_bytes: 1200,
            missing: MissingInputPolicy::RepeatLast { max_ticks: 8 },
        }
    }
}

/// Result of consuming one server tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TakenInput<I> {
    Received(I),
    /// Input was missing; the last consumed input is repeated by policy.
    Repeated(I),
    Missing,
}
impl<I> TakenInput<I> {
    pub fn input(&self) -> Option<&I> {
        match self {
            Self::Received(i) | Self::Repeated(i) => Some(i),
            Self::Missing => None,
        }
    }
    pub fn is_received(&self) -> bool {
        matches!(self, Self::Received(_))
    }
}

/// Classification of one received input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreOutcome {
    Stored,
    /// Already buffered (redundant copy).
    Duplicate,
    /// Tick already consumed or skipped.
    Late,
    /// Beyond the lead window; dropped.
    TooEarly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReceiveSummary {
    pub stored: u32,
    pub duplicates: u32,
    pub late: u32,
    pub too_early: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct InputReceiverStats {
    pub messages: u64,
    pub malformed: u64,
    pub stored: u64,
    pub duplicates: u64,
    pub late: u64,
    pub too_early: u64,
    pub received_ticks: u64,
    pub repeated_ticks: u64,
    pub missing_ticks: u64,
    /// Ticks the server skipped over in `take` (counted as missing as well).
    pub skipped_ticks: u64,
}

/// Server side, one per peer: dedups redundant inputs, enforces strictly increasing
/// consumption and the lead window, and applies the missing-input policy.
pub struct InputReceiver<I> {
    config: InputReceiverConfig,
    slots: Box<[Option<(u64, I)>]>,
    next_tick: u64,
    last_input: Option<I>,
    repeats: u32,
    last_processed: Option<u64>,
    stats: InputReceiverStats,
}
impl<I: NetCodec + Clone> InputReceiver<I> {
    /// `first_tick` is the first server tick this peer's input will be consumed for.
    pub fn new(config: InputReceiverConfig, first_tick: u64) -> Result<Self> {
        if config.max_lead_ticks == 0 || config.max_lead_ticks >= crate::MAX_RING_CAPACITY as u64 {
            return Err(PredictError::Invalid("max_lead_ticks must be 1..65536"));
        }
        if config.max_inputs_per_message == 0
            || config.max_inputs_per_message > MAX_INPUTS_PER_MESSAGE
        {
            return Err(PredictError::Invalid(
                "max_inputs_per_message must be 1..=64",
            ));
        }
        if config.max_message_bytes < message_bound::<I>(1) {
            return Err(PredictError::Invalid(
                "max_message_bytes cannot hold one input",
            ));
        }
        let capacity = config.max_lead_ticks as usize + 1;
        Ok(Self {
            config,
            slots: (0..capacity).map(|_| None).collect(),
            next_tick: first_tick,
            last_input: None,
            repeats: 0,
            last_processed: None,
            stats: InputReceiverStats::default(),
        })
    }
    pub fn config(&self) -> &InputReceiverConfig {
        &self.config
    }
    pub fn stats(&self) -> InputReceiverStats {
        self.stats
    }
    /// Next tick `take` will consume.
    pub fn next_tick(&self) -> u64 {
        self.next_tick
    }
    pub fn last_processed_input_tick(&self) -> Option<u64> {
        self.last_processed
    }
    fn slot(&self, tick: u64) -> usize {
        (tick % self.slots.len() as u64) as usize
    }
    fn buffered_at(&self, tick: u64) -> bool {
        matches!(&self.slots[self.slot(tick)], Some((t, _)) if *t == tick)
    }
    /// Number of buffered future inputs (bounded by `max_lead_ticks + 1`).
    pub fn buffered(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.as_ref().is_some_and(|(t, _)| *t >= self.next_tick))
            .count()
    }
    /// Highest tick through which every input was received or already consumed.
    pub fn received_through(&self) -> Option<u64> {
        let mut t = self.next_tick;
        while t < self.next_tick.saturating_add(self.slots.len() as u64) && self.buffered_at(t) {
            t += 1;
        }
        t.checked_sub(1)
    }
    pub fn ack(&self) -> InputAck {
        InputAck {
            received_through: self.received_through(),
            last_processed: self.last_processed,
        }
    }
    /// Decodes one untrusted message and buffers its inputs. Malformed messages change nothing.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<ReceiveSummary> {
        self.stats.messages += 1;
        let message = match InputMessage::<I>::decode(
            bytes,
            self.config.max_inputs_per_message,
            self.config.max_message_bytes,
        ) {
            Ok(m) => m,
            Err(e) => {
                self.stats.malformed += 1;
                return Err(e);
            }
        };
        let mut summary = ReceiveSummary::default();
        let newest = message.newest_tick;
        for (i, input) in message.inputs.into_iter().enumerate() {
            match self.store(newest - i as u64, input) {
                StoreOutcome::Stored => summary.stored += 1,
                StoreOutcome::Duplicate => summary.duplicates += 1,
                StoreOutcome::Late => summary.late += 1,
                StoreOutcome::TooEarly => summary.too_early += 1,
            }
        }
        Ok(summary)
    }
    /// Buffers one input from an application-defined wire format.
    pub fn store(&mut self, tick: u64, input: I) -> StoreOutcome {
        let outcome = if tick < self.next_tick {
            StoreOutcome::Late
        } else if tick - self.next_tick > self.config.max_lead_ticks {
            StoreOutcome::TooEarly
        } else if self.buffered_at(tick) {
            StoreOutcome::Duplicate
        } else {
            let slot = self.slot(tick);
            self.slots[slot] = Some((tick, input));
            StoreOutcome::Stored
        };
        match outcome {
            StoreOutcome::Stored => self.stats.stored += 1,
            StoreOutcome::Duplicate => self.stats.duplicates += 1,
            StoreOutcome::Late => self.stats.late += 1,
            StoreOutcome::TooEarly => self.stats.too_early += 1,
        }
        outcome
    }
    /// Consumes the input for server tick `tick` (>= `next_tick`); ticks in between are
    /// skipped and counted missing. Consumption is strictly increasing.
    pub fn take(&mut self, tick: u64) -> Result<TakenInput<I>> {
        if tick < self.next_tick {
            return Err(PredictError::Invalid("input tick already consumed"));
        }
        let skipped = tick - self.next_tick;
        if skipped > 0 {
            self.stats.skipped_ticks += skipped;
            self.stats.missing_ticks += skipped;
            if skipped > self.config.max_lead_ticks {
                self.slots.iter_mut().for_each(|s| *s = None);
            }
        }
        let slot = self.slot(tick);
        let found = match self.slots[slot].take() {
            Some((t, input)) if t == tick => Some(input),
            other => {
                self.slots[slot] = other;
                None
            }
        };
        self.next_tick = tick + 1;
        Ok(match found {
            Some(input) => {
                self.stats.received_ticks += 1;
                self.last_processed = Some(tick);
                self.last_input = Some(input.clone());
                self.repeats = 0;
                TakenInput::Received(input)
            }
            None => {
                self.stats.missing_ticks += 1;
                match (self.config.missing, &self.last_input) {
                    (MissingInputPolicy::RepeatLast { max_ticks }, Some(last))
                        if self.repeats < max_ticks =>
                    {
                        self.repeats += 1;
                        self.stats.repeated_ticks += 1;
                        TakenInput::Repeated(last.clone())
                    }
                    _ => TakenInput::Missing,
                }
            }
        })
    }
}
