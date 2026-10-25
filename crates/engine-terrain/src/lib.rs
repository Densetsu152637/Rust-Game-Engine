//! Heightmap terrain data model (pure Rust, no GPU, ECS or file-system access).
//!
//! * [`Heightfield`]: bounded `f32` sample grid with spacing, triangulated and bilinear height
//!   sampling, normals, bounds, a per-cell holes mask, a splat map of up to eight normalized
//!   layer weights ([`Heightfield::layer_weight_at`]) and [`TerrainLayer`] definitions
//!   (texture id, tint, UV tiling).
//! * [`lod`]: CDLOD-style quadtree selection with per-level ranges, a node budget and
//!   per-vertex geomorphing that is crack-free across level boundaries.
//! * [`mesh`]: CPU chunk meshes of selected nodes (positions, normals, UVs, splat colours,
//!   hole triangles removed).
//! * [`brush`]: deterministic raise/lower, smooth, flatten, set-height, noise, splat and hole
//!   brushes returning the dirty region.
//! * [`asset`]: the versioned, validated, bounded binary `.terrain` file.
//! * [`collision`]: physics tiles (heightfields, or triangle meshes where holes are).
//! * [`navigation`]: the walkable, raster-aligned triangle soup for navmesh baking.
//!
//! See docs/terrain.md for the formats and how `engine-project` renders, collides and
//! edits terrain.
pub mod asset;
pub mod brush;
pub mod collision;
mod error;
pub mod heightfield;
pub mod lod;
pub mod mesh;
pub mod navigation;
pub mod noise;

pub use brush::{Brush, EditReport, SculptOp, paint_holes, paint_layer, sculpt};
pub use error::{Error, Result};
pub use heightfield::{
    Heightfield, MAX_ABS_HEIGHT, MAX_LAYERS, MAX_SAMPLES, MAX_SIDE, MAX_SPACING, SampleRect,
    TerrainLayer, WEIGHT_SUM, triangulated,
};
pub use lod::{LodLayout, LodSettings, LodViewer, NodeKey, SelectedNode, Selection};
pub use mesh::{ChunkMesh, build_chunk_mesh};
pub use navigation::{NavInputOptions, NavSoup, navigation_soup};
