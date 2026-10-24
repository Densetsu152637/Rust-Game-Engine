//! Acoustic materials and the cited preset table (section 5.1).
//!
//! # Sources
//!
//! - **Absorption** (125, 500, 2000 and 4000 Hz octave values): US Department of
//!   Defense, *UFC 3-450-01 Noise and Vibration Control* (8 December 2022), Table C-4
//!   "Sound Absorption Coefficients of General Building Materials and Furnishings".
//!   The 8 kHz band uses that document's rule in section C-3.2: "80% of the 4,000
//!   values for the 8,000 Hz octave band" when no measurement exists.
//! - **Steel**: Table C-4 has no metal row; the values are the "Steel decking" row
//!   of the akustik.ua *Absorption coefficients* compilation (2014), with the same
//!   8 kHz rule.
//! - **Scattering** (one value for all bands, the mid-frequency value): X. Zeng,
//!   C. L. Christensen and J. H. Rindel, "Practical methods to define scattering
//!   coefficients in a room acoustics computer model", *Applied Acoustics* 67 (2006)
//!   771-786, Table 5 (also in the ODEON user's manual): smooth surfaces 0.02-0.05,
//!   smooth painted concrete 0.005-0.02, brickwork with filled joints 0.05-0.1,
//!   brickwork with open joints 0.1-0.2.
//! - **Transmission**: the mass law `TL = 20 log10(f m) - 47.3 dB` (diffuse field,
//!   `m` in kg/m^2; M. Long, *Architectural Acoustics*, 2006, eq. 9.21) at the band
//!   centres for the stated surface densities, as `tau = 10^(-TL/10)`, then capped
//!   at the band absorption (transmitted energy is part of the non-reflected
//!   energy). The mass law ignores coincidence dips, so real partitions transmit
//!   somewhat more around their critical frequency. Floors and water transmit
//!   nothing audible (0).
//!
//! `Grass` has no preset yet: no published octave-band table for grass could be
//! verified, and no row ships without a citation, so `preset(Grass)` returns
//! `acoustics.unimplemented`.

use crate::{AcousticsError, BANDS, BandGains, Result, unimplemented};
use engine_audio::acoustic::BAND_CENTERS_HZ;

/// Energy coefficients in `[0, 1]` per band; `transmission[b] <= absorption[b]`.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AcousticMaterial {
    pub absorption: BandGains,
    pub scattering: f32,
    pub transmission: BandGains,
}
/// Preset table entries; each shipped row cites its published source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// One cited row: absorption at 125, 500, 2000 and 4000 Hz, the scattering
/// coefficient and the surface density used for the mass law (`None`: no
/// transmission).
struct Row {
    absorption_4k: [f32; 4],
    scattering: f32,
    surface_kg_m2: Option<f32>,
}

/// Table C-4 rows (UFC 3-450-01) unless noted.
fn row(preset: MaterialPreset) -> Option<Row> {
    use MaterialPreset::*;
    Some(match preset {
        // "Gypsum Board, 1/2-in. nailed to 2x4's 16-in. o.c." (a typical interior
        // wall); one 12.5 mm leaf of about 10 kg/m^2; smooth surface.
        Default => Row {
            absorption_4k: [0.29, 0.05, 0.07, 0.09],
            scattering: 0.05,
            surface_kg_m2: Some(10.0),
        },
        // "Floors, Concrete or terrazzo"; 150 mm slab at 2300 kg/m^3.
        Concrete => Row {
            absorption_4k: [0.01, 0.015, 0.02, 0.02],
            scattering: 0.02,
            surface_kg_m2: Some(345.0),
        },
        // "Brick, unglazed"; 110 mm leaf at 1800 kg/m^3; filled joints.
        Brick => Row {
            absorption_4k: [0.03, 0.03, 0.05, 0.07],
            scattering: 0.1,
            surface_kg_m2: Some(198.0),
        },
        // "Plaster, gypsum or lime, smooth finish on tile or brick"; the brick leaf
        // plus 13 mm plaster, about 210 kg/m^2.
        Plaster => Row {
            absorption_4k: [0.013, 0.02, 0.04, 0.05],
            scattering: 0.05,
            surface_kg_m2: Some(210.0),
        },
        // "Floors, Wood"; 19 mm boards at 600 kg/m^3.
        Wood => Row {
            absorption_4k: [0.15, 0.10, 0.06, 0.07],
            scattering: 0.05,
            surface_kg_m2: Some(11.4),
        },
        // "Glass, large panes of heavy plate glass"; 6 mm at 2500 kg/m^3.
        Glass => Row {
            absorption_4k: [0.18, 0.04, 0.02, 0.02],
            scattering: 0.02,
            surface_kg_m2: Some(15.0),
        },
        // akustik.ua "Steel decking"; 1 mm steel at 7850 kg/m^3.
        Metal => Row {
            absorption_4k: [0.13, 0.08, 0.11, 0.11],
            scattering: 0.05,
            surface_kg_m2: Some(7.85),
        },
        // "Carpet, heavy, on concrete"; laid on a floor, so no transmission.
        Carpet => Row {
            absorption_4k: [0.02, 0.14, 0.60, 0.65],
            scattering: 0.05,
            surface_kg_m2: None,
        },
        // "Fabrics, medium velour, 14 oz per sq. yard, draped to half area"; about
        // 0.48 kg/m^2, so the mass law caps transmission at the absorption. Folds
        // are shallow relief: 0.2, the top of the open-joint brickwork range.
        Curtain => Row {
            absorption_4k: [0.07, 0.49, 0.70, 0.60],
            scattering: 0.2,
            surface_kg_m2: Some(0.475),
        },
        Grass => return None,
        // "Water Surface, as in a swimming pool"; smooth; nothing transmitted back.
        Water => Row {
            absorption_4k: [0.008, 0.013, 0.020, 0.025],
            scattering: 0.02,
            surface_kg_m2: None,
        },
        // Definitional test materials.
        Absorber | Reflector => return None,
    })
}

