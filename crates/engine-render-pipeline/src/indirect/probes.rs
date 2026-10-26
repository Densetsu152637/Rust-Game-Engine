use super::MAX_PROBES;
use anyhow::{Result, ensure};
use glam::{Mat4, Vec3};
use std::collections::BTreeSet;

/// Oriented probe volume. Extents/blend distance use probe-local units; its
/// transform may include scale. Data is immutable after boundary validation.
#[derive(Clone, Debug)]
pub struct ProbeVolume {
    id: String,
    probe_from_world: Mat4,
    half_extents: Vec3,
    blend_distance: f32,
    priority: i32,
}

impl ProbeVolume {
    pub fn new(
        id: String,
        world_from_probe: Mat4,
        half_extents: Vec3,
        blend_distance: f32,
        priority: i32,
    ) -> Result<Self> {
        ensure!(
            !id.is_empty() && id.len() <= 64,
            "probe id must contain 1..=64 bytes"
        );
        ensure!(
            world_from_probe.is_finite() && world_from_probe.determinant().abs() > 1e-8,
            "probe transform must be finite and invertible"
        );
        ensure!(
            world_from_probe.x_axis.w == 0.0
                && world_from_probe.y_axis.w == 0.0
                && world_from_probe.z_axis.w == 0.0
                && world_from_probe.w_axis.w == 1.0,
            "probe transform must be affine"
        );
        let probe_from_world = world_from_probe.inverse();
        ensure!(
            probe_from_world.is_finite(),
            "probe inverse transform is nonfinite"
        );
        ensure!(
            half_extents.is_finite()
                && half_extents.cmpgt(Vec3::ZERO).all()
                && half_extents.cmple(Vec3::splat(1_000_000.0)).all(),
            "probe half extents must be within (0, 1000000]"
        );
        ensure!(
            blend_distance.is_finite()
                && blend_distance >= 0.0
                && blend_distance <= half_extents.min_element(),
            "probe blend distance must be finite and within its smallest half extent"
        );
        Ok(Self {
            id,
            probe_from_world,
            half_extents,
            blend_distance,
            priority,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn priority(&self) -> i32 {
        self.priority
    }

    fn weight(&self, world: Vec3) -> Result<Option<f32>> {
        let local = self.probe_from_world.transform_point3(world);
        ensure!(local.is_finite(), "probe position transform overflow");
        let distance = (self.half_extents - local.abs()).min_element();
        if distance < 0.0 {
            return Ok(None);
        }
        let t = if self.blend_distance == 0.0 {
            1.0
        } else {
            (distance / self.blend_distance).clamp(0.0, 1.0)
        };
        Ok(Some(t * t * (3.0 - 2.0 * t)))
    }
}

/// At most one local probe plus global fallback. `local` indexes the supplied
/// slice; resolve that volume's environment externally. No temporal state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProbeSelection {
    pub local: Option<usize>,
    pub local_weight: f32,
    pub global_weight: f32,
}

/// Deterministic per-draw selection at the object's world-space sample point.
/// Highest priority wins; equal priorities choose the lexicographically first id.
/// Large meshes are NOT interpolated across several local probe volumes.
pub fn select_probe(probes: &[ProbeVolume], world: Vec3) -> Result<ProbeSelection> {
    ensure!(world.is_finite(), "probe sample position must be finite");
    ensure!(
        probes.len() <= MAX_PROBES,
        "at most {MAX_PROBES} local probes"
    );
    let mut ids = BTreeSet::new();
    let mut selected: Option<(usize, f32)> = None;
    for (index, probe) in probes.iter().enumerate() {
        ensure!(ids.insert(probe.id()), "duplicate probe id {}", probe.id());
        let Some(weight) = probe.weight(world)? else {
            continue;
        };
        if selected.is_none_or(|(prior, _)| {
            probe.priority > probes[prior].priority
                || (probe.priority == probes[prior].priority && probe.id < probes[prior].id)
        }) {
            selected = Some((index, weight));
        }
    }
    Ok(match selected {
        Some((index, weight)) => ProbeSelection {
            local: Some(index),
            local_weight: weight,
            global_weight: 1.0 - weight,
        },
        None => ProbeSelection {
            local: None,
            local_weight: 0.0,
            global_weight: 1.0,
        },
    })
}
