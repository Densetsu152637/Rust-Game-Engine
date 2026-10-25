//! Release-mode timing. Run with
//! `cargo test -p engine-fluids --release --test perf -- --ignored --nocapture`.

mod common;

use common::{DT, block, params, tank};
use glam::Vec3;
use std::time::Instant;

#[test]
#[ignore = "timing run; use --release"]
fn dam_break_10k_particles_step_time() {
    let p = params();
    let min = Vec3::ZERO;
    let mut volume = tank(p, min, Vec3::new(2.5, 1.2, 1.25), 16_384);
    let n = volume
        .fill(block(min, Vec3::new(1.25, 0.8, 1.25)), Vec3::ZERO, None)
        .unwrap();
    assert_eq!(n, 25 * 16 * 25);
    let mut samples = Vec::new();
    for _ in 0..120 {
        let start = Instant::now();
        volume.step(DT, &[]).unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1.0e3);
    }
    let steady = &samples[20..];
    let mean = steady.iter().sum::<f64>() / steady.len() as f64;
    let max = steady.iter().copied().fold(0.0, f64::max);
    println!(
        "{n} particles, {} substeps x {} iterations: mean {mean:.2} ms, max {max:.2} ms per 1/60 s step",
        p.substeps, p.iterations
    );
    let start = Instant::now();
    let grid = volume
        .sample_density_grid(min, Vec3::new(2.5, 1.2, 1.25), [100, 48, 50])
        .unwrap();
    println!(
        "density grid {} samples: {:.2} ms",
        grid.values.len(),
        start.elapsed().as_secs_f64() * 1.0e3
    );
}

#[test]
#[ignore = "parameter sweep for docs/physics-fluids.md; use --release"]
fn stability_sweep() {
    for (substeps, iterations) in [(1, 2), (1, 4), (2, 2), (2, 4), (2, 8), (4, 4)] {
        for relaxation in [0.01, 0.1, 1.0] {
            let mut p = params();
            p.substeps = substeps;
            p.iterations = iterations;
            p.relaxation = relaxation;
            let min = Vec3::ZERO;
            let max = Vec3::new(1.0, 1.5, 0.3);
            let mut volume = tank(p, min, max, 8192);
            // Dam break: a 1 m column (20 layers) collapsing into a 1 m tank.
            volume
                .fill(block(min, Vec3::new(0.4, 1.0, 0.3)), Vec3::ZERO, None)
                .unwrap();
            let mut peak: f32 = 0.0;
            for _ in 0..240 {
                volume.step(DT, &[]).unwrap();
                peak = peak.max(volume.stats().max_density);
            }
            let stats = volume.stats();
            println!(
                "substeps {substeps} iterations {iterations} relaxation {relaxation}: interior {:.1} \
                 max {:.1} peak {:.1} kinetic {:.4} J max speed {:.3}",
                stats.mean_interior_density,
                stats.max_density,
                peak,
                stats.kinetic_energy,
                stats.max_speed
            );
        }
    }
}
