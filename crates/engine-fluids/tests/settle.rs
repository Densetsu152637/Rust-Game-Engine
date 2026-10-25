//! Settling, hydrostatics and viscosity behaviour.

mod common;

use common::{block, params, run, tank};
use glam::Vec3;

#[test]
fn dam_break_settles_to_rest_density_inside_the_domain() {
    let p = params();
    let min = Vec3::ZERO;
    let max = Vec3::new(1.0, 0.8, 0.3);
    let mut volume = tank(p, min, max, 2048);
    let n = volume
        .fill(block(min, Vec3::new(0.4, 0.5, 0.3)), Vec3::ZERO, None)
        .unwrap();
    assert_eq!(n, 8 * 10 * 6);
    let mut energy = Vec::new();
    for second in 0..4 {
        run(&mut volume, 1.0, &[]);
        let stats = volume.stats();
        energy.push(stats.kinetic_energy);
        println!("t={} {stats:?}", second + 1);
    }
    let stats = volume.stats();
    assert_eq!(stats.particle_count, n);
    let inside = volume
        .positions()
        .iter()
        .all(|&x| x.cmpge(min).all() && x.cmple(max).all());
    assert!(inside, "a particle left the domain");
    let rest = p.rest_density;
    assert!(
        (stats.mean_interior_density - rest).abs() < 0.03 * rest,
        "interior mean density {}",
        stats.mean_interior_density
    );
    assert!(
        stats.max_density < 1.15 * rest,
        "max density {}",
        stats.max_density
    );
    let per_particle = stats.kinetic_energy / n as f32;
    let mass = volume.particle_mass();
    // Mean speed below 2 cm/s.
    assert!(
        per_particle < 0.5 * mass * 0.02 * 0.02,
        "kinetic energy {per_particle}"
    );
    assert!(energy[3] < energy[0], "energy did not decay: {energy:?}");
}

#[test]
fn hydrostatic_column_height_and_density_gradient() {
    let p = params();
    let min = Vec3::ZERO;
    let max = Vec3::new(0.3, 1.2, 0.3);
    let mut volume = tank(p, min, max, 2048);
    let n = volume
        .fill(block(min, Vec3::new(0.3, 0.8, 0.3)), Vec3::ZERO, None)
        .unwrap();
    assert_eq!(n, 6 * 16 * 6);
    run(&mut volume, 3.0, &[]);
    let s = p.spacing();
    let expected = n as f32 * s * s * s / (0.3 * 0.3);
    let mut heights: Vec<f32> = volume.positions().iter().map(|x| x.y).collect();
    heights.sort_by(f32::total_cmp);
    let top_layer = &heights[heights.len() - 36..];
    let surface = top_layer.iter().sum::<f32>() / top_layer.len() as f32 + 0.5 * s;
    let layer_density = |lo: f32, hi: f32| {
        let (mut sum, mut count) = (0.0, 0);
        for (x, (&rho, &surface)) in volume
            .positions()
            .iter()
            .zip(volume.densities().iter().zip(volume.surface_flags()))
        {
            if x.y >= lo && x.y < hi && !surface {
                sum += rho;
                count += 1;
            }
        }
        sum / count as f32
    };
    let deep = layer_density(0.05, 0.3);
    let shallow = layer_density(0.45, 0.65);
    println!(
        "height {surface} expected {expected} deep {deep} shallow {shallow} {:?}",
        volume.stats()
    );
    assert!(
        (surface - expected).abs() < s,
        "surface {surface} expected {expected}"
    );
    assert!(deep > shallow, "deep {deep} shallow {shallow}");
}

/// Front (largest x of any particle) and centroid x of a released column after `seconds`.
fn spread(viscosity: f32, seconds: f32) -> (f32, f32) {
    let mut p = params();
    p.viscosity = viscosity;
    let min = Vec3::ZERO;
    let max = Vec3::new(1.5, 0.6, 0.15);
    let mut volume = tank(p, min, max, 1024);
    volume
        .fill(block(min, Vec3::new(0.2, 0.5, 0.15)), Vec3::ZERO, None)
        .unwrap();
    run(&mut volume, seconds, &[]);
    let xs = volume.positions().iter().map(|x| x.x);
    let front = xs.clone().fold(f32::MIN, f32::max);
    let centroid = xs.sum::<f32>() / volume.len() as f32;
    (front, centroid)
}

#[test]
fn higher_viscosity_reduces_spreading() {
    let (thin_front, thin_centroid) = spread(0.0, 0.5);
    let (thick_front, thick_centroid) = spread(0.8, 0.5);
    println!("front {thin_front} -> {thick_front}, centroid {thin_centroid} -> {thick_centroid}");
    // At least 1.5 particle spacings less run-out and a lower centroid.
    assert!(
        thick_front < thin_front - 0.075,
        "front thin {thin_front} thick {thick_front}"
    );
    assert!(
        thick_centroid < thin_centroid,
        "centroid thin {thin_centroid} thick {thick_centroid}"
    );
}
