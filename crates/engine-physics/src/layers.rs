//! Named collision layers with a symmetric collision matrix.
use crate::{Error, Material, Result};

/// Rapier interaction groups are 32-bit, so at most 32 named layers exist.
pub const MAX_LAYERS: usize = 32;
/// Longest accepted layer name in bytes.
pub const MAX_LAYER_NAME: usize = 64;

/// Named layer `i` owns membership bit `i`; its filter is row `i` of a symmetric matrix.
///
/// Rapier's `And` interaction test requires each side's membership to meet the other's
/// filter, so a symmetric matrix makes "layer A collides with layer B" exact. New layers
/// collide with every layer until [`CollisionLayers::set_collides`] disables a pair.
/// Raw `Material::layer`/`mask` values remain valid: a raw body whose bits overlap named
/// bit `i` behaves like a member of layer `i`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollisionLayers {
    names: Vec<String>,
    rows: [u32; MAX_LAYERS],
}
impl CollisionLayers {
    /// Names must be unique, non-empty, at most 64 bytes of `[A-Za-z0-9_.-]`.
    pub fn new<I, S>(names: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut result = Self {
            names: Vec::new(),
            rows: [0; MAX_LAYERS],
        };
        for name in names {
            let name = name.into();
            if result.names.len() == MAX_LAYERS {
                return Err(Error("at most 32 collision layers"));
            }
            if name.is_empty()
                || name.len() > MAX_LAYER_NAME
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                return Err(Error("invalid collision layer name"));
            }
            if result.names.contains(&name) {
                return Err(Error("duplicate collision layer name"));
            }
            result.names.push(name);
        }
        let all = match result.names.len() {
            MAX_LAYERS => u32::MAX,
            n => (1u32 << n) - 1,
        };
        for row in &mut result.rows[..result.names.len()] {
            *row = all;
        }
        Ok(result)
    }
    pub fn names(&self) -> &[String] {
        &self.names
    }
    pub fn index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }
    fn require(&self, name: &str) -> Result<usize> {
        self.index(name).ok_or(Error("unknown collision layer"))
    }
    /// Enables or disables both directions of the pair; `a == b` toggles self-collision.
    pub fn set_collides(&mut self, a: &str, b: &str, enabled: bool) -> Result<()> {
        let (a, b) = (self.require(a)?, self.require(b)?);
        if enabled {
            self.rows[a] |= 1 << b;
            self.rows[b] |= 1 << a;
        } else {
            self.rows[a] &= !(1 << b);
            self.rows[b] &= !(1 << a);
        }
        Ok(())
    }
    pub fn collides(&self, a: &str, b: &str) -> Result<bool> {
        let (a, b) = (self.require(a)?, self.require(b)?);
        Ok(self.rows[a] & (1 << b) != 0)
    }
    /// Membership bit and filter row for the named layer.
    pub fn groups(&self, name: &str) -> Result<(u32, u32)> {
        let index = self.require(name)?;
        Ok((1 << index, self.rows[index]))
    }
    /// Returns `material` with its raw layer and mask replaced by the named layer's groups.
    pub fn apply(&self, material: Material, name: &str) -> Result<Material> {
        let (layer, mask) = self.groups(name)?;
        Ok(Material {
            layer,
            mask,
            ..material
        })
    }
    /// Query mask (for raycasts) selecting every named layer in `names`.
    pub fn query_mask(&self, names: &[&str]) -> Result<u32> {
        names
            .iter()
            .try_fold(0, |mask, name| Ok(mask | 1 << self.require(name)?))
    }
}
