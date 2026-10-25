//! CPU reference of the tonemapping curves, mirrored by
//! `wgsl/tonemap.wgsl` (`engine::tonemap`). The GPU tests compare against
//! these.

use super::settings::TonemapOperator;

type V3 = [f32; 3];

fn rows(m: &[V3; 3], v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

const SRGB_TO_REC2020: [V3; 3] = [
    [0.6274, 0.3293, 0.0433],
    [0.0691, 0.9195, 0.0114],
    [0.0164, 0.0880, 0.8956],
];
const REC2020_TO_SRGB: [V3; 3] = [
    [1.6605, -0.5876, -0.0728],
    [-0.1246, 1.1329, -0.0083],
    [-0.0182, -0.1006, 1.1187],
];
const AGX_INSET: [V3; 3] = [
    [0.856_627_15, 0.095_121_24, 0.048_251_606],
    [0.137_318_97, 0.761_242, 0.101_439_04],
    [0.111_898_21, 0.076_799_42, 0.811_302_4],
];
const AGX_OUTSET: [V3; 3] = [
    [1.127_100_6, -0.110_006_64, -0.016_493_94],
    [-0.141_329_76, 1.157_823_7, -0.016_493_94],
    [-0.141_329_76, -0.110_606_64, 1.251_936_4],
];
const AGX_MIN_EV: f32 = -12.473_93;
const AGX_MAX_EV: f32 = 4.026_069;
const LUMA: V3 = [0.2126, 0.7152, 0.0722];

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Narkowicz ACES fit of one channel.
pub fn aces_channel(x: f32) -> f32 {
    let x = x.max(0.0);
    ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0)
}

fn agx_contrast(x: f32) -> f32 {
    let x2 = x * x;
    let x4 = x2 * x2;
    15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x
        - 0.00232
}

/// AgX-style tonemap of a linear sRGB colour to a display-linear colour.
pub fn agx(color: V3, punchy: bool) -> V3 {
    let mut c = rows(&AGX_INSET, rows(&SRGB_TO_REC2020, color));
    for v in &mut c {
        let log = v.max(1e-10).log2();
        let n = ((log - AGX_MIN_EV) / (AGX_MAX_EV - AGX_MIN_EV)).clamp(0.0, 1.0);
        *v = agx_contrast(n);
    }
    if punchy {
        for v in &mut c {
            *v = v.max(0.0).powf(1.35);
        }
        let luma = dot(c, LUMA);
        for v in &mut c {
            *v = luma + 1.4 * (*v - luma);
        }
    }
    let mut c = rows(&AGX_OUTSET, c);
    for v in &mut c {
        *v = v.max(0.0).powf(2.2);
    }
    let c = rows(&REC2020_TO_SRGB, c);
    [
        c[0].clamp(0.0, 1.0),
        c[1].clamp(0.0, 1.0),
        c[2].clamp(0.0, 1.0),
    ]
}

/// Khronos PBR Neutral.
pub fn neutral(color: V3) -> V3 {
    const START: f32 = 0.8 - 0.04;
    const DESAT: f32 = 0.15;
    let x = color[0].min(color[1]).min(color[2]);
    let offset = if x < 0.08 { x - 6.25 * x * x } else { 0.04 };
    let c = [color[0] - offset, color[1] - offset, color[2] - offset];
    let peak = c[0].max(c[1]).max(c[2]);
    if peak < START {
        return c;
    }
    let d = 1.0 - START;
    let new_peak = 1.0 - d * d / (peak + d - START);
    let scale = new_peak / peak;
    let c = [c[0] * scale, c[1] * scale, c[2] * scale];
    let g = 1.0 - 1.0 / (DESAT * (peak - new_peak) + 1.0);
    [
        c[0] + (new_peak - c[0]) * g,
        c[1] + (new_peak - c[1]) * g,
        c[2] + (new_peak - c[2]) * g,
    ]
}

/// The display-linear colour (0..1) for a scene-linear input.
pub fn tonemap(operator: TonemapOperator, color: V3) -> V3 {
    match operator {
        TonemapOperator::None => color.map(|c| c.clamp(0.0, 1.0)),
        TonemapOperator::Aces => color.map(aces_channel),
        TonemapOperator::Agx => agx(color, false),
        TonemapOperator::AgxPunchy => agx(color, true),
        TonemapOperator::Neutral => neutral(color).map(|c| c.clamp(0.0, 1.0)),
    }
}

/// sRGB opto-electronic transfer of a linear value.
pub fn srgb_encode(linear: f32) -> f32 {
    let l = linear.clamp(0.0, 1.0);
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// Inverse of [`srgb_encode`].
pub fn srgb_decode(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aces_reference_values() {
        // Narkowicz: x(2.51x+0.03)/(x(2.43x+0.59)+0.14)
        for (x, want) in [(0.0, 0.0), (0.18, 0.2669), (1.0, 0.8039), (4.0, 0.9734)] {
            let got = aces_channel(x);
            assert!((got - want).abs() < 2e-4, "aces({x}) = {got}, want {want}");
        }
    }

    #[test]
    fn neutral_reference_values() {
        // Below the compression start the curve is the identity minus the
        // toe offset: 0.5 -> 0.5 - 0.04.
        let mid = neutral([0.5; 3]);
        assert!(mid.iter().all(|c| (c - 0.46).abs() < 1e-5));
        let high = neutral([4.0; 3]);
        assert!(high.iter().all(|c| (c - high[0]).abs() < 1e-5));
        assert!(high[0] > 0.9 && high[0] < 1.0, "{high:?}");
        let black = neutral([0.0; 3]);
        assert!(black.iter().all(|c| c.abs() < 1e-6));
    }

    #[test]
    fn agx_properties() {
        for punchy in [false, true] {
            let mut previous = -1.0;
            for stop in -14..=8 {
                let x = 0.18 * (stop as f32).exp2();
                let out = agx([x; 3], punchy);
                assert!(
                    out.iter().all(|c| (c - out[0]).abs() < 5e-3),
                    "grey stays grey: {out:?}"
                );
                assert!(out[0] >= previous - 1e-6, "monotonic at {x}");
                assert!(out[0] <= 1.0);
                previous = out[0];
            }
            assert!(agx([0.0; 3], punchy)[0] < 0.01);
            let mid = agx([0.18; 3], punchy)[0];
            assert!((0.1..0.5).contains(&mid), "middle grey maps to {mid}");
            assert!(agx([100.0; 3], punchy)[0] > 0.9);
        }
        let base = agx([0.5, 0.2, 0.1], false);
        let punchy = agx([0.5, 0.2, 0.1], true);
        let chroma = |c: V3| c[0] - c[2];
        assert!(chroma(punchy) > chroma(base), "punchy is more saturated");
    }

    #[test]
    fn none_clamps() {
        assert_eq!(
            tonemap(TonemapOperator::None, [4.0, 0.5, -1.0]),
            [1.0, 0.5, 0.0]
        );
    }

    #[test]
    fn srgb_round_trips() {
        for v in [0.0, 0.01, 0.2, 0.5, 1.0] {
            assert!((srgb_decode(srgb_encode(v)) - v).abs() < 1e-5);
        }
    }
}
