//! HDR display output: the CPU reference of the output transform that
//! replaces the SDR tonemap/encode at the end of the chain when the surface
//! really is an HDR colour space. Mirrored by `wgsl/hdr.wgsl`
//! (`engine::hdr`); the GPU tests compare against these functions. See
//! docs/rendering-hdr-output.md.
//!
//! Input convention: *display-linear Rec.709 relative to paper white*. A
//! value of `1.0` is the diffuse/UI white the viewer perceives as "white",
//! shown at [`HdrOutputSettings::paper_white_nits`]; `headroom` times that is
//! the brightest value the display is asked to show.

use crate::post::tonemap::rec709_to_rec2020;
use anyhow::{Result, ensure};

/// Luminance of `1.0` in scRGB (IEC 61966-2-2, DXGI
/// `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709`): 80 nits.
pub const SCRGB_UNIT_NITS: f32 = 80.0;
/// Luminance of PQ signal `1.0`.
pub const PQ_MAX_NITS: f32 = 10_000.0;
/// ST 2084 constants, exactly representable in `f32` (the WGSL declares the
/// same decimal values: 0.1593017578125, 78.84375, 0.8359375, 18.8515625,
/// 18.6875).
pub const PQ_M1: f32 = 2610.0 / 16384.0;
pub const PQ_M2: f32 = 2523.0 / 4096.0 * 128.0;
pub const PQ_C1: f32 = 3424.0 / 4096.0;
pub const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
pub const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;
/// Largest finite half float: inputs are clamped to it so an infinity from
/// the `Rgba16Float` scene cannot produce NaN.
pub const MAX_INPUT: f32 = 65_504.0;

/// ITU-R BT.2408 reference white for HDR graphics/UI: 203 nits.
pub const DEFAULT_PAPER_WHITE_NITS: f32 = 203.0;
/// Default peak when the display reports nothing better.
pub const DEFAULT_PEAK_NITS: f32 = 1000.0;
pub const MIN_PAPER_WHITE_NITS: f32 = 20.0;
pub const MAX_PAPER_WHITE_NITS: f32 = 1000.0;
pub const MAX_PEAK_NITS: f32 = PQ_MAX_NITS;

/// The signal the HDR surface expects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HdrEncoding {
    /// Linear Rec.709 primaries, `1.0` = 80 nits, in an `Rgba16Float`
    /// surface (extended linear sRGB / scRGB).
    #[default]
    ScRgb,
    /// Rec.2020 primaries, SMPTE ST 2084 (PQ) encoded, in an `Rgb10a2Unorm`
    /// surface (HDR10 / BT.2100 PQ).
    Pq,
}

impl HdrEncoding {
    /// The shader's `encoding` code.
    pub fn code(self) -> f32 {
        match self {
            Self::ScRgb => 0.0,
            Self::Pq => 1.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ScRgb => "scRGB",
            Self::Pq => "PQ",
        }
    }
}

/// Output transform parameters: encoding, paper white and display peak.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrOutputSettings {
    pub encoding: HdrEncoding,
    /// Luminance of display-linear `1.0` (UI and diffuse white), in nits.
    pub paper_white_nits: f32,
    /// Brightest luminance the roll-off lets through, in nits.
    pub peak_nits: f32,
}

impl Default for HdrOutputSettings {
    fn default() -> Self {
        Self {
            encoding: HdrEncoding::ScRgb,
            paper_white_nits: DEFAULT_PAPER_WHITE_NITS,
            peak_nits: DEFAULT_PEAK_NITS,
        }
    }
}

impl HdrOutputSettings {
    /// Finite, paper white within
    /// [`MIN_PAPER_WHITE_NITS`]..=[`MAX_PAPER_WHITE_NITS`], peak at least the
    /// paper white and at most [`MAX_PEAK_NITS`].
    pub fn validate(&self) -> Result<()> {
        validate_nits(self.paper_white_nits, self.peak_nits)
    }

    /// Peak over paper white: the largest paper-white-relative value shown.
    pub fn headroom(&self) -> f32 {
        self.peak_nits / self.paper_white_nits
    }

    /// The fragment uniform: paper white, peak, encoding code, unused.
    pub fn uniform(&self) -> [f32; 4] {
        [
            self.paper_white_nits,
            self.peak_nits,
            self.encoding.code(),
            0.0,
        ]
    }

