//! Reference fixtures shared by the CPU tests, the GPU parity tests
//! (`engine-acoustics-gpu`) and examples.
//!
//! The reference room is the shoebox of section 8 of the design record: 10 x 6 x 8 m
//! (`V = 480 m^3`, `S = 376 m^2`, `4V/S = 5.106 m`) spanning `[0, 10] x [0, 6] x
//! [0, 8]`, with one uniform material.

use crate::{
    AcousticMaterial, AcousticMesh, AcousticWorld, AcousticWorldBuilder, BandGains, MaterialId,
    Mobility, Result, WorldLimits,
};

/// Reference shoebox dimensions in metres.
pub const SHOEBOX_DIMS_M: [f32; 3] = [10.0, 6.0, 8.0];
/// Per-band absorption of the section 8 reference numbers.
pub const SHOEBOX_ABSORPTION: BandGains = [0.05, 0.1, 0.2, 0.4];
/// Section 8 Eyring RT60 for [`SHOEBOX_ABSORPTION`] without air.
pub const SHOEBOX_EYRING_RT60_S: BandGains = [4.007, 1.951, 0.921, 0.402];
/// Section 8 Sabine RT60 for [`SHOEBOX_ABSORPTION`] without air.
pub const SHOEBOX_SABINE_RT60_S: BandGains = [4.111, 2.055, 1.028, 0.514];
/// Emitter (and listener-centric receiver) position used by the parity fixtures:
/// off-centre so no symmetry plane passes through it.
pub const SHOEBOX_LISTENER: [f32; 3] = [3.7, 2.1, 4.9];
/// Second point for source-to-listener jobs.
pub const SHOEBOX_SOURCE: [f32; 3] = [6.8, 1.4, 2.6];

/// Closed axis-aligned box `[min, max]` as 12 triangles (two per face, faces in
/// `-x, +x, -y, +y, -z, +z` order) with one material.
pub fn box_mesh(min: [f32; 3], max: [f32; 3], material: MaterialId) -> AcousticMesh {
    let corner = |i: usize| -> [f32; 3] {
        [
            if i & 1 == 0 { min[0] } else { max[0] },
            if i & 2 == 0 { min[1] } else { max[1] },
            if i & 4 == 0 { min[2] } else { max[2] },
        ]
    };
    const QUADS: [[u32; 4]; 6] = [
        [0, 2, 6, 4],
        [1, 5, 7, 3],
        [0, 4, 5, 1],
        [2, 3, 7, 6],
        [0, 1, 3, 2],
        [4, 6, 7, 5],
    ];
    let mut triangles = Vec::with_capacity(12);
    for q in QUADS {
        triangles.push([q[0], q[1], q[2]]);
        triangles.push([q[0], q[2], q[3]]);
    }
    AcousticMesh {
        positions: (0..8).map(corner).collect(),
        triangles,
        materials: vec![material],
    }
}

/// A shoebox world of size `dims` at the origin with one material.
pub fn shoebox_world(dims: [f32; 3], material: AcousticMaterial) -> Result<AcousticWorld> {
    let mut b = AcousticWorldBuilder::new(WorldLimits::default()).with_revision(1);
    let m = b.add_material(material)?;
    let mesh = b.add_mesh(box_mesh([0.0; 3], dims, m))?;
    b.add_instance(mesh, crate::math::IDENTITY, Mobility::Static, u8::MAX)?;
    b.build()
}

/// The section 8 reference room: [`SHOEBOX_DIMS_M`] with
/// [`SHOEBOX_ABSORPTION`], no transmission and the given scattering.
pub fn reference_shoebox(scattering: f32) -> Result<AcousticWorld> {
    shoebox_world(
        SHOEBOX_DIMS_M,
        AcousticMaterial {
            absorption: SHOEBOX_ABSORPTION,
            scattering,
            transmission: [0.0; 4],
        },
    )
}

/// Trace settings of the parity fixtures: 1 ms bins over `length_s`, c = 343 m/s,
/// no air absorption.
pub fn reference_trace_settings(length_s: f32) -> crate::TraceSettings {
    crate::TraceSettings {
        bin_s: 0.001,
        bins: (length_s / 0.001).round() as u32,
        speed_of_sound: 343.0,
        air_np_per_m: [0.0; 4],
    }
}

/// The listener-centric late-field job of the parity fixtures (emitter and
/// receiver at [`SHOEBOX_LISTENER`], 1 m receiver, 256 reflections).
pub fn reference_job(rays: u32, seed: u64) -> crate::EnergyJob {
    crate::EnergyJob {
        emitter: SHOEBOX_LISTENER,
        receiver: SHOEBOX_LISTENER,
        receiver_radius_m: 1.0,
        rays,
        max_bounces: 256,
        seed,
    }
}

/// The CPU reference histogram for one job, for comparison with GPU tiers.
pub fn cpu_reference_histogram(
    world: &AcousticWorld,
    settings: &crate::TraceSettings,
    job: &crate::EnergyJob,
    threads: u8,
) -> std::result::Result<crate::EnergyHistogram, crate::BackendError> {
    use crate::PropagationBackend;
    let mut cpu = crate::CpuBackend::new(threads)?;
    let mut out = Vec::new();
    cpu.trace(world, settings, std::slice::from_ref(job), &mut out)?;
    Ok(out.remove(0))
}

/// Energy per band summed over bins whose start time lies in `[start_s, end_s)`.
pub fn window_energy(h: &crate::EnergyHistogram, start_s: f32, end_s: f32) -> BandGains {
    let mut e = [0.0f64; 4];
    for (i, bin) in h.bins.iter().enumerate() {
        let t = i as f32 * h.bin_s;
        if t >= start_s && t < end_s {
            for b in 0..4 {
                e[b] += f64::from(bin[b]);
            }
        }
    }
    e.map(|v| v as f32)
}

/// CSV dump: a header `time_s,e125,e500,e2000,e8000`, then one row per bin (start
/// time, energies), then `#` comment lines with rays, escaped, aborted and the mean
/// free path.
pub fn histogram_csv(h: &crate::EnergyHistogram) -> String {
    use std::fmt::Write;
    let mut s = String::from("time_s,e125,e500,e2000,e8000\n");
    for (i, b) in h.bins.iter().enumerate() {
        let _ = writeln!(
            s,
            "{:.6},{:e},{:e},{:e},{:e}",
            i as f32 * h.bin_s,
            b[0],
            b[1],
            b[2],
            b[3]
        );
    }
    let _ = writeln!(
        s,
        "# rays={} escaped={:e} aborted={} mean_free_path_m={}",
        h.rays, h.escaped, h.aborted, h.mean_free_path_m
    );
    s
}
