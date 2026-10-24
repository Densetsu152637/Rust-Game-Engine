use crate::{AcousticsError, BANDS, BandGains, Result, unimplemented};

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
    /// Every value finite and in `[0, 1]`, and `transmission[b] <= absorption[b]`
    /// (transmitted energy is part of the energy that is not reflected).
    pub fn validate(&self) -> Result<()> {
        let bad = |what: &str| {
            Err(AcousticsError::new(
                "acoustics.material.invalid",
                format!("material {what}"),
            ))
        };
        let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        if !self.absorption.iter().all(|&a| unit(a)) {
            return bad("absorption must be finite and in [0, 1]");
        }
        if !self.transmission.iter().all(|&t| unit(t)) {
            return bad("transmission must be finite and in [0, 1]");
        }
        if !unit(self.scattering) {
            return bad("scattering must be finite and in [0, 1]");
        }
        if (0..BANDS).any(|b| self.transmission[b] > self.absorption[b]) {
            return bad("transmission must not exceed absorption in any band");
        }
        Ok(())
    }
    /// Deviation from section 7.2: returns `Result` because preset values need
    /// cited sources and none are fabricated here.
    pub fn preset(_preset: MaterialPreset) -> Result<Self> {
        Err(unimplemented("AcousticMaterial::preset"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MaterialId(pub u16);
