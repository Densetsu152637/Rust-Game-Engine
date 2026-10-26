use engine_render_pipeline::indirect::*;
use glam::{Mat4, Vec3};

fn image(size: u32, color: [f32; 3]) -> RadianceImage {
    RadianceImage::new([size; 2], vec![color; (size * size) as usize]).unwrap()
}
fn environment(edge: u32) -> EnvironmentData {
    let diffuse = DiffusePolynomial::new([[0.0; 3]; 9]).unwrap();
    let mips = (0..=edge.ilog2())
        .map(|m| std::array::from_fn(|_| image(edge >> m, [2.0, 0.5, 1.0])))
        .collect();
    EnvironmentData::new(diffuse, mips).unwrap()
}

#[test]
fn radiance_is_linear_hdr_and_overflow_is_not_silently_clamped() {
    let data = image(2, [8.0, 0.25, MAX_HALF]);
    assert_eq!(data.pixels()[0], [8.0, 0.25, MAX_HALF]);
    assert_eq!(data.decoded_bytes(), 48);
    assert_eq!(data.texture_bytes(), 32);
    assert_eq!(
        data.scaled(0.5).unwrap().pixels()[0],
        [4.0, 0.125, MAX_HALF * 0.5]
    );
    for value in [-0.1, f32::NAN, f32::INFINITY, MAX_HALF + 1.0] {
        assert!(RadianceImage::new([1, 1], vec![[value; 3]]).is_err());
    }
    assert!(data.scaled(2.0).is_err());
    assert!(data.scaled(f32::NAN).is_err());
    assert!(RadianceImage::new([0, 1], vec![]).is_err());
    assert!(RadianceImage::new([2049, 1], vec![]).is_err());
    assert!(RadianceImage::new([2, 2], vec![[1.0; 3]]).is_err());
}

#[test]
fn environment_requires_every_face_and_mip_with_expected_dimensions() {
    let data = environment(4);
    assert_eq!(data.mips().len(), 3);
    assert_eq!(data.decoded_bytes(), 6 * (16 + 4 + 1) * 12);
    let diffuse = data.diffuse().clone();
    assert!(EnvironmentData::new(diffuse.clone(), vec![]).is_err());
    assert!(EnvironmentData::new(diffuse.clone(), vec![data.mips()[0].clone()]).is_err());
    let mut bad = data.mips().to_vec();
    bad[1][5] = image(1, [0.0; 3]);
    assert!(EnvironmentData::new(diffuse.clone(), bad).is_err());
    assert!(
        EnvironmentData::new(diffuse, vec![std::array::from_fn(|_| image(3, [0.0; 3]))]).is_err()
    );
}

#[test]
fn polynomial_has_an_unambiguous_signed_preconvolved_basis() {
    let mut coefficients = [[0.0; 3]; 9];
    coefficients[0] = [0.5, 1.0, 2.0];
    coefficients[1] = [0.25, -2.0, 0.0];
    coefficients[6] = [0.1, 0.0, 0.0];
    let diffuse = DiffusePolynomial::new(coefficients).unwrap();
    assert!(diffuse.evaluate(Vec3::ZERO).is_err());
    assert!((diffuse.evaluate(Vec3::X).unwrap() - Vec3::new(0.65, 0.0, 2.0)).length() < 1e-6);
    assert!((diffuse.evaluate(Vec3::Z).unwrap() - Vec3::new(0.7, 1.0, 2.0)).length() < 1e-6);
    coefficients[0][0] = f32::NAN;
    assert!(DiffusePolynomial::new(coefficients).is_err());
}

