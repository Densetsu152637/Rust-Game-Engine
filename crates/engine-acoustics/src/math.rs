//! Small vector and affine helpers (row-major 3x4 affines, `[f32; 3]` vectors).

use crate::Affine3;

pub type Vec3 = [f32; 3];

#[inline]
pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
pub fn scale(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
#[inline]
pub fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
#[inline]
pub fn length(a: Vec3) -> f32 {
    dot(a, a).sqrt()
}
/// Unit vector, or `None` for zero or non-finite input.
#[inline]
pub fn normalize(a: Vec3) -> Option<Vec3> {
    let len = length(a);
    (len.is_finite() && len > 0.0).then(|| scale(a, 1.0 / len))
}
#[inline]
pub fn finite3(a: Vec3) -> bool {
    a.iter().all(|v| v.is_finite())
}
#[inline]
pub fn max_abs(a: Vec3) -> f32 {
    a[0].abs().max(a[1].abs()).max(a[2].abs())
}

pub const IDENTITY: Affine3 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];

#[inline]
pub fn transform_point(m: &Affine3, p: Vec3) -> Vec3 {
    [
        m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + m[3],
        m[4] * p[0] + m[5] * p[1] + m[6] * p[2] + m[7],
        m[8] * p[0] + m[9] * p[1] + m[10] * p[2] + m[11],
    ]
}
#[inline]
pub fn transform_vector(m: &Affine3, v: Vec3) -> Vec3 {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
        m[4] * v[0] + m[5] * v[1] + m[6] * v[2],
        m[8] * v[0] + m[9] * v[1] + m[10] * v[2],
    ]
}
/// Transforms a normal with the inverse transpose, given `object_from_world`.
#[inline]
pub fn transform_normal(object_from_world: &Affine3, n: Vec3) -> Vec3 {
    let m = object_from_world;
    [
        m[0] * n[0] + m[4] * n[1] + m[8] * n[2],
        m[1] * n[0] + m[5] * n[1] + m[9] * n[2],
        m[2] * n[0] + m[6] * n[1] + m[10] * n[2],
    ]
}

/// Inverse of an affine, computed in `f64`. `None` when singular or non-finite.
pub fn invert(m: &Affine3) -> Option<Affine3> {
    let a: [f64; 12] = m.map(f64::from);
    let det = a[0] * (a[5] * a[10] - a[6] * a[9]) - a[1] * (a[4] * a[10] - a[6] * a[8])
        + a[2] * (a[4] * a[9] - a[5] * a[8]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let r = [
        (a[5] * a[10] - a[6] * a[9]) * inv,
        (a[2] * a[9] - a[1] * a[10]) * inv,
        (a[1] * a[6] - a[2] * a[5]) * inv,
        (a[6] * a[8] - a[4] * a[10]) * inv,
        (a[0] * a[10] - a[2] * a[8]) * inv,
        (a[2] * a[4] - a[0] * a[6]) * inv,
        (a[4] * a[9] - a[5] * a[8]) * inv,
        (a[1] * a[8] - a[0] * a[9]) * inv,
        (a[0] * a[5] - a[1] * a[4]) * inv,
    ];
    let t = [a[3], a[7], a[11]];
    let out = [
        r[0],
        r[1],
        r[2],
        -(r[0] * t[0] + r[1] * t[1] + r[2] * t[2]),
        r[3],
        r[4],
        r[5],
        -(r[3] * t[0] + r[4] * t[1] + r[5] * t[2]),
        r[6],
        r[7],
        r[8],
        -(r[6] * t[0] + r[7] * t[1] + r[8] * t[2]),
    ]
    .map(|v| v as f32);
    out.iter().all(|v| v.is_finite()).then_some(out)
}

/// Absolute determinant of the linear part.
pub fn determinant(m: &Affine3) -> f64 {
    let a: [f64; 12] = m.map(f64::from);
    a[0] * (a[5] * a[10] - a[6] * a[9]) - a[1] * (a[4] * a[10] - a[6] * a[8])
        + a[2] * (a[4] * a[9] - a[5] * a[8])
}

/// Axis-aligned box. An empty box has `min > max`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}
impl Aabb {
    pub const EMPTY: Self = Self {
        min: [f32::INFINITY; 3],
        max: [f32::NEG_INFINITY; 3],
    };
    #[inline]
    #[allow(clippy::needless_range_loop)]
    pub fn grow(&mut self, p: Vec3) {
        for k in 0..3 {
            self.min[k] = self.min[k].min(p[k]);
            self.max[k] = self.max[k].max(p[k]);
        }
    }
    #[inline]
    pub fn union(mut self, o: &Aabb) -> Self {
        for k in 0..3 {
            self.min[k] = self.min[k].min(o.min[k]);
            self.max[k] = self.max[k].max(o.max[k]);
        }
        self
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.min[0] > self.max[0]
    }
    /// Half surface area (the SAH only needs ratios).
    #[inline]
    pub fn half_area(&self) -> f32 {
        if self.is_empty() {
            return 0.0;
        }
        let d = sub(self.max, self.min);
        d[0] * d[1] + d[1] * d[2] + d[2] * d[0]
    }
    #[inline]
    pub fn centroid(&self) -> Vec3 {
        scale(add(self.min, self.max), 0.5)
    }
    #[allow(clippy::needless_range_loop)]
    /// World box of this box under `m` (all 8 corners).
    pub fn transformed(&self, m: &Affine3) -> Self {
        let mut out = Self::EMPTY;
        if self.is_empty() {
            return out;
        }
        for corner in 0..8 {
            let p = [
                if corner & 1 == 0 {
                    self.min[0]
                } else {
                    self.max[0]
                },
                if corner & 2 == 0 {
                    self.min[1]
                } else {
                    self.max[1]
                },
                if corner & 4 == 0 {
                    self.min[2]
                } else {
                    self.max[2]
                },
            ];
            out.grow(transform_point(m, p));
        }
        out
    }
}

/// Slab test. Returns the entry distance when the ray overlaps `[t_min, t_max]`.
/// `inv_dir` components may be infinite; NaN products from `0 * inf` are ignored by
/// `f32::min`/`max`, which treats a ray parallel to a slab as unbounded on that axis.
#[inline]
pub fn ray_aabb(
    min: Vec3,
    max: Vec3,
    origin: Vec3,
    inv_dir: Vec3,
    t_min: f32,
    t_max: f32,
) -> Option<f32> {
    let mut lo = t_min;
    let mut hi = t_max;
    for k in 0..3 {
        let t1 = (min[k] - origin[k]) * inv_dir[k];
        let t2 = (max[k] - origin[k]) * inv_dir[k];
        lo = lo.max(t1.min(t2));
        hi = hi.min(t1.max(t2));
    }
    (lo <= hi).then_some(lo)
}

/// Double-sided Moller-Trumbore. Returns `t` in `(t_min, t_max]`.
#[inline]
pub fn ray_triangle(
    origin: Vec3,
    dir: Vec3,
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    t_min: f32,
    t_max: f32,
) -> Option<f32> {
    let e1 = sub(v1, v0);
    let e2 = sub(v2, v0);
    let p = cross(dir, e2);
    let det = dot(e1, p);
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(origin, v0);
    let u = dot(s, p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(dir, q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = dot(e2, q) * inv;
    (t > t_min && t <= t_max).then_some(t)
}
