//! Deterministic, platform-independent random numbers and hashing.
//!
//! Everything here is integer arithmetic (wrapping), so results are bit-identical on every
//! platform. Floating point conversions use exact power-of-two scaling.

/// SplitMix64 finalizer: a strong 64-bit integer mixer.
pub fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// FNV-1a over bytes (stable across platforms; used for stable node ids).
pub fn hash_str(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Derives the seed of one node from the graph seed and the node's stable id.
///
/// The result depends only on `(graph_seed, node_id)`, never on node order, so adding or
/// removing other nodes does not change a node's random stream.
pub fn derive_seed(graph_seed: u64, node_id: &str) -> u64 {
    mix64(graph_seed ^ mix64(hash_str(node_id)))
}

/// Hashes an integer lattice cell together with a seed.
pub fn hash_cell(seed: u64, x: i64, y: i64, z: i64) -> u64 {
    let mut h = mix64(seed ^ 0x9e37_79b9_7f4a_7c15);
    h = mix64(h ^ (x as u64));
    h = mix64(h ^ (y as u64).rotate_left(21));
    mix64(h ^ (z as u64).rotate_left(42))
}

/// SplitMix64 generator.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mix64(self.state)
    }
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }
    /// Uniform in `[0, 1)` with 24 bits of precision.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / 16_777_216.0
    }
    /// Uniform in `[0, 1)` with 53 bits of precision.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / 9_007_199_254_740_992.0
    }
    /// Uniform in `[low, high)`; returns `low` when the range is empty.
    pub fn range_f32(&mut self, low: f32, high: f32) -> f32 {
        if high <= low {
            low
        } else {
            low + (high - low) * self.next_f32()
        }
    }
    /// Uniform integer in `[0, bound)` (unbiased enough for `bound < 2^32`); 0 for `bound == 0`.
    pub fn below(&mut self, bound: u32) -> u32 {
        ((u64::from(self.next_u32()) * u64::from(bound)) >> 32) as u32
    }
    /// `true` with probability `p`.
    pub fn chance(&mut self, p: f32) -> bool {
        self.next_f32() < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_matches_reference_vector() {
        // Reference outputs of SplitMix64 seeded with 0 (Vigna's reference implementation).
        let mut rng = Rng::new(0);
        assert_eq!(rng.next_u64(), 0xe220_a839_7b1d_cdaf);
        assert_eq!(rng.next_u64(), 0x6e78_9e6a_a1b9_65f4);
        assert_eq!(rng.next_u64(), 0x06c4_5d18_8009_454f);
    }

    #[test]
    fn equal_seeds_match_and_different_seeds_differ() {
        let a: Vec<u64> = (0..16).map(|_| 0).collect();
        let mut x = Rng::new(7);
        let mut y = Rng::new(7);
        let mut z = Rng::new(8);
        let xs: Vec<u64> = a.iter().map(|_| x.next_u64()).collect();
        let ys: Vec<u64> = a.iter().map(|_| y.next_u64()).collect();
        let zs: Vec<u64> = a.iter().map(|_| z.next_u64()).collect();
        assert_eq!(xs, ys);
        assert_ne!(xs, zs);
    }

    #[test]
    fn unit_floats_are_in_range_with_expected_moments() {
        let mut rng = Rng::new(42);
        let n = 200_000;
        let (mut sum, mut sq) = (0.0_f64, 0.0_f64);
        for _ in 0..n {
            let v = f64::from(rng.next_f32());
            assert!((0.0..1.0).contains(&v));
            sum += v;
            sq += v * v;
        }
        let mean = sum / f64::from(n);
        let variance = sq / f64::from(n) - mean * mean;
        assert!((mean - 0.5).abs() < 0.005, "mean {mean}");
        assert!((variance - 1.0 / 12.0).abs() < 0.002, "variance {variance}");
    }

    #[test]
    fn below_stays_in_bound_and_covers_range() {
        let mut rng = Rng::new(3);
        let mut seen = [false; 10];
        for _ in 0..2000 {
            let v = rng.below(10) as usize;
            seen[v] = true;
        }
        assert!(seen.iter().all(|s| *s));
        assert_eq!(rng.below(0), 0);
    }

    #[test]
    fn node_seed_depends_only_on_graph_seed_and_id() {
        let a = derive_seed(1, "scatter");
        assert_eq!(a, derive_seed(1, "scatter"));
        assert_ne!(a, derive_seed(2, "scatter"));
        assert_ne!(a, derive_seed(1, "scatter2"));
        assert_eq!(hash_str("abc"), 0xe71f_a219_0541_574b);
    }

    #[test]
    fn cell_hash_distinguishes_axes() {
        assert_ne!(hash_cell(1, 1, 2, 3), hash_cell(1, 2, 1, 3));
        assert_ne!(hash_cell(1, 0, 0, 0), hash_cell(2, 0, 0, 0));
        assert_eq!(hash_cell(5, -3, 4, 9), hash_cell(5, -3, 4, 9));
    }
}