#[test]
fn budgets_reject_atomically_and_count_decoded_and_gpu_bytes_separately() {
    let data = environment(1);
    let mut budget = IndirectBudget::default();
    for _ in 0..MAX_ENVIRONMENTS {
        budget.include_environment(&data).unwrap();
    }
    let before = budget.clone();
    assert!(budget.include_environment(&data).is_err());
    assert_eq!(budget, before);
    let map = image(2048, [0.0; 3]);
    let mut budget = IndirectBudget::default();
    budget.include_lightmap(&map).unwrap();
    budget.include_lightmap(&map).unwrap();
    let before = budget.clone();
    assert!(budget.include_lightmap(&map).is_err());
    assert_eq!(budget, before);
    let mut count = IndirectBudget::default();
    for _ in 0..MAX_LIGHTMAPS {
        count.include_lightmap(&image(1, [0.0; 3])).unwrap();
    }
    assert!(count.include_lightmap(&image(1, [0.0; 3])).is_err());
}

fn probe(id: &str, priority: i32) -> ProbeVolume {
    ProbeVolume::new(id.into(), Mat4::IDENTITY, Vec3::ONE, 0.5, priority).unwrap()
}

#[test]
fn probe_selection_is_order_independent_and_blends_at_the_inner_boundary() {
    let probes = [probe("z", 1), probe("a", 1), probe("outside", 0)];
    let center = select_probe(&probes, Vec3::ZERO).unwrap();
    assert_eq!(probes[center.local.unwrap()].id(), "a");
    assert_eq!(center.local_weight, 1.0);
    let blend = select_probe(&probes, Vec3::new(0.75, 0.0, 0.0)).unwrap();
    assert_eq!((blend.local_weight, blend.global_weight), (0.5, 0.5));
    let boundary = select_probe(&probes, Vec3::X).unwrap();
    assert_eq!((boundary.local_weight, boundary.global_weight), (0.0, 1.0));
    assert_eq!(select_probe(&probes, Vec3::splat(2.0)).unwrap().local, None);
    let reverse: Vec<_> = probes.into_iter().rev().collect();
    let pick = select_probe(&reverse, Vec3::ZERO).unwrap();
    assert_eq!(reverse[pick.local.unwrap()].id(), "a");
    let higher = [probe("a", 1), probe("z", 2)];
    assert_eq!(select_probe(&higher, Vec3::ZERO).unwrap().local, Some(1));
}

#[test]
fn probes_use_the_declared_local_space_and_reject_bad_boundaries() {
    let world = Mat4::from_scale_rotation_translation(
        Vec3::splat(2.0),
        glam::Quat::from_rotation_z(0.7),
        Vec3::new(5.0, 3.0, 1.0),
    );
    let volume = ProbeVolume::new("rotated".into(), world, Vec3::ONE, 0.5, 0).unwrap();
    let selected =
        select_probe(&[volume], world.transform_point3(Vec3::new(0.75, 0.0, 0.0))).unwrap();
    assert!((selected.local_weight - 0.5).abs() < 1e-5);
    assert!(ProbeVolume::new("bad".into(), Mat4::ZERO, Vec3::ONE, 0.0, 0).is_err());
    assert!(ProbeVolume::new("bad".into(), Mat4::IDENTITY, Vec3::ZERO, 0.0, 0).is_err());
    assert!(ProbeVolume::new("bad".into(), Mat4::IDENTITY, Vec3::ONE, 2.0, 0).is_err());
    assert!(select_probe(&[probe("same", 0), probe("same", 1)], Vec3::ZERO).is_err());
    assert!(select_probe(&vec![probe("a", 0); MAX_PROBES + 1], Vec3::ZERO).is_err());
    assert!(select_probe(&[], Vec3::splat(f32::NAN)).is_err());
}

