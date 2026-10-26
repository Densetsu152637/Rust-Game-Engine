//! Foliage and vegetation data model (pure Rust: glam, serde and `engine-pcg`; no GPU, ECS
//! or file-system access).
//!
//! * [`FoliageType`]: mesh, LOD meshes, impostor, density per square metre and density map,
//!   scale / yaw / tilt ranges, slope / height / altitude limits, align-to-normal, minimum
//!   spacing, per-type exclusion and LOD distances.
//! * [`FoliageField`]: instances in spatial cells; bounded ([`MAX_INSTANCES`]), revisioned
//!   per cell and serializable.
//! * [`scatter`]: deterministic placement over an `engine_pcg::Surface` with density maps
//!   (a terrain paint layer, an explicit grid), exclusion zones and minimum-distance thinning.
//! * [`paint`]: add / erase instances in a brush with deterministic seeds.
//! * [`LodBands`] / [`LodTracker`]: distance bands with hysteresis, impostor beyond the last
//!   mesh band, culling.
//! * [`WindParams`]: the wind model and its CPU sway evaluation (the GPU shader contract).
//! * [`asset`]: the `.foliage` file.
//!
//! See docs/world-foliage.md for how `engine-project` and `engine-world-render` use it.
pub mod asset;
mod error;
pub mod field;
pub mod lod;
pub mod paint;
pub mod scatter;
pub mod types;
pub mod wind;

pub use asset::FoliageAsset;
pub use error::{Error, Result};
pub use field::{
    Cell, CellKey, FoliageField, Instance, MAX_CELL_SIZE, MAX_INSTANCES, MIN_CELL_SIZE,
};
pub use lod::{LodBands, LodLevel, LodTracker, UNKNOWN_CODE};
pub use paint::{Brush, PaintContext, PaintOp, PaintReport, paint};
pub use scatter::{ExclusionZone, HoleMask, ScatterParams, ScatterResult, TypeReport, scatter};
pub use types::{
    DensityGrid, DensityMap, FoliageType, Impostor, MAX_DENSITY, MAX_LOD_MESHES, MAX_TYPES,
    TypeExclusion, validate_types,
};
pub use wind::{WindBlock, WindParams, bend_factor};
