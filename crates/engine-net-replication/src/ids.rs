use crate::error::{ReplicationError, Result};
use crate::wire::{Reader, put_varint, varint_len};
use serde::Serialize;
use std::fmt;

/// Compact server-allocated entity identifier: an index plus a generation that changes every
/// time the index is reused, so a stale packet or RPC can never alias a newer entity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct NetId {
    index: u32,
    generation: u16,
}

impl NetId {
    pub const fn new(index: u32, generation: u16) -> Self {
        Self { index, generation }
    }
    pub const fn index(self) -> u32 {
        self.index
    }
    pub const fn generation(self) -> u16 {
        self.generation
    }
    /// Packs the ID into 48 bits for host-side maps (for example string ID -> `NetId`).
    pub const fn to_raw(self) -> u64 {
        ((self.generation as u64) << 32) | self.index as u64
    }
    pub fn from_raw(raw: u64) -> Option<Self> {
        (raw >> 48 == 0).then(|| Self::new(raw as u32, (raw >> 32) as u16))
    }

    pub(crate) fn put(self, out: &mut Vec<u8>) {
        put_varint(out, u64::from(self.index));
        out.extend_from_slice(&self.generation.to_le_bytes());
    }

    pub(crate) fn wire_len(self) -> usize {
        varint_len(u64::from(self.index)) + 2
    }

    /// Reads an ID, rejecting indices at or above `max_entities`.
    pub(crate) fn read(r: &mut Reader<'_>, max_entities: usize) -> Result<Self> {
        let index = r.varint_u32()?;
        if index as usize >= max_entities {
            return Err(ReplicationError::Limit("entity index above max_entities"));
        }
        Ok(Self::new(index, r.u16()?))
    }
}

impl fmt::Debug for NetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NetId({}v{})", self.index, self.generation)
    }
}
impl fmt::Display for NetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}v{}", self.index, self.generation)
    }
}

/// True when `a` is strictly newer than `b` in wrapping 32-bit sequence space.
pub(crate) fn seq_newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

/// True when generation `a` is strictly newer than `b` in wrapping 16-bit space.
pub(crate) fn gen_newer(a: u16, b: u16) -> bool {
    (a.wrapping_sub(b) as i16) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_round_trip_and_wrap_compare() {
        let id = NetId::new(70_000, 65_535);
        assert_eq!(NetId::from_raw(id.to_raw()), Some(id));
        assert_eq!(NetId::from_raw(1 << 48), None);
        assert!(seq_newer(1, u32::MAX));
        assert!(!seq_newer(u32::MAX, 1));
        assert!(!seq_newer(5, 5));
        assert!(gen_newer(0, u16::MAX));
    }
}
