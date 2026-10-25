//! Seeded, platform-independent random numbers (SplitMix64 and Box-Muller).

/// SplitMix64 generator.
#[derive(Clone, Debug)]
pub struct SplitMix64(u64);
impl SplitMix64 {
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
    /// Uniform in (0, 1].
    pub fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    /// A pair of independent standard normal deviates.
    pub fn gaussian_pair(&mut self) -> (f32, f32) {
        let (u1, u2) = (self.unit(), self.unit());
        let r = (-2.0 * u1.ln()).sqrt();
        let a = std::f64::consts::TAU * u2;
        ((r * a.cos()) as f32, (r * a.sin()) as f32)
    }
}
