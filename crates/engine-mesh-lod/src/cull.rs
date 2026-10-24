//! Frustum extraction and conservative visibility tests.

use glam::{Mat4, Vec3, Vec4};

use crate::bounds::{Aabb, BoundingSphere};

/// Result of testing a volume against a frustum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Containment {
    /// Entirely outside at least one plane.
    Outside,
    /// Not provably outside; may straddle a plane (AABB tests may report this
    /// for boxes that only miss the frustum near a corner).
    Intersecting,
    /// Entirely inside all planes.
    Inside,
}

/// Six normalised planes `(nx, ny, nz, d)` with inward-facing normals:
/// a point is inside when `n.p + d >= 0`. Order: left, right, bottom, top,
/// near, far. A degenerate plane (e.g. far plane of an infinite projection)
/// is stored as zero and accepts everything.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frustum {
    pub planes: [Vec4; 6],
}

fn normalize_plane(p: Vec4) -> Vec4 {
    let len = p.truncate().length();
    if len > 1e-12 && len.is_finite() { p / len } else { Vec4::ZERO }
}

impl Frustum {
    /// Extracts planes from a view-projection matrix (Gribb-Hartmann) for
    /// right-handed, WebGPU clip space: `-w <= x,y <= w`, `0 <= z <= w`.
    /// Works for reversed-Z and infinite far planes.
    pub fn from_view_projection(vp: &Mat4) -> Self {
        let r0 = vp.row(0);
        let r1 = vp.row(1);
        let r2 = vp.row(2);
        let r3 = vp.row(3);
        Self {
            planes: [
                normalize_plane(r3 + r0),
                normalize_plane(r3 - r0),
                normalize_plane(r3 + r1),
                normalize_plane(r3 - r1),
                normalize_plane(r2),
                normalize_plane(r3 - r2),
            ],
        }
    }

    /// Signed distance of `p` to plane `i` (positive inside).
    #[inline]
    fn signed(plane: &Vec4, p: Vec3) -> f32 {
        plane.truncate().dot(p) + plane.w
    }

    /// `true` when the point is inside or on every plane.
    pub fn contains_point(&self, p: Vec3) -> bool {
        self.planes.iter().all(|pl| Self::signed(pl, p) >= 0.0)
    }

    /// Classifies a sphere. Never reports `Outside` for a visible sphere.
    pub fn test_sphere(&self, s: &BoundingSphere) -> Containment {
        let mut result = Containment::Inside;
        for pl in &self.planes {
            let d = Self::signed(pl, s.center);
            if d < -s.radius {
                return Containment::Outside;
            }
            if d < s.radius {
                result = Containment::Intersecting;
            }
        }
        result
    }

    /// Classifies a box with the positive/negative-vertex test (conservative).
    pub fn test_aabb(&self, b: &Aabb) -> Containment {
        let mut result = Containment::Inside;
        for pl in &self.planes {
            let n = pl.truncate();
            let positive = Vec3::select(n.cmpge(Vec3::ZERO), b.max, b.min);
            let negative = Vec3::select(n.cmpge(Vec3::ZERO), b.min, b.max);
            if n.dot(positive) + pl.w < 0.0 {
                return Containment::Outside;
            }
            if n.dot(negative) + pl.w < 0.0 {
                result = Containment::Intersecting;
            }
        }
        result
    }

    pub fn intersects_sphere(&self, s: &BoundingSphere) -> bool {
        self.test_sphere(s) != Containment::Outside
    }

    pub fn intersects_aabb(&self, b: &Aabb) -> bool {
        self.test_aabb(b) != Containment::Outside
    }
}

/// Writes the indices (ascending) of spheres not outside `frustum` into `out`,
/// clearing it first. Reuse `out` across frames to avoid allocation.
pub fn cull_spheres(frustum: &Frustum, spheres: &[BoundingSphere], out: &mut Vec<u32>) {
    out.clear();
    out.extend(
        spheres
            .iter()
            .enumerate()
            .filter(|(_, s)| frustum.intersects_sphere(s))
            .map(|(i, _)| i as u32),
    );
}

/// Writes the indices (ascending) of boxes not outside `frustum` into `out`,
/// clearing it first.
pub fn cull_aabbs(frustum: &Frustum, boxes: &[Aabb], out: &mut Vec<u32>) {
    out.clear();
    out.extend(
        boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| frustum.intersects_aabb(b))
            .map(|(i, _)| i as u32),
    );
}
