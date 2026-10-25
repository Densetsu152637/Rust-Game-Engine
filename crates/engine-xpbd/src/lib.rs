//! Extended position-based dynamics (XPBD) for cloth and tetrahedral soft bodies.
//!
//! Units are meters, seconds and kilograms. The crate is ECS-free and depends only on
//! `glam`: a host owns one [`XpbdWorld`], inserts cloth and soft-body objects under stable
//! `u64` ids, hands it plain-data [`ColliderProxy`] values for the rigid bodies the
//! particles collide with, steps it with fixed seconds and reads positions, normals,
//! surface triangles and per-proxy reaction impulses back.
//!
//! The solver follows "small steps" XPBD (Macklin et al. 2019): every step is split into a
//! fixed number of substeps, each predicting positions, projecting compliant constraints
//! once (or a fixed number of iterations), resolving proxy contacts with Coulomb friction
//! and deriving velocities from the position change. Iteration order is the stable
//! constraint order built at creation and objects are stepped in ascending id order, so
//! identical inputs give bitwise-identical results on one machine and build.
mod cloth;
mod mesh;
mod particles;
mod proxy;
mod softbody;
mod world;

pub use cloth::{Cloth, ClothDesc, ClothParams};
pub use mesh::{box_tet_mesh, grid_mesh, surface_triangles};
pub use proxy::{ColliderProxy, ProxyImpulse, ProxyShape};
pub use softbody::{SoftBody, SoftBodyDesc, SoftBodyParams};
pub use world::{ObjectKind, XpbdSettings, XpbdWorld};

/// Most particles in one cloth or soft body.
pub const MAX_OBJECT_PARTICLES: usize = 65_536;
/// Most triangles in one cloth.
pub const MAX_OBJECT_TRIANGLES: usize = 131_072;
/// Most tetrahedra in one soft body.
pub const MAX_OBJECT_TETS: usize = 262_144;
/// Upper bound for the particle budget passed to [`XpbdWorld::new`].
pub const MAX_WORLD_PARTICLES: usize = 1_048_576;
/// Most objects (cloths plus soft bodies) in one world.
pub const MAX_OBJECTS: usize = 4096;
/// Most collider proxies accepted by one [`XpbdWorld::step`].
pub const MAX_PROXIES: usize = 1024;
/// Upper bound for [`XpbdSettings::substeps`].
pub const MAX_SUBSTEPS: u32 = 64;
/// Upper bound for [`XpbdSettings::iterations`].
pub const MAX_ITERATIONS: u32 = 16;
/// Longest accepted step in seconds.
pub const MAX_STEP: f32 = 0.25;
/// Largest accepted absolute coordinate in meters.
pub const MAX_COORDINATE: f32 = 1.0e5;
/// Largest accepted speed (wind, proxy velocity) and the particle speed clamp, in m/s.
pub const MAX_SPEED: f32 = 1.0e4;
/// Largest accepted compliance value (any unit).
pub const MAX_COMPLIANCE: f32 = 1.0e6;
/// Largest accepted velocity damping rate in 1/s.
pub const MAX_DAMPING: f32 = 1.0e3;
/// Largest accepted friction coefficient.
pub const MAX_FRICTION: f32 = 10.0;
/// Largest accepted shape extent, thickness or particle radius in meters.
pub const MAX_EXTENT: f32 = 1.0e4;
/// Smallest accepted rest triangle area in square meters.
pub const MIN_TRIANGLE_AREA: f32 = 1.0e-10;
/// Smallest accepted rest tetrahedron volume in cubic meters.
pub const MIN_TET_VOLUME: f32 = 1.0e-12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn finite_bounded(v: glam::Vec3, bound: f32) -> bool {
    v.is_finite() && v.abs().max_element() <= bound
}

pub(crate) fn in_range(value: f32, min: f32, max: f32) -> bool {
    value.is_finite() && value >= min && value <= max
}
