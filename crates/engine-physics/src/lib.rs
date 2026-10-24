//! Headless Rapier worlds. Coordinates are meters, seconds and kilograms.
//!
//! The crate is ECS-free. [`dim2`] and [`dim3`] are thin dimension modules that compile the
//! same dimension-generic sources (`world.rs` and `features/**`) against `rapier2d` and
//! `rapier3d`. Feature modules extend `World` with `impl World` blocks and keep their own
//! per-world side tables through the crate-internal `Extension` seam described in
//! `src/world.rs` and `docs/physics.md#extension-seams`.
// Dimension-generic sources are deliberately compiled once per dimension module.
#![allow(clippy::duplicate_mod)]
pub mod dim2;
pub mod dim3;
mod layers;
pub use dim2::World as World2D;
pub use dim3::World as World3D;
pub use layers::{CollisionLayers, MAX_LAYER_NAME, MAX_LAYERS};

/// Most points accepted by a convex hull shape.
pub const MAX_HULL_POINTS: usize = 4096;
/// Most vertices accepted by a triangle mesh shape.
pub const MAX_MESH_VERTICES: usize = 65_536;
/// Most triangles accepted by a triangle mesh shape.
pub const MAX_MESH_TRIANGLES: usize = 131_072;
/// Most height samples accepted by a heightfield (3D: rows x columns).
pub const MAX_HEIGHTFIELD_SAMPLES: usize = 1_048_576;
/// Most parts accepted by a compound shape.
pub const MAX_COMPOUND_PARTS: usize = 64;
/// Upper bound for `set_solver_iterations` in both dimensions.
pub const MAX_SOLVER_ITERATIONS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyKind {
    Fixed,
    Dynamic,
    Kinematic,
}
/// How two touching colliders combine friction or restitution coefficients.
///
/// When the two colliders disagree, Rapier applies the higher-priority rule:
/// `Max > Multiply > Min > Average`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum CombineRule {
    #[default]
    Average,
    Min,
    Max,
    Multiply,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub density: f32,
    pub friction: f32,
    pub restitution: f32,
    pub friction_combine: CombineRule,
    pub restitution_combine: CombineRule,
    pub sensor: bool,
    /// Raw membership bits. [`CollisionLayers::apply`] fills this from a named layer.
    pub layer: u32,
    /// Raw filter bits; a pair interacts only when each side's layer meets the other's mask.
    pub mask: u32,
}
impl Default for Material {
    fn default() -> Self {
        Self {
            density: 1.0,
            friction: 0.5,
            restitution: 0.0,
            friction_combine: CombineRule::Average,
            restitution_combine: CombineRule::Average,
            sensor: false,
            layer: 1,
            mask: u32::MAX,
        }
    }
}
impl Material {
    /// Density in 1e-6..=1e4, friction in 0..=1e4 and restitution in 0..=1, all finite.
    pub fn validate(self) -> Result<()> {
        if !self.density.is_finite()
            || !(0.000001..=10000.).contains(&self.density)
            || !self.friction.is_finite()
            || !(0.0..=10000.).contains(&self.friction)
            || !self.restitution.is_finite()
            || !(0.0..=1.0).contains(&self.restitution)
        {
            return Err(Error("invalid physics material"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollisionEvent {
    pub first: u64,
    pub second: u64,
    pub started: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub id: u64,
    pub distance: f32,
}
fn finite(values: &[f32]) -> bool {
    values.iter().all(|v| v.is_finite() && v.abs() <= 1e8)
}
fn dimension(value: f32) -> bool {
    value.is_finite() && (0.000001..=10000.).contains(&value)
}
fn valid_joint_limits(limits: Option<[f32; 2]>) -> bool {
    limits.is_none_or(|v| {
        finite(&v) && v[0] <= v[1] && v[0] >= -std::f32::consts::PI && v[1] <= std::f32::consts::PI
    })
}
fn valid_hull<const N: usize>(points: &[[f32; N]]) -> Result<()> {
    if points.len() <= N || points.len() > MAX_HULL_POINTS {
        return Err(Error("convex hull needs between dim+1 and 4096 points"));
    }
    if !points.iter().all(|p| finite(p)) {
        return Err(Error("invalid convex hull points"));
    }
    Ok(())
}
fn valid_mesh<const N: usize>(vertices: &[[f32; N]], indices: &[[u32; 3]]) -> Result<()> {
    if vertices.len() < 3
        || vertices.len() > MAX_MESH_VERTICES
        || indices.is_empty()
        || indices.len() > MAX_MESH_TRIANGLES
    {
        return Err(Error("triangle mesh vertex or triangle count out of range"));
    }
    if !vertices.iter().all(|v| finite(v)) {
        return Err(Error("invalid triangle mesh vertices"));
    }
    if indices
        .iter()
        .flatten()
        .any(|&i| i as usize >= vertices.len())
    {
        return Err(Error("triangle mesh index out of range"));
    }
    Ok(())
}
fn valid_heights(heights: &[f32], scale: &[f32]) -> Result<()> {
    if !finite(heights) || !scale.iter().all(|&s| dimension(s)) {
        return Err(Error("invalid heightfield heights or scale"));
    }
    Ok(())
}
