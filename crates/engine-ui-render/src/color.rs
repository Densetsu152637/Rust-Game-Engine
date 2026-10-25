//! Colour-vision filter matrices: the CPU reference for the `ui.color_filter`
//! shader (which only multiplies by the matrix computed here).
//!
//! **Colour space.** All matrices act on *linear* sRGB (Rec. 709 primaries)
//! RGB, which is what the HDR scene colour holds. Machado et al. derive their
//! dichromacy simulation in a linear cone-response model and publish matrices
//! for linear RGB; applying them to gamma-encoded values would shift hues.
//!
//! **Sources.**
//! - Simulation: G. M. Machado, M. M. Oliveira and L. A. F. Fernandes, "A
//!   Physiologically-based Model for Simulation of Color Vision Deficiency",
//!   IEEE TVCG 15(6), 2009 (matrices at severity 1.0, the dichromacy case, from
//!   <https://www.inf.ufrgs.br/~oliveira/pubs_files/CVD_Simulation/CVD_Simulation.html>).
//!   The paper interpolates matrices linearly between severities, which is how
//!   `strength` is applied here (a blend from identity).
//! - Correction ("daltonization"): the error between the original and its
//!   simulation is moved into the channels the deficiency keeps, after
//!   B. Fidaner, M. Lin and N. Dhar, "Daltonize" (2005) and the commonly used
//!   protan/deutan shift `[[0,0,0],[.7,1,0],[.7,0,1]]`. The tritanopia shift
//!   `[[1,0,.7],[0,1,.7],[0,0,0]]` is this engine's analogue (blue-yellow
//!   error into red and green), not a published constant.
//! - Achromatopsia: Rec. 709 relative luminance `0.2126 R + 0.7152 G + 0.0722 B`
//!   in all three channels.
//!
//! Because simulation, correction and the strength blend are all linear in the
//! input colour, every filter is exactly one 3x3 matrix ([`filter_matrix`]).
use engine_project::accessibility::{ColorFilterKind, ColorFilterMode, ColorFilterSettings};

/// A row-major 3x3 matrix: `out = M * [r, g, b]`.
pub type Matrix3 = [[f32; 3]; 3];

pub const IDENTITY: Matrix3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Machado 2009, protanopia, severity 1.0.
pub const PROTANOPIA: Matrix3 = [
    [0.152286, 1.052583, -0.204868],
    [0.114503, 0.786281, 0.099216],
    [-0.003882, -0.048116, 1.051998],
];
/// Machado 2009, deuteranopia, severity 1.0.
pub const DEUTERANOPIA: Matrix3 = [
    [0.367322, 0.860646, -0.227968],
    [0.280085, 0.672501, 0.047413],
    [-0.011820, 0.042940, 0.968881],
];
/// Machado 2009, tritanopia, severity 1.0.
pub const TRITANOPIA: Matrix3 = [
    [1.255528, -0.076749, -0.178779],
    [-0.078411, 0.930809, 0.147602],
    [0.004733, 0.691367, 0.303900],
];
/// Rec. 709 luminance in every channel.
pub const ACHROMATOPSIA: Matrix3 = [
    [0.2126, 0.7152, 0.0722],
    [0.2126, 0.7152, 0.0722],
    [0.2126, 0.7152, 0.0722],
];

/// Error shift of protanopia and deuteranopia correction.
const SHIFT_RED_GREEN: Matrix3 = [[0.0, 0.0, 0.0], [0.7, 1.0, 0.0], [0.7, 0.0, 1.0]];
/// Error shift of tritanopia correction.
const SHIFT_BLUE_YELLOW: Matrix3 = [[1.0, 0.0, 0.7], [0.0, 1.0, 0.7], [0.0, 0.0, 0.0]];

/// The severity-1 simulation matrix of a deficiency (`None` for no filter).
pub fn simulation_matrix(kind: ColorFilterKind) -> Option<Matrix3> {
    match kind {
        ColorFilterKind::None => None,
        ColorFilterKind::Protanopia => Some(PROTANOPIA),
        ColorFilterKind::Deuteranopia => Some(DEUTERANOPIA),
        ColorFilterKind::Tritanopia => Some(TRITANOPIA),
        ColorFilterKind::Achromatopsia => Some(ACHROMATOPSIA),
    }
}