/// Mass-law energy transmission coefficient at `frequency` for `surface_kg_m2`.
pub fn mass_law_transmission(frequency_hz: f32, surface_kg_m2: f32) -> f32 {
    let tl = 20.0 * (frequency_hz * surface_kg_m2).log10() - 47.3;
    10f32.powf(-tl / 10.0).min(1.0)
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

    /// The cited preset (see the module documentation for sources). Keeps the
    /// `Result` return of the Stream 0 skeleton: `Grass` has no verified source yet
    /// and returns `acoustics.unimplemented`.
    pub fn preset(preset: MaterialPreset) -> Result<Self> {
        match preset {
            MaterialPreset::Absorber => {
                return Ok(Self {
                    absorption: [1.0; BANDS],
                    scattering: 0.0,
                    transmission: [0.0; BANDS],
                });
            }
            MaterialPreset::Reflector => {
                return Ok(Self {
                    absorption: [0.0; BANDS],
                    scattering: 0.0,
                    transmission: [0.0; BANDS],
                });
            }
            _ => {}
        }
        let Some(r) = row(preset) else {
            return Err(unimplemented(
                "the grass preset (no verified published coefficients)",
            ));
        };
        let a = r.absorption_4k;
        // Bands 125, 500, 2000 Hz from the table, 8 kHz as 80% of 4 kHz.
        let absorption = [a[0], a[1], a[2], 0.8 * a[3]];
        let mut transmission = [0.0; BANDS];
        if let Some(m) = r.surface_kg_m2 {
            for b in 0..BANDS {
                transmission[b] = mass_law_transmission(BAND_CENTERS_HZ[b], m).min(absorption[b]);
            }
        }
        Ok(Self {
            absorption,
            scattering: r.scattering,
            transmission,
        })
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct MaterialId(pub u16);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_validate_and_follow_their_sources() {
        use MaterialPreset::*;
        for p in [
            Default, Concrete, Brick, Plaster, Wood, Glass, Metal, Carpet, Curtain, Water,
            Absorber, Reflector,
        ] {
            let m = AcousticMaterial::preset(p).unwrap();
            m.validate().unwrap();
        }
        assert_eq!(
            AcousticMaterial::preset(Grass).unwrap_err().code,
            "acoustics.unimplemented"
        );
        let brick = AcousticMaterial::preset(Brick).unwrap();
        assert_eq!(brick.absorption, [0.03, 0.03, 0.05, 0.8 * 0.07]);
        // 110 mm brick at 500 Hz: TL = 20 log10(500 * 198) - 47.3 = 52.6 dB.
        assert!((brick.transmission[1] - 10f32.powf(-5.26)).abs() < 1e-7);
        // The light curtain is capped by its absorption at low frequencies and
        // follows the mass law (about 12 dB at 2 kHz) above.
        let curtain = AcousticMaterial::preset(Curtain).unwrap();
        assert_eq!(curtain.transmission[..2], curtain.absorption[..2]);
        assert!((curtain.transmission[2] - mass_law_transmission(2000.0, 0.475)).abs() < 1e-7);
        assert_eq!(
            AcousticMaterial::preset(Absorber).unwrap().absorption,
            [1.0; 4]
        );
    }
}