    /// Encode a display-linear Rec.709 colour (1.0 = paper white) into the
    /// surface's signal: roll off to the peak, then scale to scRGB or convert
    /// to Rec.2020 nits and apply the PQ OETF.
    pub fn encode(&self, rgb: [f32; 3]) -> [f32; 3] {
        let clean = rgb.map(|v| {
            if v.is_nan() {
                0.0
            } else {
                v.clamp(0.0, MAX_INPUT)
            }
        });
        let rolled = rolloff(clean, self.headroom());
        match self.encoding {
            HdrEncoding::ScRgb => rolled.map(|v| scrgb_from_nits(v * self.paper_white_nits)),
            HdrEncoding::Pq => {
                rec709_to_rec2020(rolled).map(|v| pq_oetf(v * self.paper_white_nits))
            }
        }
    }
}

/// The `engine::hdr` WGSL module: `wgsl/hdr.wgsl` with its `{{NAME}}`
/// placeholders filled from the constants and matrix above, so the shader and
/// the CPU reference cannot disagree on them.
pub fn wgsl_source() -> String {
    let mut source = include_str!("wgsl/hdr.wgsl").replace(
        "
", "
",
    );
    let mut fill = |name: &str, value: f32| {
        source = source.replace(&format!("{{{{{name}}}}}"), &format!("{value:?}"));
    };
    fill("SCRGB_UNIT_NITS", SCRGB_UNIT_NITS);
    fill("PQ_MAX_NITS", PQ_MAX_NITS);
    fill("PQ_M1", PQ_M1);
    fill("PQ_M2", PQ_M2);
    fill("PQ_C1", PQ_C1);
    fill("PQ_C2", PQ_C2);
    fill("PQ_C3", PQ_C3);
    fill("MAX_INPUT", MAX_INPUT);
    let matrix = crate::post::tonemap::SRGB_TO_REC2020;
    for (row, prefix) in matrix.iter().zip(["R", "G", "B"]) {
        for (column, value) in row.iter().enumerate() {
            fill(&format!("REC2020_{prefix}{column}"), *value);
        }
    }
    source
}

/// Shared validation of a paper-white / peak pair.
pub fn validate_nits(paper_white_nits: f32, peak_nits: f32) -> Result<()> {
    ensure!(
        paper_white_nits.is_finite() && peak_nits.is_finite(),
        "HDR paper white and peak must be finite"
    );
    ensure!(
        (MIN_PAPER_WHITE_NITS..=MAX_PAPER_WHITE_NITS).contains(&paper_white_nits),
        "HDR paper white {paper_white_nits} nits is outside \
         {MIN_PAPER_WHITE_NITS}..={MAX_PAPER_WHITE_NITS}"
    );
    ensure!(
        peak_nits <= MAX_PEAK_NITS,
        "HDR peak {peak_nits} nits exceeds {MAX_PEAK_NITS}"
    );
    ensure!(
        peak_nits >= paper_white_nits,
        "HDR peak {peak_nits} nits is below the paper white {paper_white_nits} nits"
    );
    Ok(())
}

/// scRGB value of a luminance in nits (Rec.709 primaries, grey).
pub fn scrgb_from_nits(nits: f32) -> f32 {
    nits / SCRGB_UNIT_NITS
}

/// Inverse of [`scrgb_from_nits`].
pub fn nits_from_scrgb(value: f32) -> f32 {
    value * SCRGB_UNIT_NITS
}

/// SMPTE ST 2084 inverse EOTF: absolute luminance in nits to the 0..1 PQ
/// signal.
pub fn pq_oetf(nits: f32) -> f32 {
    let y = (nits / PQ_MAX_NITS).clamp(0.0, 1.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)).powf(PQ_M2)
}

/// SMPTE ST 2084 EOTF: the 0..1 PQ signal to nits.
pub fn pq_eotf(signal: f32) -> f32 {
    let e = signal.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    let numerator = (e - PQ_C1).max(0.0);
    let denominator = PQ_C2 - PQ_C3 * e;
    PQ_MAX_NITS * (numerator / denominator).powf(1.0 / PQ_M1)
}

/// Where the shoulder starts for `headroom`: paper white (1.0) is untouched
/// whenever the display has at least 2x headroom.
pub fn rolloff_start(headroom: f32) -> f32 {
    (0.5 * headroom).min(1.0)
}

/// Roll-off of a paper-white-relative value (hue-preserving callers scale by
/// `rolloff(max) / max`). Identity up to [`rolloff_start`], then an
/// exponential shoulder with slope 1 at the knee that approaches `headroom`
/// and never exceeds it. Monotonic.
pub fn rolloff_value(value: f32, headroom: f32) -> f32 {
    let start = rolloff_start(headroom);
    if value <= start {
        return value;
    }
    let range = (headroom - start).max(1e-6);
    start + range * (1.0 - (-(value - start) / range).exp())
}

