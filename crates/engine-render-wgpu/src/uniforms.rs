//! Per-frame uniform arena planning.
//!
//! All uniform data of one frame is packed into a single staging buffer, each
//! distinct content at its own aligned offset, then uploaded once and bound with
//! dynamic offsets. Distinct contents never share bytes, so a later draw cannot
//! overwrite what an earlier draw in the same frame still needs.

use crate::translate::align_up;
use std::collections::HashMap;

/// Smallest arena allocated.
pub const MIN_ARENA_BYTES: u64 = 64 * 1024;

/// Packs uniform byte blocks at `alignment`-aligned offsets, sharing identical
/// contents.
#[derive(Debug)]
pub struct UniformPacker {
    alignment: u64,
    bytes: Vec<u8>,
    seen: HashMap<Vec<u8>, u32>,
}

impl UniformPacker {
    /// `alignment` is the device's `min_uniform_buffer_offset_alignment`.
    pub fn new(alignment: u32) -> Self {
        Self {
            alignment: u64::from(alignment.max(4)),
            bytes: Vec::new(),
            seen: HashMap::new(),
        }
    }

    /// Offset of `data`, appending it when its exact contents are new.
    pub fn push(&mut self, data: &[u8]) -> u32 {
        if let Some(offset) = self.seen.get(data) {
            return *offset;
        }
        let offset = align_up(self.bytes.len() as u64, self.alignment);
        self.bytes.resize(offset as usize, 0);
        self.bytes.extend_from_slice(data);
        let offset = u32::try_from(offset).unwrap_or(u32::MAX);
        self.seen.insert(data.to_vec(), offset);
        offset
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Bytes to upload, padded to the 4-byte buffer copy alignment.
    pub fn finish(mut self) -> Vec<u8> {
        let padded = align_up(self.bytes.len() as u64, 4) as usize;
        self.bytes.resize(padded, 0);
        self.bytes
    }

    /// Current packed length (unpadded).
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

/// Capacity to allocate when `needed` bytes do not fit: at least the minimum
/// arena, otherwise the next power of two.
pub fn arena_capacity(needed: u64) -> u64 {
    needed.max(MIN_ARENA_BYTES).next_power_of_two()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_contents_get_aligned_offsets() {
        let mut packer = UniformPacker::new(256);
        let a = packer.push(&[1; 64]);
        let b = packer.push(&[2; 64]);
        let c = packer.push(&[3; 16]);
        assert_eq!((a, b, c), (0, 256, 512));
        let bytes = packer.finish();
        assert_eq!(bytes.len(), 528);
        assert_eq!(bytes[0], 1);
        assert_eq!(bytes[256], 2);
        assert_eq!(bytes[512], 3);
        assert_eq!(bytes[100], 0, "gaps are zero padding");
    }

    #[test]
    fn identical_contents_share_one_offset() {
        let mut packer = UniformPacker::new(256);
        let first = packer.push(&[7; 64]);
        let other = packer.push(&[8; 64]);
        let again = packer.push(&[7; 64]);
        assert_eq!(first, again);
        assert_ne!(first, other);
        assert_eq!(packer.len(), 256 + 64);
    }

    #[test]
    fn offsets_respect_odd_device_alignments() {
        let mut packer = UniformPacker::new(64);
        assert_eq!(packer.push(&[1; 4]), 0);
        assert_eq!(packer.push(&[2; 4]), 64);
        assert_eq!(packer.push(&[3; 100]), 128);
        assert_eq!(packer.push(&[4; 4]), 256);
        assert_eq!(packer.finish().len() % 4, 0);
    }

    #[test]
    fn arena_grows_in_powers_of_two_above_the_minimum() {
        assert_eq!(arena_capacity(1), MIN_ARENA_BYTES);
        assert_eq!(arena_capacity(MIN_ARENA_BYTES), MIN_ARENA_BYTES);
        assert_eq!(arena_capacity(MIN_ARENA_BYTES + 1), MIN_ARENA_BYTES * 2);
        assert_eq!(arena_capacity(1_000_000), 1 << 20);
    }

    #[test]
    fn empty_packer_reports_empty() {
        let packer = UniformPacker::new(256);
        assert!(packer.is_empty());
        assert!(packer.finish().is_empty());
    }
}
