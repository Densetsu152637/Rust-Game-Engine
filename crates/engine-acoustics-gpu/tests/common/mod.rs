//! Scenes and comparison helpers shared by the GPU acceptance tests.
#![allow(dead_code)]

use engine_acoustics::{
    AcousticMaterial, AcousticMesh, AcousticWorld, AcousticWorldBuilder, Affine3, BANDS, BandGains,
    EnergyHistogram, EnergyJob, InstanceId, MaterialId, Mobility, TraceSettings, WorldLimits,
};

pub const IDENTITY: Affine3 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
/// Reference absorption of section 8.
pub const REFERENCE_ALPHA: BandGains = [0.05, 0.1, 0.2, 0.4];

pub fn translation(x: f32, y: f32, z: f32) -> Affine3 {
    [1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, y, 0.0, 0.0, 1.0, z]
}

pub fn material(absorption: BandGains, scattering: f32) -> AcousticMaterial {
    AcousticMaterial {
        absorption,
        scattering,
        transmission: [0.0; BANDS],
    }
}

/// Closed box [0, size] with every face split into `k x k` cells (2 triangles each).
pub fn box_mesh(size: [f32; 3], k: u32, material: MaterialId) -> AcousticMesh {
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    // (axis fixed, value, u axis, v axis)
    for axis in 0..3 {
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        for side in [0.0, size[axis]] {
            let base = positions.len() as u32;
            for i in 0..=k {
                for j in 0..=k {
                    let mut p = [0.0f32; 3];
                    p[axis] = side;
                    p[u] = size[u] * i as f32 / k as f32;
                    p[v] = size[v] * j as f32 / k as f32;
                    positions.push(p);
                }
            }
            for i in 0..k {
                for j in 0..k {
                    let a = base + i * (k + 1) + j;
                    let b = a + k + 1;
                    triangles.push([a, b, b + 1]);
                    triangles.push([a, b + 1, a + 1]);
                }
            }
        }
    }
    AcousticMesh {
        positions,
        triangles,
        materials: vec![material],
    }
}

/// Reference shoebox (10 x 6 x 8 m) with uniform material.
pub fn shoebox_world(absorption: BandGains, scattering: f32, k: u32) -> AcousticWorld {
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let m = b
        .add_material(material(absorption, scattering))
        .expect("material");
    let mesh = b.add_mesh(box_mesh([10.0, 6.0, 8.0], k, m)).expect("mesh");
    b.add_instance(mesh, IDENTITY, Mobility::Static, 0xff)
        .expect("instance");
    b.build().expect("world")
}

/// L-shaped room (footprint (0,0)-(10,4) plus (0,4)-(4,10) in x-z, 3 m high)
/// with a dynamic 0.2 x 3 x 2.5 m panel. Returns the world and the panel id.
pub fn l_room_world() -> (AcousticWorld, InstanceId) {
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let walls = b
        .add_material(material([0.05, 0.1, 0.2, 0.3], 0.5))
        .expect("material");
    let panel_material = b
        .add_material(material([0.1, 0.2, 0.3, 0.5], 0.3))
        .expect("material");
    let h = 3.0;
    // Footprint corners counter-clockwise.
    let footprint = [
        [0.0, 0.0],
        [10.0, 0.0],
        [10.0, 4.0],
        [4.0, 4.0],
        [4.0, 10.0],
        [0.0, 10.0],
    ];
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    let mut quad = |positions: &mut Vec<[f32; 3]>, q: [[f32; 3]; 4]| {
        let base = positions.len() as u32;
        positions.extend_from_slice(&q);
        triangles.push([base, base + 1, base + 2]);
        triangles.push([base, base + 2, base + 3]);
    };
    for i in 0..footprint.len() {
        let [x0, z0] = footprint[i];
        let [x1, z1] = footprint[(i + 1) % footprint.len()];
        quad(
            &mut positions,
            [[x0, 0.0, z0], [x1, 0.0, z1], [x1, h, z1], [x0, h, z0]],
        );
    }
    for y in [0.0, h] {
        for (min, max) in [([0.0, 0.0], [10.0, 4.0]), ([0.0, 4.0], [4.0, 10.0])] {
            quad(
                &mut positions,
                [
                    [min[0], y, min[1]],
                    [max[0], y, min[1]],
                    [max[0], y, max[1]],
                    [min[0], y, max[1]],
                ],
            );
        }
    }
    let room = b
        .add_mesh(AcousticMesh {
            positions,
            triangles,
            materials: vec![walls],
        })
        .expect("room mesh");
    b.add_instance(room, IDENTITY, Mobility::Static, 0xff)
        .expect("room");
    let panel = b
        .add_mesh(box_mesh([0.2, 2.5, 3.0], 1, panel_material))
        .expect("panel mesh");
    let id = b
        .add_instance(panel, translation(5.0, 0.0, 0.5), Mobility::Dynamic, 0xff)
        .expect("panel");
    (b.build().expect("world"), id)
}

pub fn settings(length_s: f32) -> TraceSettings {
    TraceSettings {
        bin_s: 0.001,
        bins: (length_s / 0.001).round() as u32,
        speed_of_sound: 343.0,
        air_np_per_m: [0.0; BANDS],
    }
}

pub fn job(emitter: [f32; 3], receiver: [f32; 3], rays: u32, bounces: u16, seed: u64) -> EnergyJob {
    EnergyJob {
        emitter,
        receiver,
        receiver_radius_m: 1.0,
        rays,
        max_bounces: bounces,
        seed,
    }
}

/// Per-band energy in `[start_s, end_s)`.
pub fn window(h: &EnergyHistogram, start_s: f32, end_s: f32) -> BandGains {
    let mut sum = [0.0f64; BANDS];
    for (i, bin) in h.bins.iter().enumerate() {
        let t = i as f32 * h.bin_s;
        if t >= start_s && t < end_s {
            for (s, v) in sum.iter_mut().zip(bin) {
                *s += f64::from(*v);
            }
        }
    }
    sum.map(|v| v as f32)
}

pub fn db_ratio(a: BandGains, b: BandGains) -> BandGains {
    std::array::from_fn(|i| 10.0 * (a[i] / b[i]).log10())
}

pub fn relative(a: f32, b: f32) -> f32 {
    (a - b).abs() / b.abs()
}