pub fn multiply(a: Matrix3, b: Matrix3) -> Matrix3 {
    let mut out = [[0.0; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    out
}

fn blend(from: Matrix3, to: Matrix3, t: f32) -> Matrix3 {
    let mut out = from;
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell += (to[r][c] - from[r][c]) * t;
        }
    }
    out
}

/// The matrix that applies `settings` to linear RGB: the identity for no
/// filter or strength 0, the simulation or daltonization matrix at strength 1,
/// and the linear blend in between. `Correct` with achromatopsia has no
/// defined correction (settings validation rejects it) and simulates.
pub fn filter_matrix(settings: ColorFilterSettings) -> Matrix3 {
    let Some(simulation) = simulation_matrix(settings.kind) else {
        return IDENTITY;
    };
    let strength = settings.strength.clamp(0.0, 1.0);
    let full = match (settings.mode, settings.kind) {
        (ColorFilterMode::Correct, ColorFilterKind::Protanopia | ColorFilterKind::Deuteranopia) => {
            daltonize(simulation, SHIFT_RED_GREEN)
        }
        (ColorFilterMode::Correct, ColorFilterKind::Tritanopia) => {
            daltonize(simulation, SHIFT_BLUE_YELLOW)
        }
        _ => simulation,
    };
    blend(IDENTITY, full, strength)
}

/// `I + shift * (I - simulation)`: the input plus its simulation error moved
/// into the kept channels.
fn daltonize(simulation: Matrix3, shift: Matrix3) -> Matrix3 {
    let mut error = IDENTITY;
    for (r, row) in error.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell -= simulation[r][c];
        }
    }
    let corrected = multiply(shift, error);
    let mut out = IDENTITY;
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell += corrected[r][c];
        }
    }
    out
}

/// Apply [`filter_matrix`] to a linear RGB colour; negative results clamp to 0
/// like the shader (values above 1 are kept: the scene is HDR).
pub fn apply_linear(rgb: [f32; 3], settings: ColorFilterSettings) -> [f32; 3] {
    apply_matrix(&filter_matrix(settings), rgb)
}

pub fn apply_matrix(matrix: &Matrix3, rgb: [f32; 3]) -> [f32; 3] {
    matrix.map(|row| (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]).max(0.0))
}

/// sRGB electro-optical transfer function (0..1 encoded to linear).
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB opto-electronic transfer function (linear to 0..1 encoded).
pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// The GPU uniform of the filter: the matrix as three padded rows.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FilterUniform {
    pub rows: [[f32; 4]; 3],
}

impl FilterUniform {
    pub fn new(matrix: &Matrix3) -> Self {
        Self {
            rows: matrix.map(|r| [r[0], r[1], r[2], 0.0]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(kind: ColorFilterKind, mode: ColorFilterMode, strength: f32) -> ColorFilterSettings {
        ColorFilterSettings {
            kind,
            mode,
            strength,
        }
    }

    fn close(a: [f32; 3], b: [f32; 3]) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 1e-3, "{a:?} != {b:?}");
        }
    }

    #[test]
    fn strength_zero_and_no_filter_are_the_identity() {
        for kind in ColorFilterKind::ALL {
            for mode in [ColorFilterMode::Simulate, ColorFilterMode::Correct] {
                assert_eq!(filter_matrix(settings(kind, mode, 0.0)), IDENTITY, "{kind:?}");
            }
        }
        assert_eq!(
            filter_matrix(settings(ColorFilterKind::None, ColorFilterMode::Simulate, 1.0)),
            IDENTITY
        );
        close(
            apply_linear([0.2, 0.5, 0.9], settings(ColorFilterKind::Tritanopia, ColorFilterMode::Simulate, 0.0)),
            [0.2, 0.5, 0.9],
        );
    }