/// [`rolloff_value`] applied to the brightest channel, scaling the colour so
/// hue and saturation survive. Every output channel is at most `headroom`.
pub fn rolloff(rgb: [f32; 3], headroom: f32) -> [f32; 3] {
    let peak = rgb[0].max(rgb[1]).max(rgb[2]);
    if peak <= rolloff_start(headroom) {
        return rgb;
    }
    let scale = rolloff_value(peak, headroom) / peak;
    rgb.map(|v| v * scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shader::HDR_OUTPUT_WGSL;

    fn close(a: f32, b: f32, tolerance: f32) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn pq_matches_known_code_values() {
        // Narrow-range-free (full 0..1) ST 2084 signal values.
        for (nits, want) in [
            (100.0, 0.508_078),
            (203.0, 0.580_689),
            (1000.0, 0.751_827),
            (4000.0, 0.902_572),
            (10_000.0, 1.0),
        ] {
            let got = pq_oetf(nits);
            assert!(close(got, want, 5e-5), "pq({nits}) = {got}, want {want}");
        }
        assert!(pq_oetf(0.0) < 1e-5, "black is code zero");
        assert!(close(pq_oetf(1e9), 1.0, 1e-6), "above 10000 nits clamps");
    }

    #[test]
    fn pq_round_trips() {
        for nits in [0.1, 1.0, 10.0, 80.0, 203.0, 1000.0, 5000.0, 10_000.0] {
            let back = pq_eotf(pq_oetf(nits));
            assert!(
                close(back, nits, nits * 2e-3 + 1e-3),
                "{nits} -> {back} nits"
            );
        }
        assert!(close(pq_eotf(1.0), 10_000.0, 1.0));
        assert_eq!(pq_eotf(0.0), 0.0);
        let mut previous = -1.0;
        for step in 0..=100 {
            let nits = pq_eotf(step as f32 / 100.0);
            assert!(nits >= previous, "EOTF is monotonic");
            previous = nits;
        }
    }

    #[test]
    fn scrgb_scales_by_eighty_nits() {
        assert_eq!(scrgb_from_nits(80.0), 1.0);
        assert_eq!(scrgb_from_nits(203.0), 203.0 / 80.0);
        assert_eq!(nits_from_scrgb(scrgb_from_nits(1000.0)), 1000.0);
        assert_eq!(scrgb_from_nits(0.0), 0.0);
    }

    #[test]
    fn paper_white_lands_on_the_configured_nits() {
        for paper in [100.0, 203.0, 300.0] {
            let scrgb = HdrOutputSettings {
                encoding: HdrEncoding::ScRgb,
                paper_white_nits: paper,
                peak_nits: 1000.0,
            };
            let white = scrgb.encode([1.0; 3]);
            for c in white {
                assert!(close(nits_from_scrgb(c), paper, 1e-3), "{white:?}");
            }
            let pq = HdrOutputSettings {
                encoding: HdrEncoding::Pq,
                ..scrgb
            };
            let signal = pq.encode([1.0; 3]);
            for c in signal {
                // Rec.709 white stays white in Rec.2020 (rows sum to ~1).
                assert!(close(pq_eotf(c), paper, paper * 2e-3), "{signal:?}");
            }
        }
    }

    #[test]
    fn rec709_primaries_convert_into_rec2020() {
        let settings = HdrOutputSettings {
            encoding: HdrEncoding::Pq,
            ..Default::default()
        };
        let red = settings.encode([1.0, 0.0, 0.0]);
        // Rec.709 red is inside Rec.2020: red dominates, green and blue
        // carry the matrix leakage 0.0691 and 0.0164 of the red.
        let nits = red.map(|c| pq_eotf(c) / DEFAULT_PAPER_WHITE_NITS);
        assert!(close(nits[0], 0.6274, 5e-3), "{nits:?}");
        assert!(close(nits[1], 0.0691, 2e-3), "{nits:?}");
        assert!(close(nits[2], 0.0164, 1e-3), "{nits:?}");
        let scrgb = HdrOutputSettings::default().encode([1.0, 0.0, 0.0]);
        assert_eq!(scrgb[1], 0.0, "scRGB keeps Rec.709 primaries untouched");
    }

    #[test]
    fn rolloff_is_identity_below_the_knee_and_capped_at_the_peak() {
        for headroom in [1.0f32, 2.0, 4.9, 12.5] {
            let start = rolloff_start(headroom);
            assert_eq!(rolloff_value(start * 0.5, headroom), start * 0.5);
            assert!(close(rolloff_value(start, headroom), start, 1e-6));
            let mut previous = 0.0;
            for step in 0..400 {
                let x = step as f32 * 0.25;
                let y = rolloff_value(x, headroom);
                assert!(y >= previous - 1e-6, "monotonic at {x}");
                assert!(y <= headroom + 1e-5, "capped: {y} > {headroom}");
                previous = y;
            }
            assert!(
                close(rolloff_value(1.0e4, headroom), headroom, 1e-3),
                "approaches the peak"
            );
        }
        // Paper white is untouched whenever the display has 2x headroom.
        assert_eq!(rolloff_value(1.0, 4.9), 1.0);
    }

    #[test]
    fn rolloff_preserves_hue_and_caps_every_channel() {
        let out = rolloff([20.0, 10.0, 5.0], 4.9);
        assert!(out.iter().all(|&c| c <= 4.9));
        assert!(close(out[0] / out[1], 2.0, 1e-5) && close(out[1] / out[2], 2.0, 1e-5));
        assert_eq!(rolloff([0.5, 0.25, 0.0], 4.9), [0.5, 0.25, 0.0]);
    }

    #[test]
    fn encode_never_exceeds_the_peak_and_is_finite() {
        for encoding in [HdrEncoding::ScRgb, HdrEncoding::Pq] {
            let settings = HdrOutputSettings {
                encoding,
                paper_white_nits: 203.0,
                peak_nits: 1000.0,
            };
            for value in [
                [f32::INFINITY, 0.0, 0.0],
                [f32::NAN, -1.0, 1e9],
                [100.0, 100.0, 100.0],
            ] {
                let out = settings.encode(value);
                assert!(out.iter().all(|c| c.is_finite()), "{out:?}");
                let nits = match encoding {
                    HdrEncoding::ScRgb => out.map(nits_from_scrgb),
                    HdrEncoding::Pq => out.map(pq_eotf),
                };
                assert!(nits.iter().all(|&n| n <= 1000.0 * 1.01), "{nits:?}");
            }
        }
    }

    #[test]
    fn validation_rejects_bad_settings() {
        assert!(HdrOutputSettings::default().validate().is_ok());
        let with = |paper: f32, peak: f32| HdrOutputSettings {
            paper_white_nits: paper,
            peak_nits: peak,
            ..Default::default()
        };
        assert!(with(f32::NAN, 1000.0).validate().is_err());
        assert!(with(203.0, f32::INFINITY).validate().is_err());
        assert!(with(1000.0, 203.0).validate().is_err(), "paper above peak");
        assert!(with(5.0, 1000.0).validate().is_err(), "paper too low");
        assert!(with(2000.0, 4000.0).validate().is_err(), "paper too high");
        assert!(with(203.0, 20_000.0).validate().is_err(), "peak too high");
        assert!(with(203.0, 203.0).validate().is_ok(), "peak = paper white");
    }

    /// The WGSL module is generated from the Rust constants (one source).
    #[test]
    fn the_wgsl_module_is_generated_from_the_rust_constants() {
        let wgsl = wgsl_source();
        assert!(!wgsl.contains("{{"), "unfilled placeholder in hdr.wgsl");
        for (name, value) in [
            ("ENGINE_SCRGB_UNIT_NITS", SCRGB_UNIT_NITS),
            ("ENGINE_PQ_MAX_NITS", PQ_MAX_NITS),
            ("ENGINE_PQ_M1", PQ_M1),
            ("ENGINE_PQ_M2", PQ_M2),
            ("ENGINE_PQ_C1", PQ_C1),
            ("ENGINE_PQ_C2", PQ_C2),
            ("ENGINE_PQ_C3", PQ_C3),
            ("ENGINE_MAX_INPUT", MAX_INPUT),
        ] {
            let line = wgsl
                .lines()
                .find(|l| l.trim_start().starts_with(&format!("const {name}:")))
                .unwrap_or_else(|| panic!("hdr.wgsl declares {name}"));
            let text = line.split('=').nth(1).unwrap().trim().trim_end_matches(';');
            assert_eq!(text.parse::<f32>().unwrap(), value, "{name}");
        }
        for row in crate::post::tonemap::SRGB_TO_REC2020 {
            let text = format!("vec3<f32>({:?}, {:?}, {:?})", row[0], row[1], row[2]);
            assert!(wgsl.contains(&text), "hdr.wgsl lacks the row {text}");
        }
    }

    #[test]
    fn the_output_shader_composes_and_validates() {
        let composer = crate::shader::ShaderComposer::new();
        let source = composer.compose(HDR_OUTPUT_WGSL).unwrap();
        crate::shader::validate_wgsl(&source).unwrap();
    }
}
