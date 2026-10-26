//! Ground description the generator runs against.
//!
//! Terrain (or any other height source) implements [`Surface`]; the crate ships flat and
//! closure-driven analytic implementations for tests and simple games.
use glam::Vec3;
use std::collections::BTreeMap;

/// Axis-aligned box in local space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn new(min: [f32; 3], max: [f32; 3]) -> Self {
        Self {
            min: Vec3::from(min),
            max: Vec3::from(max),
        }
    }
    /// A box so large it never clips (used for unbounded flat ground).
    pub fn unbounded() -> Self {
        Self::new([-1.0e9; 3], [1.0e9; 3])
    }
    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }
    pub fn contains(&self, p: Vec3) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }
    /// Intersection of the horizontal (x, z) extents; y spans both boxes.
    pub fn intersect_xz(&self, other: &Aabb) -> Aabb {
        Aabb {
            min: Vec3::new(
                self.min.x.max(other.min.x),
                self.min.y.min(other.min.y),
                self.min.z.max(other.min.z),
            ),
            max: Vec3::new(
                self.max.x.min(other.max.x),
                self.max.y.max(other.max.y),
                self.max.z.min(other.max.z),
            ),
        }
    }
    /// `true` when the horizontal extent is empty or non-finite.
    pub fn is_empty_xz(&self) -> bool {
        !(self.max.x > self.min.x && self.max.z > self.min.z)
            || !self.min.is_finite()
            || !self.max.is_finite()
    }
}

/// Queryable ground: height, normal and optional painted layer weights.
pub trait Surface: Send + Sync {
    /// Horizontal extent over which the surface is defined.
    fn bounds(&self) -> Aabb;
    /// Ground height at local `(x, z)`.
    fn height(&self, x: f32, z: f32) -> f32;
    /// Unit ground normal at local `(x, z)`.
    fn normal(&self, x: f32, z: f32) -> Vec3;
    /// Weight in `[0, 1]` of the named paint layer, or `None` when the layer is unknown.
    fn layer_weight(&self, _layer: &str, _x: f32, _z: f32) -> Option<f32> {
        None
    }
}

/// Unit normal of a height function by central differences.
pub fn normal_from_heights(height: &dyn Fn(f32, f32) -> f32, x: f32, z: f32) -> Vec3 {
    const EPS: f32 = 0.05;
    let dx = (height(x + EPS, z) - height(x - EPS, z)) / (2.0 * EPS);
    let dz = (height(x, z + EPS) - height(x, z - EPS)) / (2.0 * EPS);
    Vec3::new(-dx, 1.0, -dz).normalize()
}

/// Horizontal ground at constant height.
#[derive(Clone, Copy, Debug)]
pub struct FlatSurface {
    pub height: f32,
    pub bounds: Aabb,
}

impl FlatSurface {
    pub fn new(height: f32) -> Self {
        Self {
            height,
            bounds: Aabb::unbounded(),
        }
    }
}

impl Surface for FlatSurface {
    fn bounds(&self) -> Aabb {
        self.bounds
    }
    fn height(&self, _x: f32, _z: f32) -> f32 {
        self.height
    }
    fn normal(&self, _x: f32, _z: f32) -> Vec3 {
        Vec3::Y
    }
}

type Scalar = Box<dyn Fn(f32, f32) -> f32 + Send + Sync>;

/// Analytic surface defined by closures.
pub struct FnSurface {
    bounds: Aabb,
    height: Scalar,
    layers: BTreeMap<String, Scalar>,
}

impl FnSurface {
    pub fn new(bounds: Aabb, height: impl Fn(f32, f32) -> f32 + Send + Sync + 'static) -> Self {
        Self {
            bounds,
            height: Box::new(height),
            layers: BTreeMap::new(),
        }
    }
    /// Adds a paint layer whose weight is `weight(x, z)` clamped to `[0, 1]`.
    pub fn with_layer(
        mut self,
        name: &str,
        weight: impl Fn(f32, f32) -> f32 + Send + Sync + 'static,
    ) -> Self {
        self.layers.insert(name.to_owned(), Box::new(weight));
        self
    }
}

impl Surface for FnSurface {
    fn bounds(&self) -> Aabb {
        self.bounds
    }
    fn height(&self, x: f32, z: f32) -> f32 {
        (self.height)(x, z)
    }
    fn normal(&self, x: f32, z: f32) -> Vec3 {
        normal_from_heights(&*self.height, x, z)
    }
    fn layer_weight(&self, layer: &str, x: f32, z: f32) -> Option<f32> {
        self.layers.get(layer).map(|f| f(x, z).clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slope_normal_matches_analytic_plane() {
        // h = 0.5 x  => normal (-0.5, 1, 0) normalized.
        let surface = FnSurface::new(Aabb::unbounded(), |x, _| 0.5 * x);
        let n = surface.normal(3.0, 4.0);
        let expected = Vec3::new(-0.5, 1.0, 0.0).normalize();
        assert!((n - expected).length() < 1e-4, "{n:?}");
    }

    #[test]
    fn flat_surface_is_level_with_up_normal_and_no_layers() {
        let s = FlatSurface::new(2.0);
        assert_eq!(s.height(5.0, -3.0), 2.0);
        assert_eq!(s.normal(1.0, 1.0), Vec3::Y);
        assert_eq!(s.layer_weight("grass", 0.0, 0.0), None);
    }

    #[test]
    fn layers_clamp_and_unknown_layers_are_none() {
        let s = FnSurface::new(Aabb::unbounded(), |_, _| 0.0).with_layer("mud", |x, _| x);
        assert_eq!(s.layer_weight("mud", 2.0, 0.0), Some(1.0));
        assert_eq!(s.layer_weight("mud", -2.0, 0.0), Some(0.0));
        assert_eq!(s.layer_weight("sand", 0.0, 0.0), None);
    }

    #[test]
    fn intersection_clips_horizontal_extent() {
        let a = Aabb::new([0.0, 0.0, 0.0], [10.0, 5.0, 10.0]);
        let b = Aabb::new([5.0, -1.0, -5.0], [20.0, 9.0, 4.0]);
        let c = a.intersect_xz(&b);
        assert_eq!((c.min.x, c.max.x, c.min.z, c.max.z), (5.0, 10.0, 0.0, 4.0));
        assert!(!c.is_empty_xz());
        assert!(Aabb::new([0.0; 3], [1.0, 1.0, 0.0]).is_empty_xz());
    }
}