    #[test]
    fn simulation_maps_primaries_to_the_published_columns() {
        use ColorFilterMode::Simulate;
        // Pure red is the first matrix column, pure green the second, blue the third.
        close(
            apply_linear([1.0, 0.0, 0.0], settings(ColorFilterKind::Protanopia, Simulate, 1.0)),
            [0.152286, 0.114503, 0.0], // third component -0.003882 clamps to 0
        );
        close(
            apply_linear([0.0, 1.0, 0.0], settings(ColorFilterKind::Protanopia, Simulate, 1.0)),
            [1.052583, 0.786281, 0.0],
        );
        close(
            apply_linear([1.0, 0.0, 0.0], settings(ColorFilterKind::Deuteranopia, Simulate, 1.0)),
            [0.367322, 0.280085, 0.0],
        );
        close(
            apply_linear([0.0, 1.0, 0.0], settings(ColorFilterKind::Deuteranopia, Simulate, 1.0)),
            [0.860646, 0.672501, 0.042940],
        );
        close(
            apply_linear([0.0, 0.0, 1.0], settings(ColorFilterKind::Tritanopia, Simulate, 1.0)),
            [0.0, 0.147602, 0.303900],
        );
    }

    #[test]
    fn simulation_rows_preserve_white() {
        // Machado's matrices keep achromatic colours achromatic: each row sums to 1.
        for matrix in [PROTANOPIA, DEUTERANOPIA, TRITANOPIA, ACHROMATOPSIA] {
            for row in matrix {
                assert!((row.iter().sum::<f32>() - 1.0).abs() < 2e-3, "{row:?}");
            }
        }
        close(
            apply_linear([0.5, 0.5, 0.5], settings(ColorFilterKind::Deuteranopia, ColorFilterMode::Simulate, 1.0)),
            [0.5, 0.5, 0.5],
        );
    }

    #[test]
    fn achromatopsia_is_rec709_luminance() {
        let out = apply_linear(
            [0.2, 0.4, 0.6],
            settings(ColorFilterKind::Achromatopsia, ColorFilterMode::Simulate, 1.0),
        );
        let y = 0.2126 * 0.2 + 0.7152 * 0.4 + 0.0722 * 0.6;
        close(out, [y, y, y]);
    }

    #[test]
    fn strength_blends_linearly_from_the_identity() {
        let full = apply_linear([1.0, 0.0, 0.0], settings(ColorFilterKind::Deuteranopia, ColorFilterMode::Simulate, 1.0));
        let half = apply_linear([1.0, 0.0, 0.0], settings(ColorFilterKind::Deuteranopia, ColorFilterMode::Simulate, 0.5));
        close(half, [0.5 * (1.0 + full[0]), 0.5 * full[1], 0.5 * full[2]]);
    }

    #[test]
    fn correction_moves_lost_red_green_difference_into_green_and_blue() {
        // Under deuteranopia red and green collapse toward each other; the
        // correction adds the lost error to G and B, so the corrected red is
        // distinguishable (more blue than the plain red) while keeping R.
        let red = [1.0, 0.0, 0.0];
        let corrected = apply_linear(red, settings(ColorFilterKind::Deuteranopia, ColorFilterMode::Correct, 1.0));
        assert!((corrected[0] - 1.0).abs() < 1e-5, "red channel is untouched: {corrected:?}");
        assert!(corrected[1] > 0.1 && corrected[2] > 0.4, "{corrected:?}");
        // White is unchanged: it has no simulation error.
        close(
            apply_linear([1.0, 1.0, 1.0], settings(ColorFilterKind::Deuteranopia, ColorFilterMode::Correct, 1.0)),
            [1.0, 1.0, 1.0],
        );
        // Tritanopia correction moves blue error into red and green.
        let blue = apply_linear([0.0, 0.0, 1.0], settings(ColorFilterKind::Tritanopia, ColorFilterMode::Correct, 1.0));
        assert!(blue[0] > 0.3 && blue[2] > 0.5, "{blue:?}");
    }

    #[test]
    fn srgb_transfer_round_trips() {
        for v in [0.0, 0.01, 0.2, 0.5, 0.9, 1.0] {
            assert!((srgb_to_linear(linear_to_srgb(v)) - v).abs() < 1e-4);
        }
        assert!((linear_to_srgb(0.152286) - 0.4266).abs() < 1e-3);
    }

    #[test]
    fn the_uniform_is_three_padded_rows() {
        let uniform = FilterUniform::new(&PROTANOPIA);
        assert_eq!(std::mem::size_of::<FilterUniform>(), 48);
        assert_eq!(uniform.rows[1], [0.114503, 0.786281, 0.099216, 0.0]);
    }
}
