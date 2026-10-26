//! Capture-owned translation frames; authored curves remain f32 and origin arithmetic is f64.
use super::{Result, invalid};
use bevy_ecs::prelude::Entity;
use engine_sequencer::Value;
use std::collections::BTreeMap;

pub(crate) type RootRebases = BTreeMap<Entity, ([f32; 3], [f32; 3])>;

pub(super) struct RootTranslation {
    key_origin: [f64; 3],
    current_origin: [f64; 3],
    baseline_absolute: [f64; 3],
    conflicted: bool,
}
impl RootTranslation {
    pub fn new(key_origin: [f64; 3], current_origin: [f64; 3], baseline: [f32; 3]) -> Result<Self> {
        let baseline_absolute = std::array::from_fn(|i| current_origin[i] + f64::from(baseline[i]));
        checked_local(baseline_absolute)?;
        Ok(Self {
            key_origin,
            current_origin,
            baseline_absolute,
            conflicted: false,
        })
    }
    pub fn sample(&self, value: [f32; 3]) -> Result<[f32; 3]> {
        checked_local(std::array::from_fn(|i| {
            f64::from(value[i]) + (self.key_origin[i] - self.current_origin[i])
        }))
    }
    pub fn valid(&self) -> bool {
        !self.conflicted
    }
    pub fn invalidate(&mut self) {
        self.conflicted = true;
    }
    /// Actual before/after values preserve recognition through multiple unobserved f32 shifts.
    /// A write that happened before a shift is never reclassified as a legitimate rebase.
    pub fn shifted(
        &mut self,
        from: [f64; 3],
        to: [f64; 3],
        actual: ([f32; 3], [f32; 3]),
        expected: Value,
    ) -> Option<(Value, Value)> {
        if self.current_origin != from
            || expected != Value::Vector(actual.0)
            || actual.1.iter().any(|v| !v.is_finite())
        {
            self.conflicted = true;
        }
        self.current_origin = to;
        let baseline =
            match checked_local(std::array::from_fn(|i| self.baseline_absolute[i] - to[i])) {
                Ok(value) => value,
                Err(_) => {
                    self.conflicted = true;
                    return None;
                }
            };
        if self.conflicted {
            return None;
        }
        Some((Value::Vector(baseline), Value::Vector(actual.1)))
    }
}
fn checked_local(value: [f64; 3]) -> Result<[f32; 3]> {
    if value
        .iter()
        .any(|v| !v.is_finite() || v.abs() > f64::from(f32::MAX))
    {
        return Err(invalid(
            "Sequence translation is not representable in the current origin frame",
        ));
    }
    Ok(value.map(|v| v as f32))
}
