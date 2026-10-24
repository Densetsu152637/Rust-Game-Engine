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
