//! Fixed-capacity history keyed by tick.
use crate::{PredictError, Result};

/// Largest ring capacity any component accepts (65 536 ticks, about 18 minutes at 60 Hz).
pub const MAX_RING_CAPACITY: usize = 1 << 16;

/// Bounded tick-keyed ring buffer.
///
/// The visible window is `[newest - capacity + 1, newest]`. Inserting a newer tick slides the
/// window forward and implicitly evicts older ticks; inserting below the window is rejected.
/// Memory is allocated once at construction and never grows.
#[derive(Clone, Debug)]
pub struct TickRing<T> {
    slots: Box<[Option<(u64, T)>]>,
    newest: Option<u64>,
}
impl<T> TickRing<T> {
    pub fn new(capacity: usize) -> Result<Self> {
        if capacity == 0 || capacity > MAX_RING_CAPACITY {
            return Err(PredictError::Invalid("ring capacity must be 1..=65536"));
        }
        Ok(Self {
            slots: (0..capacity).map(|_| None).collect(),
            newest: None,
        })
    }
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }
    pub fn newest(&self) -> Option<u64> {
        self.newest
    }
    /// Oldest tick the current window can hold (it may be empty).
    pub fn window_start(&self) -> Option<u64> {
        self.newest
            .map(|n| n.saturating_sub(self.capacity() as u64 - 1))
    }
    fn slot(&self, tick: u64) -> usize {
        (tick % self.capacity() as u64) as usize
    }
    fn in_window(&self, tick: u64) -> bool {
        self.newest
            .is_some_and(|n| tick <= n && n - tick < self.capacity() as u64)
    }
    /// Stores `value` at `tick`, replacing any value for the same tick.
    pub fn insert(&mut self, tick: u64, value: T) -> Result<()> {
        if let Some(n) = self.newest
            && tick < n
            && n - tick >= self.capacity() as u64
        {
            return Err(PredictError::Invalid("tick is older than the ring window"));
        }
        let slot = self.slot(tick);
        self.slots[slot] = Some((tick, value));
        self.newest = Some(self.newest.map_or(tick, |n| n.max(tick)));
        Ok(())
    }
    pub fn get(&self, tick: u64) -> Option<&T> {
        if !self.in_window(tick) {
            return None;
        }
        match &self.slots[self.slot(tick)] {
            Some((t, v)) if *t == tick => Some(v),
            _ => None,
        }
    }
    pub fn get_mut(&mut self, tick: u64) -> Option<&mut T> {
        if !self.in_window(tick) {
            return None;
        }
        let slot = self.slot(tick);
        match &mut self.slots[slot] {
            Some((t, v)) if *t == tick => Some(v),
            _ => None,
        }
    }
    pub fn contains(&self, tick: u64) -> bool {
        self.get(tick).is_some()
    }
    pub fn remove(&mut self, tick: u64) -> Option<T> {
        if !self.contains(tick) {
            return None;
        }
        let slot = self.slot(tick);
        self.slots[slot].take().map(|(_, v)| v)
    }
    pub fn clear(&mut self) {
        self.slots.iter_mut().for_each(|s| *s = None);
        self.newest = None;
    }
    /// Number of visible entries (O(capacity)).
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.as_ref().is_some_and(|(t, _)| self.in_window(*t)))
            .count()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
