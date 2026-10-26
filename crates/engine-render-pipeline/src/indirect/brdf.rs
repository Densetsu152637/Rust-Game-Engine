use anyhow::{Result, ensure};
use glam::DVec3;

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

// The shader caps D by flooring its denominator. Ideal GGX concentrates most
// samples inside that suppressed peak; at grazing angles the remaining tail is
// poorly covered even with 4096 samples. Sample the actual capped NDF instead.
struct GgxSampling {
    a2: f64,
    cap: f64,
    tail_mass: f64,
    mass: f64,
}
impl GgxSampling {
    fn new(a2: f64) -> Self {
        let cap = (1e-7 / std::f64::consts::PI).sqrt();
        let (tail_mass, mass) = if a2 < cap {
            let tail = a2 / (1.0 - a2) * (cap.recip() - 1.0);
            let peak = a2 * (cap - a2) / ((1.0 - a2) * cap * cap);
            (tail, tail + peak)
        } else {
            (1.0, 1.0)
        };
        Self {
            a2,
            cap,
            tail_mass,
            mass,
        }
    }

    fn sin_squared(&self, xi: f64) -> f64 {
        if self.a2 >= self.cap {
            return self.a2 * xi / (1.0 + (self.a2 - 1.0) * xi);
        }
        // u=NdotH^2 has density pi*D. With d=1-(1-a2)*u,
        // its lower CDF is a2/(1-a2)*(1/d-1) until d=cap;
        // the remaining capped peak has constant density a2/cap^2.
        let target = (1.0 - xi) * self.mass;
        let sin2 = if target <= self.tail_mass {
            let d = (1.0 + target * (1.0 - self.a2) / self.a2).recip();
            (d - self.a2) / (1.0 - self.a2)
        } else {
            (self.mass - target) * self.cap * self.cap / self.a2
        };
        sin2.clamp(0.0, 1.0)
    }
}

fn integrate(nv: f32, roughness: f32, samples: u32) -> [f32; 2] {
    // CPU preparation uses f64 for narrow-lobe inversion and accumulation. This
    // preserves the shader's mathematical regularization, not GPU f32 rounding.
    let nv = f64::from(nv);
    // Actual geometry uses the unfloored view direction.
    let view = DVec3::new((1.0 - nv * nv).max(0.0).sqrt(), 0.0, nv);
    let shader_nv = nv.max(0.0001);
    let a2 = f64::from(roughness).max(0.045).powi(4);
    let sampling = GgxSampling::new(a2);
    let gv = 2.0 * shader_nv / (shader_nv + (a2 + (1.0 - a2) * shader_nv * shader_nv).sqrt());
    let mut result = [0.0; 2];
    for i in 0..samples {
        let phi = std::f64::consts::TAU * (f64::from(i) + 0.5) / f64::from(samples);
        let xi = f64::from(i.reverse_bits()) * (1.0 / 4_294_967_296.0);
        let sin2 = sampling.sin_squared(xi);
        let nh = (1.0 - sin2).max(0.0).sqrt();
        let sin = sin2.sqrt();
        let half = DVec3::new(sin * phi.cos(), sin * phi.sin(), nh);
        let vh = view.dot(half).max(0.0);
        let light = 2.0 * vh * half - view;
        let nl = light.z;
        if nl > 0.0 && nh > 0.0 {
            // Reconstruct the ACTUAL shader half vector, including its length
            // floor. Its D need not equal the sampled half-vector's capped D.
            let sum = light + view;
            let shader_half = sum / sum.length().max(0.000001);
            let shader_nh = shader_half.z.max(0.0);
            let shader_vh = view.dot(shader_half).max(0.0);
            let sampled_denom = a2 + (1.0 - a2) * sin2;
            let shader_denom = shader_nh * shader_nh * (a2 - 1.0) + 1.0;
            let distribution_ratio = (std::f64::consts::PI * sampled_denom * sampled_denom)
                .max(1e-7)
                / (std::f64::consts::PI * shader_denom * shader_denom).max(1e-7);
            let gl = 2.0 * nl / (nl + (a2 + (1.0 - a2) * nl * nl).sqrt()).max(0.000001);
            let specular_denom = (4.0 * shader_nv * nl).max(0.000001);
            // pdf_h=D_sample*NdotH/Z, pdf_l=pdf_h/(4*VdotH).
            // Thus BRDF*cos(theta_l)/pdf_l retains Z and D_shader/D_sample.
            let weight = sampling.mass * distribution_ratio * gv * gl * nl * 4.0 * vh
                / (nh * specular_denom);
            let fresnel = (1.0 - shader_vh).clamp(0.0, 1.0).powi(5);
            result[0] += (1.0 - fresnel) * weight;
            result[1] += fresnel * weight;
        }
    }
    result.map(|v| (v / f64::from(samples)) as f32)
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

#[cfg(test)]
mod sampling_tests {
    use super::GgxSampling;

    // Independent quadrature of the original shader D, in u=NdotH^2. Cluster
    // cells near the cap at u=1; no production CDF or normalization formula.
    fn density_integral(a2: f64, upper: f64) -> f64 {
        let mut sum = 0.0;
        for i in 0..32768 {
            let t = (f64::from(i) + 0.5) / 32768.0;
            let u = upper * (1.0 - t * t);
            let denom = u * (a2 - 1.0) + 1.0;
            let density =
                std::f64::consts::PI * a2 / (std::f64::consts::PI * denom * denom).max(1e-7);
            sum += density * 2.0 * upper * t;
        }
        sum / 32768.0
    }

    #[test]
    fn cap_transition_is_continuous_and_roughness_one_is_finite() {
        let boundary = (1e-7 / std::f64::consts::PI).sqrt();
        let below = GgxSampling::new(boundary * (1.0 - 1e-6));
        let above = GgxSampling::new(boundary * (1.0 + 1e-6));
        assert!((below.mass - above.mass).abs() < 1e-10);
        let isotropic = GgxSampling::new(1.0);
        assert_eq!(isotropic.mass, 1.0);
        for xi in [0.0, 0.125, 0.5, 0.875, 0.999, 1.0] {
            assert!((below.sin_squared(xi) - above.sin_squared(xi)).abs() < 2e-6);
            assert_eq!(isotropic.sin_squared(xi), xi);
        }
    }

    #[test]
    fn capped_ndf_normalization_and_inverse_cdf_cover_both_regimes() {
        let boundary = (1e-7 / std::f64::consts::PI).sqrt();
        for a2 in [
            0.045f64.powi(4),
            0.1f64.powi(4),
            boundary * (1.0 - 1e-6),
            boundary,
            boundary * (1.0 + 1e-6),
            0.5f64.powi(4),
            1.0,
        ] {
            let sampling = GgxSampling::new(a2);
            let integral = density_integral(a2, 1.0);
            assert!(
                (integral / sampling.mass - 1.0).abs() < 2e-5,
                "NDF mass at a2={a2}: {integral} vs {}",
                sampling.mass
            );
            for xi in [0.0, 0.125, 0.5, 0.875, 0.999] {
                let sin2 = sampling.sin_squared(xi);
                assert!((0.0..=1.0).contains(&sin2));
                let cdf = density_integral(a2, 1.0 - sin2) / sampling.mass;
                assert!(
                    (cdf - (1.0 - xi)).abs() < 2e-5,
                    "inverse CDF at a2={a2},xi={xi}: {cdf}"
                );
            }
        }
    }
}
