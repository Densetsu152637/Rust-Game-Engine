use anyhow::{Result, ensure};
use glam::DVec2;

const MAX_SAMPLES: usize = 64;
/// Ordered samples with linear interpolation and clamping outside the range.
#[derive(Clone, Debug)]
pub struct BlendSpace1D(Vec<f32>);
impl BlendSpace1D {
    pub fn new(points: Vec<f32>) -> Result<Self> {
        ensure!(
            (2..=MAX_SAMPLES).contains(&points.len()),
            "blend1d requires 2..64 samples"
        );
        ensure!(
            points.iter().all(|v| v.is_finite()) && points.windows(2).all(|p| p[1] > p[0]),
            "blend1d positions must be finite and strictly increasing"
        );
        Ok(Self(points))
    }
    pub fn weights(&self, value: f32) -> Result<Vec<f32>> {
        ensure!(value.is_finite(), "nonfinite blend1d parameter");
        let mut weights = vec![0.; self.0.len()];
        let upper = self.0.partition_point(|p| *p < value);
        if upper == 0 {
            weights[0] = 1.;
        } else if upper == self.0.len() {
            weights[upper - 1] = 1.;
        } else {
            let t = ((f64::from(value) - f64::from(self.0[upper - 1]))
                / (f64::from(self.0[upper]) - f64::from(self.0[upper - 1])))
                as f32;
            weights[upper - 1] = 1. - t;
            weights[upper] = t;
        }
        Ok(weights)
    }
}
/// Authored triangle mesh; barycentric interpolation with nearest-triangle clamping.
#[derive(Clone, Debug)]
pub struct BlendSpace2D {
    points: Vec<DVec2>,
    triangles: Vec<[usize; 3]>,
}
impl BlendSpace2D {
    pub fn new(points: Vec<[f32; 2]>, triangles: Vec<[usize; 3]>) -> Result<Self> {
        ensure!(
            (3..=MAX_SAMPLES).contains(&points.len()) && (1..=128).contains(&triangles.len()),
            "blend2d sample/triangle limit"
        );
        let points: Vec<_> = points
            .into_iter()
            .map(|p| DVec2::new(f64::from(p[0]), f64::from(p[1])))
            .collect();
        ensure!(
            points.iter().all(|p| p.is_finite()),
            "nonfinite blend2d position"
        );
        for (i, p) in points.iter().enumerate() {
            ensure!(
                points[..i].iter().all(|q| p != q),
                "duplicate blend2d position"
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut used = vec![false; points.len()];
        for t in &triangles {
            ensure!(
                t.iter().all(|i| *i < points.len()),
                "blend2d triangle index outside samples"
            );
            let mut key = *t;
            key.sort_unstable();
            ensure!(seen.insert(key), "duplicate blend2d triangle");
            let a = points[t[1]] - points[t[0]];
            let b = points[t[2]] - points[t[0]];
            ensure!(
                a.perp_dot(b).abs() > 1e-10 * a.length() * b.length()
                    && a.length_squared() > 0.
                    && b.length_squared() > 0.,
                "degenerate blend2d triangle"
            );
            for i in t {
                used[*i] = true;
            }
        }
        ensure!(used.into_iter().all(|v| v), "unused blend2d sample");
        Ok(Self { points, triangles })
    }
    pub fn weights(&self, value: [f32; 2]) -> Result<Vec<f32>> {
        let p = DVec2::new(f64::from(value[0]), f64::from(value[1]));
        ensure!(p.is_finite(), "nonfinite blend2d parameter");
        let mut best = (f64::INFINITY, [0usize; 3], [0.; 3]);
        for t in &self.triangles {
            let [a, b, c] = t.map(|i| self.points[i]);
            let den = (b - a).perp_dot(c - a);
            let v = (p - a).perp_dot(c - a) / den;
            let w = (b - a).perp_dot(p - a) / den;
            let bary = [1. - v - w, v, w];
            if bary.iter().all(|w| *w >= 0.) {
                best = (0., *t, bary);
                break;
            }
            for (i, j) in [(0, 1), (1, 2), (2, 0)] {
                let a = self.points[t[i]];
                let d = self.points[t[j]] - a;
                let factor = ((p - a).dot(d) / d.length_squared()).clamp(0., 1.);
                let distance = p.distance_squared(a + d * factor);
                if distance < best.0 {
                    let mut w = [0.; 3];
                    w[i] = 1. - factor;
                    w[j] = factor;
                    best = (distance, *t, w);
                }
            }
        }
        let mut weights = vec![0.; self.points.len()];
        for (i, w) in best.1.into_iter().zip(best.2) {
            weights[i] = w as f32;
        }
        Ok(weights)
    }
}
