use anyhow::{Result, ensure};
use glam::Vec3;

pub const MAX_LUT_SIZE: u32 = 128;
pub const MAX_SAMPLES: u32 = 4096;
pub const MAX_LUT_WORK: u64 = 16 * 1024 * 1024;

/// CPU split-sum GGX integration, matching engine::brdf's separable Smith,
/// roughness floor and distribution/view/specular denominator regularization.
/// Returns A,B for F0*A+B. This performs BRDF preparation only; it does not
/// prefilter environment images or bake a scene.
pub fn integrate_brdf(n_dot_v: f32, roughness: f32, samples: u32) -> Result<[f32; 2]> {
    ensure!(
        n_dot_v.is_finite() && n_dot_v > 0.0 && n_dot_v <= 1.0,
        "BRDF NdotV must be finite and within (0,1]"
    );
    ensure!(
        roughness.is_finite() && (0.0..=1.0).contains(&roughness),
        "BRDF roughness must be within [0,1]"
    );
    ensure!(
        (64..=MAX_SAMPLES).contains(&samples),
        "BRDF sample count must be 64..={MAX_SAMPLES}"
    );
    Ok(integrate(n_dot_v, roughness, samples))
}

fn integrate(nv: f32, roughness: f32, samples: u32) -> [f32; 2] {
    // The geometry uses the actual view direction; only the BRDF's NdotV is
    // floored. Replacing the view direction by the floored value is incorrect.
    let view = Vec3::new((1.0 - nv * nv).max(0.0).sqrt(), 0.0, nv);
    let shader_nv = nv.max(0.0001);
    let alpha = roughness.max(0.045).powi(2);
    let a2 = alpha * alpha;
    let gv = 2.0 * shader_nv / (shader_nv + (a2 + (1.0 - a2) * shader_nv * shader_nv).sqrt());
    let mut result = [0.0; 2];
    for i in 0..samples {
        let phi = std::f32::consts::TAU * (i as f32 + 0.5) / samples as f32;
        let xi = i.reverse_bits() as f32 * (1.0 / 4_294_967_296.0);
        let nh = ((1.0 - xi) / (1.0 + (a2 - 1.0) * xi)).sqrt();
        let sin = (1.0 - nh * nh).max(0.0).sqrt();
        let half = Vec3::new(sin * phi.cos(), sin * phi.sin(), nh);
        let vh = view.dot(half).max(0.0);
        let light = 2.0 * vh * half - view;
        let nl = light.z;
        if nl > 0.0 {
            // The PDF still uses IDEAL GGX. The actual shader clamps its D
            // denominator, so D no longer cancels out of BRDF / PDF. Retain
            // D_shader / D_ideal, including the shader's half-vector floor.
            let sum = light + view;
            let shader_half = sum / sum.length().max(0.000001);
            let shader_nh = shader_half.z.max(0.0);
            let shader_vh = view.dot(shader_half).max(0.0);
            let ideal_denom = nh * nh * (a2 - 1.0) + 1.0;
            let shader_denom = shader_nh * shader_nh * (a2 - 1.0) + 1.0;
            let distribution_ratio = std::f32::consts::PI * ideal_denom * ideal_denom
                / (std::f32::consts::PI * shader_denom * shader_denom).max(0.0000001);
            let gl = 2.0 * nl / (nl + (a2 + (1.0 - a2) * nl * nl).sqrt()).max(0.000001);
            let specular_denom = (4.0 * shader_nv * nl).max(0.000001);
            // Multiplying BRDF*cos(theta_l) by 1/pdf_l, where
            // pdf_l = D_ideal * NdotH / (4*VdotH).
            let weight = distribution_ratio * gv * gl * nl * 4.0 * vh / (nh * specular_denom);
            let fresnel = (1.0 - shader_vh).clamp(0.0, 1.0).powi(5);
            result[0] += (1.0 - fresnel) * weight;
            result[1] += fresnel * weight;
        }
    }
    result.map(|v| v / samples as f32)
}

/// Texel centers: x=NdotV, y=perceptual roughness, both (i+0.5)/size.
#[derive(Clone, Debug)]
pub struct BrdfLut {
    size: u32,
    values: Vec<[f32; 2]>,
}

impl BrdfLut {
    pub fn prepare(size: u32, samples: u32) -> Result<Self> {
        ensure!(
            (8..=MAX_LUT_SIZE).contains(&size),
            "BRDF LUT size must be 8..={MAX_LUT_SIZE}"
        );
        ensure!(
            (64..=MAX_SAMPLES).contains(&samples),
            "BRDF sample count must be 64..={MAX_SAMPLES}"
        );
        ensure!(
            u64::from(size) * u64::from(size) * u64::from(samples) <= MAX_LUT_WORK,
            "BRDF LUT preparation work limit exceeded"
        );
        let values = (0..size)
            .flat_map(|y| {
                (0..size).map(move |x| {
                    integrate(
                        (x as f32 + 0.5) / size as f32,
                        (y as f32 + 0.5) / size as f32,
                        samples,
                    )
                })
            })
            .collect();
        Ok(Self { size, values })
    }
    pub fn size(&self) -> u32 {
        self.size
    }
    pub fn values(&self) -> &[[f32; 2]] {
        &self.values
    }
}
