//! Small deterministic generator (SplitMix64). Output depends only on the seed.
use glam::DVec3;

pub(crate) struct Rng(u64);
impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub(crate) fn range(&mut self, min: f64, max: f64) -> f64 {
        min + (max - min) * self.unit()
    }
    /// Uniform direction on the unit sphere.
    pub(crate) fn direction(&mut self) -> DVec3 {
        let z = self.range(-1., 1.);
        let angle = self.range(0., std::f64::consts::TAU);
        let r = (1. - z * z).max(0.).sqrt();
        DVec3::new(r * angle.cos(), r * angle.sin(), z)
    }
}
