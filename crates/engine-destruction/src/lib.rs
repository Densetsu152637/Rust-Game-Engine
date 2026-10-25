//! Deterministic Voronoi pre-fracture of closed triangle meshes and convex polygons.
//!
//! The crate depends on `glam` and `std` only. Given a closed mesh and a seed pattern it
//! computes one Voronoi cell per seed by half-space clipping the mesh, closes every cut with
//! cap faces, and reports per fragment a convex hull (collider), an exact triangle mesh whose
//! cap triangles are flagged interior, volume, centroid and inertia. See
//! `docs/physics-destruction.md` for the algorithm, robustness notes and limits.
mod convex;
mod decompose;
mod fracture;
mod fracture2d;
mod hull;
mod mass;
mod mesh;
mod rng;
mod seeds;
mod solid;
mod triangulate;

pub use fracture::{
    Fracture, Fragment, FragmentMesh, fracture, fracture_with_seeds, generate_seeds,
};
pub use fracture2d::{Fracture2, Fragment2, Pattern2, fracture_polygon};
pub use mesh::{Mesh, Topology, topology};
pub(crate) use seeds::IMPACT_CLUSTER_FRACTION;

use glam::Vec3;

/// Hard caps that keep a fracture bounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Most seeds (and so most fragments per seed cell) accepted. Default 256.
    pub max_fragments: usize,
    /// Most vertices in the input mesh and in the combined fragment meshes. Default 65536.
    pub max_vertices: usize,
    /// Most triangles in the input mesh. Default 20000.
    pub max_input_triangles: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_fragments: 256,
            max_vertices: 65_536,
            max_input_triangles: 20_000,
        }
    }
}
/// How seed points are distributed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pattern {
    /// `count` seeds uniformly inside the solid.
    Uniform { count: usize, seed: u64 },
    /// 75% of the seeds fall within `radius` of `point` (density rising toward it), the rest
    /// are uniform, so fragments are small near the impact and large far from it.
    Impact {
        count: usize,
        seed: u64,
        point: Vec3,
        radius: f32,
    },
}
impl Pattern {
    pub fn count(&self) -> usize {
        match *self {
            Self::Uniform { count, .. } | Self::Impact { count, .. } => count,
        }
    }
    pub fn seed(&self) -> u64 {
        match *self {
            Self::Uniform { seed, .. } | Self::Impact { seed, .. } => seed,
        }
    }
    pub fn validate(&self, limits: &Limits) -> Result<()> {
        if !(1..=limits.max_fragments).contains(&self.count()) {
            return Err(Error::InvalidPattern("fragment count out of range"));
        }
        if let Self::Impact { point, radius, .. } = *self
            && (!point.is_finite() || !radius.is_finite() || radius <= 0.)
        {
            return Err(Error::InvalidPattern(
                "impact point must be finite and radius positive",
            ));
        }
        Ok(())
    }
}
/// Why a fracture request was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidMesh(&'static str),
    InvalidPattern(&'static str),
    LimitExceeded(&'static str),
    Degenerate(&'static str),
    /// The clipping hit a configuration it cannot resolve robustly (reported, never papered
    /// over with wrong geometry).
    NonRobust(&'static str),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (kind, message) = match self {
            Self::InvalidMesh(m) => ("invalid mesh", m),
            Self::InvalidPattern(m) => ("invalid pattern", m),
            Self::LimitExceeded(m) => ("limit exceeded", m),
            Self::Degenerate(m) => ("degenerate input", m),
            Self::NonRobust(m) => ("non-robust geometry", m),
        };
        write!(f, "{kind}: {message}")
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