/// Independent f64 LIGHT-direction integral of the ACTUAL regularized shader.
fn brdf_reference(nv: f64, roughness: f64, resolution: u32) -> [f64; 2] {
    let a2 = roughness.max(0.045).powi(4);
    let shader_nv = nv.max(0.0001);
    let vx = (1.0 - nv * nv).sqrt();
    let gv = 2.0 * shader_nv / (shader_nv + (a2 + (1.0 - a2) * shader_nv * shader_nv).sqrt());
    let mut result = [0.0; 2];
    // Independent LIGHT-direction quadrature, with explicit change-of-variable
    // Jacobians. Cluster z around the mirror ray's z=nv, and azimuth around pi,
    // so low-roughness grazing lobes cannot fall between a uniform phi grid.
    // Symmetry about y=0 lets [0,pi] represent the full azimuth with factor 2.
    for side in 0..2 {
        let span = if side == 0 { nv } else { 1.0 - nv };
        for y in 0..resolution {
            let t = (y as f64 + 0.5) / f64::from(resolution);
            let nl = if side == 0 {
                nv - span * t * t
            } else {
                nv + span * t * t
            };
            let dz = 2.0 * span * t;
            let radial = (1.0 - nl * nl).max(0.0).sqrt();
            for x in 0..resolution {
                let u = (x as f64 + 0.5) / f64::from(resolution);
                let phi = std::f64::consts::PI * (1.0 - u.powi(4));
                let dphi = 4.0 * std::f64::consts::PI * u.powi(3);
                let hx = vx + radial * phi.cos();
                let hy = radial * phi.sin();
                let hz = nv + nl;
                let inverse = (hx * hx + hy * hy + hz * hz).sqrt().max(0.000001).recip();
                let nh = (hz * inverse).max(0.0);
                let vh = ((vx * hx + nv * hz) * inverse).max(0.0);
                let denom = nh * nh * (a2 - 1.0) + 1.0;
                let d = a2 / (std::f64::consts::PI * denom * denom).max(0.0000001);
                let gl = 2.0 * nl / (nl + (a2 + (1.0 - a2) * nl * nl).sqrt()).max(0.000001);
                let weight =
                    d * gv * gl * nl / (4.0 * shader_nv * nl).max(0.000001) * 2.0 * dz * dphi;
                let fc = (1.0 - vh).clamp(0.0, 1.0).powi(5);
                result[0] += weight * (1.0 - fc);
                result[1] += weight * fc;
            }
        }
    }
    result.map(|v| v / f64::from(resolution).powi(2))
}

#[test]
fn split_sum_matches_independent_hemisphere_integration() {
    for (nv, roughness) in [(0.25, 0.5), (0.5, 0.75), (0.9, 1.0)] {
        let actual = integrate_brdf(nv, roughness, 4096).unwrap();
        let expected = brdf_reference(f64::from(nv), f64::from(roughness), 256);
        for c in 0..2 {
            assert!(
                (f64::from(actual[c]) - expected[c]).abs() < 0.012,
                "{nv} {roughness} {actual:?} {expected:?}"
            );
        }
    }
    assert!(integrate_brdf(0.0, 0.5, 128).is_err());
    assert!(integrate_brdf(0.5, f32::NAN, 128).is_err());
    assert!(BrdfLut::prepare(128, 4096).is_err());
    let lut = BrdfLut::prepare(8, 64).unwrap();
    assert_eq!(lut.values().len(), 64);
    assert!(
        lut.values()
            .iter()
            .flatten()
            .all(|v| v.is_finite() && *v >= 0.0)
    );
}

#[test]
fn low_roughness_and_grazing_match_actual_shader_regularization() {
    // 512/1024 midpoint grids independently check oracle convergence. The
    // comparison allows 0.002 absolute + 4% relative error for bounded 4096
    // QMC samples; this is far below the omitted regularization's response.
    for (nv, roughness) in [
        (0.00005, 0.045),
        (0.01, 0.045),
        (0.9, 0.045),
        (0.00005, 0.1),
        (0.05, 0.1),
        (0.9, 0.1),
    ] {
        let coarse = brdf_reference(f64::from(nv), f64::from(roughness), 512);
        let fine = brdf_reference(f64::from(nv), f64::from(roughness), 1024);
        let actual = integrate_brdf(nv, roughness, 4096).unwrap();
        for c in 0..2 {
            assert!(
                (fine[c] - coarse[c]).abs() < 0.001 + 0.01 * fine[c].abs(),
                "unresolved oracle ({nv},{roughness}): {coarse:?} -> {fine:?}"
            );
            assert!(
                (f64::from(actual[c]) - fine[c]).abs() < 0.002 + 0.04 * fine[c].abs(),
                "shader regularization ({nv},{roughness}): {actual:?} vs {fine:?}"
            );
        }
    }
}
