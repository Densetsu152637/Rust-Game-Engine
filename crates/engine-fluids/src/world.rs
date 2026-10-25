//! Fluid volumes keyed by stable ids, stepped together against one proxy list.

use crate::proxy::{self, ColliderProxy, ProxyImpulse};
use crate::volume::{FluidVolume, StepReport, VolumeDesc, validate_dt};
use crate::{Error, MAX_VOLUMES, Result};
use std::collections::BTreeMap;

/// Result of [`FluidWorld::step`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldStepReport {
    /// Per-volume reports in ascending volume id order.
    pub volumes: Vec<(u64, StepReport)>,
    /// Impulses summed over all volumes for every supplied proxy, sorted by proxy id.
    pub proxy_impulses: Vec<ProxyImpulse>,
}

/// Owner of fluid volumes addressed by stable `u64` ids (for example ECS entity bits).
#[derive(Clone, Debug, Default)]
pub struct FluidWorld {
    volumes: BTreeMap<u64, FluidVolume>,
}

impl FluidWorld {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_volume(&mut self, id: u64, desc: VolumeDesc) -> Result<&mut FluidVolume> {
        if self.volumes.contains_key(&id) {
            return Err(Error::DuplicateVolume(id));
        }
        if self.volumes.len() >= MAX_VOLUMES {
            return Err(Error::Invalid("too many fluid volumes (MAX_VOLUMES)"));
        }
        let volume = FluidVolume::new(desc)?;
        Ok(self.volumes.entry(id).or_insert(volume))
    }

    pub fn remove_volume(&mut self, id: u64) -> Option<FluidVolume> {
        self.volumes.remove(&id)
    }

    pub fn volume(&self, id: u64) -> Option<&FluidVolume> {
        self.volumes.get(&id)
    }

    pub fn volume_mut(&mut self, id: u64) -> Result<&mut FluidVolume> {
        self.volumes.get_mut(&id).ok_or(Error::UnknownVolume(id))
    }

    /// Volume ids in ascending order.
    pub fn ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.volumes.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.volumes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.volumes.is_empty()
    }

    /// Step every volume by `dt` against `proxies`. Inputs are validated before any volume
    /// changes.
    pub fn step(&mut self, dt: f32, proxies: &[ColliderProxy]) -> Result<WorldStepReport> {
        validate_dt(dt)?;
        let prepared = proxy::prepare(proxies)?;
        let mut report = WorldStepReport {
            volumes: Vec::with_capacity(self.volumes.len()),
            proxy_impulses: prepared
                .iter()
                .map(|p| ProxyImpulse {
                    id: p.id,
                    linear: glam::Vec3::ZERO,
                    angular: glam::Vec3::ZERO,
                })
                .collect(),
        };
        for (&id, volume) in &mut self.volumes {
            let volume_report = volume.step_prepared(dt, &prepared);
            for (total, part) in report
                .proxy_impulses
                .iter_mut()
                .zip(&volume_report.proxy_impulses)
            {
                total.linear += part.linear;
                total.angular += part.angular;
            }
            report.volumes.push((id, volume_report));
        }
        Ok(report)
    }
}
