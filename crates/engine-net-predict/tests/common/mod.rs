#![allow(dead_code)]

/// Seeded SplitMix64 generator so every test run is reproducible.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
    pub fn range(&mut self, lo: u64, hi_inclusive: u64) -> u64 {
        lo + self.below(hi_inclusive - lo + 1)
    }
    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
    pub fn bytes(&mut self, max_len: usize) -> Vec<u8> {
        let len = self.below(max_len as u64 + 1) as usize;
        (0..len).map(|_| self.next_u64() as u8).collect()
    }
    /// Random corruption of a valid message: flips, truncation, extension or byte overwrite.
    pub fn mutate(&mut self, valid: &[u8]) -> Vec<u8> {
        let mut out = valid.to_vec();
        match self.below(4) {
            0 if !out.is_empty() => {
                let i = self.below(out.len() as u64) as usize;
                out[i] ^= 1 << self.below(8);
            }
            1 => out.truncate(self.below(out.len() as u64 + 1) as usize),
            2 => {
                let extra = self.below(16) as usize;
                out.extend((0..extra).map(|_| self.next_u64() as u8));
            }
            _ if !out.is_empty() => {
                let i = self.below(out.len() as u64) as usize;
                out[i] = self.next_u64() as u8;
            }
            _ => {}
        }
        out
    }
}
