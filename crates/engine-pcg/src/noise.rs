//! Seeded coherent noise: Perlin, Simplex, Worley (cellular) and fractal wrappers.
//!
//! Coordinates are `f64` so absolute (large-world) positions keep precision. All randomness
//! comes from integer hashing of the seed, so output is identical on every platform. The
//! fractal sum order is fixed (octave 0 upwards).
use crate::rng::{Rng, hash_cell};
use serde::{Deserialize, Serialize};

/// Basis noise function.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseKind {
    #[default]
    Perlin,
    Simplex,
    /// Distance to the nearest feature point.
    WorleyF1,
    /// Distance to the second nearest feature point.
    WorleyF2,
    /// `F2 - F1`: cell borders are dark, cell interiors bright.
    WorleyF2MinusF1,
}

/// Fractal wrapper applied over the basis noise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fractal {
    #[default]
    None,
    /// Fractional Brownian motion.
    Fbm,
    /// Ridged: sharp creases where the basis crosses zero.
    Ridged,
}

/// Distance metric for Worley noise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Distance {
    #[default]
    Euclidean,
    Manhattan,
}

/// Complete description of a noise function.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoiseParams {
    pub kind: NoiseKind,
    pub fractal: Fractal,
    pub seed: u64,
    /// Lattice cells per unit length of the first octave.
    pub frequency: f64,
    pub octaves: u32,
    pub lacunarity: f64,
    pub gain: f64,
    pub distance: Distance,
    /// Worley feature point jitter in `[0, 1]`.
    pub jitter: f64,
}

impl Default for NoiseParams {
    fn default() -> Self {
        Self {
            kind: NoiseKind::Perlin,
            fractal: Fractal::None,
            seed: 0,
            frequency: 1.0,
            octaves: 4,
            lacunarity: 2.0,
            gain: 0.5,
            distance: Distance::Euclidean,
            jitter: 1.0,
        }
    }
}

const GRAD3: [[f64; 3]; 12] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
];

const OCTAVE_SHIFT: f64 = 37.719;

/// A noise function ready to sample (holds the seeded permutation table).
#[derive(Clone, Debug)]
pub struct Noise {
    params: NoiseParams,
    perm: [u8; 512],
}

fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}
fn floor_i(x: f64) -> i64 {
    x.floor() as i64
}

impl Noise {
    pub fn new(params: NoiseParams) -> Self {
        let mut table: [u8; 256] = std::array::from_fn(|i| i as u8);
        let mut rng = Rng::new(params.seed ^ 0x5851_f42d_4c95_7f2d);
        for i in (1..256).rev() {
            let j = rng.below(i as u32 + 1) as usize;
            table.swap(i, j);
        }
        let perm = std::array::from_fn(|i| table[i & 255]);
        Self { params, perm }
    }
    pub fn params(&self) -> &NoiseParams {
        &self.params
    }
    fn p(&self, i: i64) -> usize {
        usize::from(self.perm[(i & 255) as usize])
    }

    /// Single-octave Perlin gradient noise in `[-1, 1]`.
    pub fn perlin2(&self, x: f64, y: f64) -> f64 {
        let (xi, yi) = (floor_i(x), floor_i(y));
        let (xf, yf) = (x - xi as f64, y - yi as f64);
        let grad = |h: usize, dx: f64, dy: f64| {
            let g = GRAD3[h % 12];
            g[0] * dx + g[1] * dy
        };
        let h = |ix: i64, iy: i64| self.p(ix + self.p(iy) as i64);
        let (u, v) = (fade(xf), fade(yf));
        let n00 = grad(h(xi, yi), xf, yf);
        let n10 = grad(h(xi + 1, yi), xf - 1.0, yf);
        let n01 = grad(h(xi, yi + 1), xf, yf - 1.0);
        let n11 = grad(h(xi + 1, yi + 1), xf - 1.0, yf - 1.0);
        lerp(lerp(n00, n10, u), lerp(n01, n11, u), v).clamp(-1.0, 1.0)
    }

