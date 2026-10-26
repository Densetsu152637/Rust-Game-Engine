//! Deterministic pseudo-random numbers for AI decisions.
//!
//! The workspace has no RNG dependency and AI must replay bit-for-bit, so every
//! consumer owns a [`SplitMix64`] seeded from authored data. Never seed from time
//! or entity addresses.

/// SplitMix64: tiny, fast, passes BigCrush, and is fully specified by one `u64`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Derives an independent stream from a base seed and a salt (for example a
    /// node index), so equal authored seeds on different nodes still diverge.
    pub fn derive(seed: u64, salt: u64) -> Self {
        let mut mixer = Self::new(seed ^ salt.wrapping_mul(0xA076_1D64_78BD_642F));
        let state = mixer.next_u64();
        Self { state }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..bound` (multiply-shift; the bias is below 2^-32 for the
    /// small bounds used here). Returns 0 when `bound` is 0.
    pub fn next_below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        ((u128::from(self.next_u64()) * bound as u128) >> 64) as usize
    }

    /// In-place Fisher-Yates shuffle.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.next_below(i + 1);
            items.swap(i, j);
        }
    }
}
