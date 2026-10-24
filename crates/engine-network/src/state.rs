use crate::{Error, Limits, Result};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntitySnapshot {
    pub id: String,
    pub translation: [f32; 3],
    /// Unit quaternion, xyzw order.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub velocity: [f32; 3],
}
impl EntitySnapshot {
    pub fn identity(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            velocity: [0.0; 3],
        }
    }
}

/// Full authoritative entity set. Missing IDs are despawned immediately on apply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub tick: u64,
    pub entities: Vec<EntitySnapshot>,
}
impl Snapshot {
    pub(crate) fn validate(&self, limits: &Limits) -> Result<()> {
        if self.entities.len() > limits.max_entities {
            return Err(Error::Invalid("too many entities"));
        }
        let mut ids = BTreeSet::new();
        for entity in &self.entities {
            if !limits.valid_id(&entity.id) || !ids.insert(&entity.id) {
                return Err(Error::Invalid("duplicate or invalid entity ID"));
            }
            if entity
                .translation
                .iter()
                .chain(&entity.rotation)
                .chain(&entity.scale)
                .chain(&entity.velocity)
                .any(|v| !v.is_finite() || v.abs() > 1.0e12)
                || (Quat::from_array(entity.rotation).length_squared() - 1.0).abs() > 0.001
            {
                return Err(Error::Invalid(
                    "non-finite transform or non-unit quaternion",
                ));
            }
        }
        Ok(())
    }
}

/// Two validated snapshots; interpolation never extrapolates or invents despawned entities.
#[derive(Default)]
pub struct ReplicationState {
    limits: Limits,
    previous: Option<Snapshot>,
    current: Option<Snapshot>,
}
impl ReplicationState {
    pub fn new(limits: Limits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            limits,
            previous: None,
            current: None,
        })
    }
    pub fn latest(&self) -> Option<&Snapshot> {
        self.current.as_ref()
    }
    pub fn apply(&mut self, snapshot: Snapshot) -> Result<()> {
        snapshot.validate(&self.limits)?;
        if self
            .current
            .as_ref()
            .is_some_and(|old| snapshot.tick <= old.tick)
        {
            return Err(Error::Invalid("snapshot tick must increase"));
        }
        self.previous = self.current.replace(snapshot);
        Ok(())
    }
    /// Fractional simulation tick, normally one snapshot interval behind the server.
    pub fn sample(&self, render_tick: f64) -> Option<Snapshot> {
        let current = self.current.as_ref()?;
        let Some(previous) = &self.previous else {
            return Some(current.clone());
        };
        let alpha = if render_tick.is_finite() {
            ((render_tick - previous.tick as f64) / (current.tick - previous.tick) as f64)
                .clamp(0.0, 1.0) as f32
        } else {
            1.0
        };
        let previous_entities: BTreeMap<_, _> = previous
            .entities
            .iter()
            .map(|e| (e.id.as_str(), e))
            .collect();
        let entities = current
            .entities
            .iter()
            .map(|new| {
                let Some(old) = previous_entities.get(new.id.as_str()) else {
                    return new.clone();
                };
                let mut result = new.clone();
                result.translation = Vec3::from_array(old.translation)
                    .lerp(Vec3::from_array(new.translation), alpha)
                    .to_array();
                result.scale = Vec3::from_array(old.scale)
                    .lerp(Vec3::from_array(new.scale), alpha)
                    .to_array();
                result.velocity = Vec3::from_array(old.velocity)
                    .lerp(Vec3::from_array(new.velocity), alpha)
                    .to_array();
                result.rotation = Quat::from_array(old.rotation)
                    .slerp(Quat::from_array(new.rotation), alpha)
                    .normalize()
                    .to_array();
                result
            })
            .collect();
        Some(Snapshot {
            tick: current.tick,
            entities,
        })
    }
}