    /// Single-octave Perlin gradient noise in `[-1, 1]`.
    pub fn perlin3(&self, x: f64, y: f64, z: f64) -> f64 {
        let (xi, yi, zi) = (floor_i(x), floor_i(y), floor_i(z));
        let (xf, yf, zf) = (x - xi as f64, y - yi as f64, z - zi as f64);
        let grad = |h: usize, dx: f64, dy: f64, dz: f64| {
            let g = GRAD3[h % 12];
            g[0] * dx + g[1] * dy + g[2] * dz
        };
        let h = |ix: i64, iy: i64, iz: i64| self.p(ix + self.p(iy + self.p(iz) as i64) as i64);
        let (u, v, w) = (fade(xf), fade(yf), fade(zf));
        let c = |dx: i64, dy: i64, dz: i64| {
            grad(
                h(xi + dx, yi + dy, zi + dz),
                xf - dx as f64,
                yf - dy as f64,
                zf - dz as f64,
            )
        };
        let x00 = lerp(c(0, 0, 0), c(1, 0, 0), u);
        let x10 = lerp(c(0, 1, 0), c(1, 1, 0), u);
        let x01 = lerp(c(0, 0, 1), c(1, 0, 1), u);
        let x11 = lerp(c(0, 1, 1), c(1, 1, 1), u);
        lerp(lerp(x00, x10, v), lerp(x01, x11, v), w).clamp(-1.0, 1.0)
    }

    /// Single-octave 2D simplex noise in `[-1, 1]`.
    pub fn simplex2(&self, x: f64, y: f64) -> f64 {
        const F2: f64 = 0.366_025_403_784_438_6;
        const G2: f64 = 0.211_324_865_405_187_1;
        let s = (x + y) * F2;
        let (i, j) = (floor_i(x + s), floor_i(y + s));
        let t = (i + j) as f64 * G2;
        let (x0, y0) = (x - (i as f64 - t), y - (j as f64 - t));
        let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
        let (x1, y1) = (x0 - i1 as f64 + G2, y0 - j1 as f64 + G2);
        let (x2, y2) = (x0 - 1.0 + 2.0 * G2, y0 - 1.0 + 2.0 * G2);
        let corner = |gi: usize, dx: f64, dy: f64| {
            let t = 0.5 - dx * dx - dy * dy;
            if t < 0.0 {
                0.0
            } else {
                let g = GRAD3[gi % 12];
                let t2 = t * t;
                t2 * t2 * (g[0] * dx + g[1] * dy)
            }
        };
        let n0 = corner(self.p(i + self.p(j) as i64), x0, y0);
        let n1 = corner(self.p(i + i1 + self.p(j + j1) as i64), x1, y1);
        let n2 = corner(self.p(i + 1 + self.p(j + 1) as i64), x2, y2);
        (70.0 * (n0 + n1 + n2)).clamp(-1.0, 1.0)
    }

    /// Single-octave 3D simplex noise in `[-1, 1]`.
    pub fn simplex3(&self, x: f64, y: f64, z: f64) -> f64 {
        const F3: f64 = 1.0 / 3.0;
        const G3: f64 = 1.0 / 6.0;
        let s = (x + y + z) * F3;
        let (i, j, k) = (floor_i(x + s), floor_i(y + s), floor_i(z + s));
        let t = (i + j + k) as f64 * G3;
        let (x0, y0, z0) = (x - (i as f64 - t), y - (j as f64 - t), z - (k as f64 - t));
        let (i1, j1, k1, i2, j2, k2) = if x0 >= y0 {
            if y0 >= z0 {
                (1, 0, 0, 1, 1, 0)
            } else if x0 >= z0 {
                (1, 0, 0, 1, 0, 1)
            } else {
                (0, 0, 1, 1, 0, 1)
            }
        } else if y0 < z0 {
            (0, 0, 1, 0, 1, 1)
        } else if x0 < z0 {
            (0, 1, 0, 0, 1, 1)
        } else {
            (0, 1, 0, 1, 1, 0)
        };
        let corner = |gi: usize, dx: f64, dy: f64, dz: f64| {
            let t = 0.6 - dx * dx - dy * dy - dz * dz;
            if t < 0.0 {
                0.0
            } else {
                let g = GRAD3[gi % 12];
                let t2 = t * t;
                t2 * t2 * (g[0] * dx + g[1] * dy + g[2] * dz)
            }
        };
        let h = |a: i64, b: i64, c: i64| self.p(a + self.p(b + self.p(c) as i64) as i64);
        let n0 = corner(h(i, j, k), x0, y0, z0);
        let n1 = corner(
            h(i + i1, j + j1, k + k1),
            x0 - i1 as f64 + G3,
            y0 - j1 as f64 + G3,
            z0 - k1 as f64 + G3,
        );
        let n2 = corner(
            h(i + i2, j + j2, k + k2),
            x0 - i2 as f64 + 2.0 * G3,
            y0 - j2 as f64 + 2.0 * G3,
            z0 - k2 as f64 + 2.0 * G3,
        );
        let n3 = corner(
            h(i + 1, j + 1, k + 1),
            x0 - 1.0 + 3.0 * G3,
            y0 - 1.0 + 3.0 * G3,
            z0 - 1.0 + 3.0 * G3,
        );
        (32.0 * (n0 + n1 + n2 + n3)).clamp(-1.0, 1.0)
    }

