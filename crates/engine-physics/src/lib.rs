//! Headless Rapier worlds. Coordinates are meters, seconds and kilograms.
pub mod dim2;
pub mod dim3;
mod world;
pub use dim2::World as World2D;
pub use dim3::World as World3D;

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
#[derive(Clone, Copy, Debug)]
pub struct Material {
    pub density: f32,
    pub friction: f32,
    pub restitution: f32,
    pub sensor: bool,
    pub layer: u32,
    pub mask: u32,
}
impl Default for Material {
    fn default() -> Self {
        Self {
            density: 1.0,
            friction: 0.5,
            restitution: 0.0,
            sensor: false,
            layer: 1,
            mask: u32::MAX,
        }
    }
}
impl Material {
    fn validate(self) -> Result<()> {
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
