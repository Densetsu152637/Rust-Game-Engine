use crate::{BandGains, Result, unimplemented};

/// Energy coefficients in `[0, 1]` per band; `transmission[b] <= absorption[b]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AcousticMaterial {
    pub absorption: BandGains,
    pub scattering: f32,
    pub transmission: BandGains,
}
/// Preset table entries; each shipped row must cite its published source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialPreset {
    Default,
    Concrete,
    Brick,
    Plaster,
    Wood,
    Glass,
    Metal,
    Carpet,
    Curtain,
    Grass,
    Water,
    Absorber,
    Reflector,
}
impl AcousticMaterial {
    pub fn validate(&self) -> Result<()> {
        Err(unimplemented("AcousticMaterial::validate"))
    }
    /// Deviation from section 7.2: returns `Result` because preset values need
    /// cited sources and none are fabricated here.
    pub fn preset(_preset: MaterialPreset) -> Result<Self> {
        Err(unimplemented("AcousticMaterial::preset"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MaterialId(pub u16);