    fn feature_offset(&self, cx: i64, cy: i64, cz: i64) -> [f64; 3] {
        let h = hash_cell(self.params.seed, cx, cy, cz);
        let unit = |bits: u64| (bits & 0x1f_ffff) as f64 / 2_097_152.0;
        let j = self.params.jitter.clamp(0.0, 1.0);
        [
            0.5 + (unit(h) - 0.5) * j,
            0.5 + (unit(h >> 21) - 0.5) * j,
            0.5 + (unit(h >> 42) - 0.5) * j,
        ]
    }

    fn metric(&self, d: [f64; 3]) -> f64 {
        match self.params.distance {
            Distance::Euclidean => (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt(),
            Distance::Manhattan => d[0].abs() + d[1].abs() + d[2].abs(),
        }
    }

    /// Raw `(F1, F2)` Worley distances over the 3x3 (2D) or 3x3x3 (3D) cell neighbourhood.
    pub fn worley(&self, x: f64, y: f64, z: f64, three_d: bool) -> (f64, f64) {
        let (cx, cy, cz) = (floor_i(x), floor_i(y), if three_d { floor_i(z) } else { 0 });
        let (mut f1, mut f2) = (f64::MAX, f64::MAX);
        let zr = if three_d { -1..=1 } else { 0..=0 };
        for dz in zr {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (ix, iy, iz) = (cx + dx, cy + dy, cz + dz);
                    let o = self.feature_offset(ix, iy, iz);
                    let d = self.metric([
                        ix as f64 + o[0] - x,
                        iy as f64 + o[1] - y,
                        if three_d { iz as f64 + o[2] - z } else { 0.0 },
                    ]);
                    if d < f1 {
                        f2 = f1;
                        f1 = d;
                    } else if d < f2 {
                        f2 = d;
                    }
                }
            }
        }
        (f1, f2)
    }

    /// One octave in `[-1, 1]` (Worley outputs are normalized and recentred).
    fn octave(&self, x: f64, y: f64, z: f64, three_d: bool) -> f64 {
        match self.params.kind {
            NoiseKind::Perlin => {
                if three_d {
                    self.perlin3(x, y, z)
                } else {
                    self.perlin2(x, y)
                }
            }
            NoiseKind::Simplex => {
                if three_d {
                    self.simplex3(x, y, z)
                } else {
                    self.simplex2(x, y)
                }
            }
            kind => {
                let (f1, f2) = self.worley(x, y, z, three_d);
                let dims = if three_d { 3.0 } else { 2.0 };
                let scale = match self.params.distance {
                    Distance::Euclidean => f64::sqrt(dims),
                    Distance::Manhattan => dims,
                };
                let v = match kind {
                    NoiseKind::WorleyF1 => f1 / scale,
                    NoiseKind::WorleyF2 => f2 / (scale * 1.5),
                    _ => (f2 - f1) / scale,
                };
                v.clamp(0.0, 1.0) * 2.0 - 1.0
            }
        }
    }

    /// Fractal-combined value in `[-1, 1]`.
    fn signed(&self, x: f64, y: f64, z: f64, three_d: bool) -> f64 {
        let p = &self.params;
        let octaves = if p.fractal == Fractal::None {
            1
        } else {
            p.octaves.clamp(1, 16)
        };
        let (mut freq, mut amp, mut sum, mut norm) = (p.frequency, 1.0, 0.0, 0.0);
        for o in 0..octaves {
            let shift = f64::from(o) * OCTAVE_SHIFT;
            let n = self.octave(
                x * freq + shift,
                y * freq + shift,
                z * freq + shift,
                three_d,
            );
            let v = match p.fractal {
                Fractal::Ridged => {
                    let r = 1.0 - n.abs();
                    r * r * 2.0 - 1.0
                }
                _ => n,
            };
            sum += v * amp;
            norm += amp;
            freq *= p.lacunarity;
            amp *= p.gain;
        }
        if norm > 0.0 { sum / norm } else { 0.0 }
    }

    /// 2D sample in `[0, 1]`.
    pub fn sample_2d(&self, x: f64, y: f64) -> f32 {
        (self.signed(x, y, 0.0, false) * 0.5 + 0.5).clamp(0.0, 1.0) as f32
    }
    /// 3D sample in `[0, 1]`.
    pub fn sample_3d(&self, x: f64, y: f64, z: f64) -> f32 {
        (self.signed(x, y, z, true) * 0.5 + 0.5).clamp(0.0, 1.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(kind: NoiseKind, fractal: Fractal, seed: u64) -> Noise {
        Noise::new(NoiseParams {
            kind,
            fractal,
            seed,
            frequency: 0.37,
            ..Default::default()
        })
    }

    const KINDS: [NoiseKind; 5] = [
        NoiseKind::Perlin,
        NoiseKind::Simplex,
        NoiseKind::WorleyF1,
        NoiseKind::WorleyF2,
        NoiseKind::WorleyF2MinusF1,
    ];
    const FRACTALS: [Fractal; 3] = [Fractal::None, Fractal::Fbm, Fractal::Ridged];

    fn grid() -> impl Iterator<Item = (f64, f64)> {
        (0..120).flat_map(|i| {
            (0..120).map(move |j| (f64::from(i) * 0.731 - 40.1, f64::from(j) * 0.613 + 9.7))
        })
    }

    #[test]
    fn outputs_stay_in_unit_range_for_every_kind_and_fractal() {
        for kind in KINDS {
            for fractal in FRACTALS {
                let n = make(kind, fractal, 11);
                for (x, y) in grid() {
                    let v = n.sample_2d(x, y);
                    assert!((0.0..=1.0).contains(&v), "{kind:?}/{fractal:?} 2d {v}");
                    let w = n.sample_3d(x, y, x * 0.5 - y * 0.25);
                    assert!((0.0..=1.0).contains(&w), "{kind:?}/{fractal:?} 3d {w}");
                }
            }
        }
    }

    #[test]
    fn raw_basis_functions_reach_most_of_the_signed_range() {
        let n = make(NoiseKind::Perlin, Fractal::None, 5);
        let (mut lo, mut hi) = (1.0_f64, -1.0_f64);
        for (x, y) in grid() {
            for v in [
                n.perlin2(x, y),
                n.simplex2(x, y),
                n.perlin3(x, y, 1.5),
                n.simplex3(x, y, -2.25),
            ] {
                assert!((-1.0..=1.0).contains(&v));
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        assert!(lo < -0.6 && hi > 0.6, "range {lo}..{hi}");
    }

    #[test]
    fn deterministic_for_equal_seeds_and_different_for_different_seeds() {
        for kind in KINDS {
            let a = make(kind, Fractal::Fbm, 99);
            let b = make(kind, Fractal::Fbm, 99);
            let c = make(kind, Fractal::Fbm, 100);
            let mut differing = 0;
            let mut total = 0;
            for (x, y) in grid().step_by(37) {
                assert_eq!(a.sample_2d(x, y).to_bits(), b.sample_2d(x, y).to_bits());
                total += 1;
                if (a.sample_2d(x, y) - c.sample_2d(x, y)).abs() > 1e-4 {
                    differing += 1;
                }
            }
            assert!(differing * 10 > total * 9, "{kind:?}: {differing}/{total}");
        }
    }

    #[test]
    fn continuity_small_step_gives_small_change() {
        for kind in KINDS {
            let n = make(kind, Fractal::None, 3);
            let step = 1e-3;
            let mut worst = 0.0_f32;
            for (x, y) in grid().step_by(11) {
                worst = worst.max((n.sample_2d(x, y) - n.sample_2d(x + step, y)).abs());
                worst = worst.max((n.sample_2d(x, y) - n.sample_2d(x, y + step)).abs());
            }
            assert!(worst < 0.01, "{kind:?} jump {worst}");
        }
    }

    #[test]
    fn mean_and_variance_over_large_sample() {
        let expectations = [
            (NoiseKind::Perlin, 0.5, 0.02, 0.01, 0.06),
            (NoiseKind::Simplex, 0.5, 0.02, 0.02, 0.08),
        ];
        for (kind, mean_target, mean_tol, var_min, var_max) in expectations {
            let n = Noise::new(NoiseParams {
                kind,
                seed: 17,
                frequency: 0.5,
                ..Default::default()
            });
            let (mut sum, mut sq, mut count) = (0.0_f64, 0.0_f64, 0.0_f64);
            for i in 0..250 {
                for j in 0..250 {
                    let v = f64::from(
                        n.sample_2d(f64::from(i) * 0.37 + 0.11, f64::from(j) * 0.41 + 0.07),
                    );
                    sum += v;
                    sq += v * v;
                    count += 1.0;
                }
            }
            let mean = sum / count;
            let var = sq / count - mean * mean;
            assert!(
                (mean - mean_target).abs() < mean_tol,
                "{kind:?} mean {mean}"
            );
            assert!((var_min..var_max).contains(&var), "{kind:?} variance {var}");
        }
    }

    #[test]
    fn worley_f1_is_bounded_and_f2_is_not_smaller() {
        let n = make(NoiseKind::WorleyF1, Fractal::None, 8);
        for (x, y) in grid().step_by(7) {
            let (f1, f2) = n.worley(x, y, 0.0, false);
            assert!(f1 <= f2 + 1e-12);
            assert!(f1 <= 2.0_f64.sqrt() + 1e-9, "f1 {f1}");
        }
    }

    #[test]
    fn worley_manhattan_distance_dominates_euclidean_for_the_same_feature() {
        let euclid = Noise::new(NoiseParams {
            kind: NoiseKind::WorleyF1,
            seed: 4,
            ..Default::default()
        });
        let manhattan = Noise::new(NoiseParams {
            kind: NoiseKind::WorleyF1,
            seed: 4,
            distance: Distance::Manhattan,
            ..Default::default()
        });
        // Manhattan distance >= Euclidean for any one feature; F1 minima may pick another
        // feature but the Manhattan minimum can never be below the Euclidean minimum.
        for (x, y) in grid().step_by(13) {
            let (e, _) = euclid.worley(x, y, 0.0, false);
            let (m, _) = manhattan.worley(x, y, 0.0, false);
            assert!(m >= e - 1e-9, "manhattan {m} < euclid {e}");
        }
    }

    #[test]
    fn worley_without_jitter_has_exact_cell_center_distance() {
        let n = Noise::new(NoiseParams {
            kind: NoiseKind::WorleyF1,
            jitter: 0.0,
            ..Default::default()
        });
        let (f1, f2) = n.worley(0.5, 0.5, 0.0, false);
        assert!(f1.abs() < 1e-12);
        assert!((f2 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn ridged_mean_exceeds_fbm_mean() {
        let ridged = make(NoiseKind::Perlin, Fractal::Ridged, 2);
        let fbm = make(NoiseKind::Perlin, Fractal::Fbm, 2);
        let mean = |n: &Noise| {
            grid()
                .map(|(x, y)| f64::from(n.sample_2d(x, y)))
                .sum::<f64>()
                / 14400.0
        };
        assert!(mean(&ridged) > mean(&fbm) + 0.1);
    }

    #[test]
    fn fractal_octaves_add_detail() {
        let base = Noise::new(NoiseParams {
            fractal: Fractal::Fbm,
            octaves: 1,
            seed: 6,
            frequency: 0.5,
            ..Default::default()
        });
        let detailed = Noise::new(NoiseParams {
            fractal: Fractal::Fbm,
            octaves: 6,
            gain: 0.7,
            seed: 6,
            frequency: 0.5,
            ..Default::default()
        });
        let roughness = |n: &Noise| {
            (0..2000)
                .map(|i| {
                    let x = f64::from(i) * 0.05;
                    f64::from((n.sample_2d(x + 0.05, 0.3) - n.sample_2d(x, 0.3)).abs())
                })
                .sum::<f64>()
        };
        assert!(roughness(&detailed) > roughness(&base) * 1.2);
    }

    #[test]
    fn golden_values_are_stable() {
        let perlin = Noise::new(NoiseParams {
            seed: 1234,
            ..Default::default()
        });
        let simplex = Noise::new(NoiseParams {
            kind: NoiseKind::Simplex,
            seed: 1234,
            ..Default::default()
        });
        let worley = Noise::new(NoiseParams {
            kind: NoiseKind::WorleyF1,
            seed: 1234,
            ..Default::default()
        });
        let fbm = Noise::new(NoiseParams {
            fractal: Fractal::Fbm,
            seed: 1234,
            ..Default::default()
        });
        let got = [
            perlin.sample_2d(0.3, 0.7),
            perlin.sample_3d(1.3, 2.7, 3.1),
            simplex.sample_2d(0.3, 0.7),
            simplex.sample_3d(1.3, 2.7, 3.1),
            worley.sample_2d(0.3, 0.7),
            fbm.sample_2d(5.25, -3.5),
        ];
        for (g, e) in got.iter().zip(GOLDEN) {
            assert!((g - e).abs() < 1e-6, "golden mismatch got {got:?}");
        }
    }
    /// Pinned from real output on first run; any change breaks cross-platform determinism.
    const GOLDEN: [f32; 6] = [
        0.308_703_57,
        0.526_507_74,
        0.061_402_17,
        0.571_970_3,
        0.309_169_1,
        0.539_193_8,
    ];
}
