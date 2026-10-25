//! Pure-Rust CPU mesh LOD toolkit.
//!
//! * [`simplify`]: quadric edge-collapse simplification with attribute costs.
//! * [`build_lod_chain`], [`select_lod`], [`ScreenSizeLods`]: LOD chains and selection.
//! * [`build_hlod`]: spatial clustering and proxy meshes for static instances.
//! * [`Frustum`], [`cull_spheres`], [`cull_aabbs`]: culling math (WebGPU depth 0..1).
//!
//! No GPU code; see `docs/rendering-lod.md` for algorithms, guarantees and limits.

pub mod bounds;
pub mod cull;
pub mod distance;
pub mod error;
pub mod hlod;
pub mod lod;
pub mod mesh;
pub mod simplify;

pub use bounds::{Aabb, BoundingSphere};
pub use cull::{Containment, Frustum, cull_aabbs, cull_spheres};
pub use distance::{distance_to_surface, mesh_error};
pub use error::{MeshLodError, Result};
pub use hlod::{HlodCluster, HlodInstance, HlodOptions, build_hlod};
pub use lod::{
    LodChain, LodLevel, LodSelectionParams, LodTarget, ScreenSizeLods, build_lod_chain,
    level_pixel_error, projected_error_pixels, screen_height_fraction, select_lod,
};
pub use mesh::MeshData;
pub use simplify::{SimplifyOptions, SimplifyResult, simplify};
