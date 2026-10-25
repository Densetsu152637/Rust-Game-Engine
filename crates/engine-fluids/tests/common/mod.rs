//! Shared scene builders for the behaviour tests.
#![allow(dead_code)]

use engine_fluids::{
    ColliderProxy, Domain, FluidParams, FluidVolume, Region, StepReport, VolumeDesc,
};
use glam::Vec3;

pub const DT: f32 = 1.0 / 60.0;

/// Coarse water (5 cm spacing) so debug-build tests stay fast.
pub fn params() -> FluidParams {
    FluidParams {
        particle_radius: 0.025,
        ..FluidParams::default()
    }
}

pub fn tank(params: FluidParams, min: Vec3, max: Vec3, capacity: usize) -> FluidVolume {
    FluidVolume::new(VolumeDesc {
        params,
        domain: Domain::new(min, max),
        capacity,
    })
    .expect("valid volume")
}

pub fn block(min: Vec3, max: Vec3) -> Region {
    Region::Box {
        center: (min + max) * 0.5,
        half_extents: (max - min) * 0.5,
    }
}

pub fn run(volume: &mut FluidVolume, seconds: f32, proxies: &[ColliderProxy]) -> Vec<StepReport> {
    let steps = (seconds / DT).round() as usize;
    (0..steps)
        .map(|_| volume.step(DT, proxies).expect("valid step"))
        .collect()
}
