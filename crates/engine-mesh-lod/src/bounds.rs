//! Bounding volumes and builders.

use glam::{Mat4, Vec3};

/// Axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    /// Tight box around `points`; `None` for an empty slice.
    pub fn from_points(points: &[Vec3]) -> Option<Self> {
        let first = *points.first()?;
        let (min, max) = points
            .iter()
            .fold((first, first), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        Some(Self { min, max })
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Half-size along each axis.
    pub fn half_extents(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    /// Largest full side length.
    pub fn max_extent(&self) -> f32 {
        (self.max - self.min).max_element()
    }

    /// Smallest box containing both.
    pub fn union(&self, other: &Aabb) -> Aabb {
        Aabb {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    /// Box around the 8 transformed corners (exact for affine transforms).
    pub fn transformed(&self, m: &Mat4) -> Aabb {
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { self.min.x } else { self.max.x },
                if i & 2 == 0 { self.min.y } else { self.max.y },
                if i & 4 == 0 { self.min.z } else { self.max.z },
            );
            let p = m.transform_point3(corner);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        Aabb { min: lo, max: hi }
    }
}

/// Bounding sphere.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingSphere {
    pub center: Vec3,
    pub radius: f32,
}

impl BoundingSphere {
    /// Sphere that circumscribes the box.
    pub fn from_aabb(aabb: &Aabb) -> Self {
        Self {
            center: aabb.center(),
            radius: aabb.half_extents().length(),
        }
    }

    /// Enclosing sphere: Ritter's algorithm compared against the box-centre
    /// sphere; the smaller is returned. Always encloses every point.
    pub fn from_points(points: &[Vec3]) -> Option<Self> {
        let aabb = Aabb::from_points(points)?;
        let center = aabb.center();
        let box_radius = points
            .iter()
            .map(|p| p.distance(center))
            .fold(0.0f32, f32::max);
        let box_sphere = Self {
            center,
            radius: box_radius,
        };

        // Ritter: seed with the most distant pair found from an arbitrary point.
        let x = points[0];
        let y = *points
            .iter()
            .max_by(|a, b| a.distance_squared(x).total_cmp(&b.distance_squared(x)))?;
        let z = *points
            .iter()
            .max_by(|a, b| a.distance_squared(y).total_cmp(&b.distance_squared(y)))?;
        let mut c = (y + z) * 0.5;
        let mut r = y.distance(z) * 0.5;
        for p in points {
            let d = p.distance(c);
            if d > r {
                let new_r = (r + d) * 0.5;
                c += (*p - c) * ((new_r - r) / d);
                r = new_r;
            }
        }
        // Rounding safety: inflate to cover every point exactly.
        let r = points.iter().map(|p| p.distance(c)).fold(r, f32::max);
        let ritter = Self {
            center: c,
            radius: r,
        };
        Some(if ritter.radius < box_sphere.radius {
            ritter
        } else {
            box_sphere
        })
    }

    /// Sphere around the transformed sphere, using the largest axis scale.
    pub fn transformed(&self, m: &Mat4) -> Self {
        let sx = m.x_axis.truncate().length();
        let sy = m.y_axis.truncate().length();
        let sz = m.z_axis.truncate().length();
        Self {
            center: m.transform_point3(self.center),
            radius: self.radius * sx.max(sy).max(sz),
        }
    }
}
