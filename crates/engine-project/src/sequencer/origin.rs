//! Capture-owned translation frames; authored curves remain f32 and origin arithmetic is f64.
use super::{Result, invalid};
use crate::world::coordinates::{self, AbsolutePosition, TranslationSnapshot};
use bevy_ecs::prelude::{Entity, World};
use engine_sequencer::Value;
use std::collections::BTreeMap;

pub(crate) struct RootRebase {
    pub before: [f32; 3],
    pub after: [f32; 3],
    pub absolute_before: Option<AbsolutePosition>,
    pub absolute_after: Option<AbsolutePosition>,
}
pub(crate) type RootRebases = BTreeMap<Entity, RootRebase>;

pub(super) struct RootSample {
    pub local: [f32; 3],
    pub absolute: Option<[f64; 3]>,
    pub residual: [f64; 3],
}
#[derive(Clone, Copy)]
enum Baseline {
    Exact {
        position: [f64; 3],
        residual: [f64; 3],
    },
    Local {
        position: [f32; 3],
        origin: [f64; 3],
    },
}
pub(super) struct RootTranslation {
    key_origin: [f64; 3],
    current_origin: [f64; 3],
    baseline: Baseline,
    absolute_component: Option<AbsolutePosition>,
    conflicted: bool,
}
impl RootTranslation {
    pub fn new(
        key_origin: [f64; 3],
        current_origin: [f64; 3],
        baseline: TranslationSnapshot,
    ) -> Result<Self> {
        let result = Self {
            key_origin,
            current_origin,
            baseline: baseline.exact.map_or(
                Baseline::Local {
                    position: baseline.local,
                    origin: current_origin,
                },
                |position| Baseline::Exact {
                    position,
                    residual: baseline.residual,
                },
            ),
            absolute_component: baseline.component,
            conflicted: false,
        };
        result.baseline_sample()?;
        Ok(result)
    }
    fn local_sample(&self, value: [f32; 3], origin: [f64; 3]) -> Result<RootSample> {
        // Subtract origins before adding the f32 value. Adding a tiny local key to a
        // large absolute origin first would lose representable local detail in f64.
        let local = checked_local(std::array::from_fn(|i| {
            f64::from(value[i]) + (origin[i] - self.current_origin[i])
        }))?;
        let (absolute, residual) = if self.absolute_component.is_some() {
            let absolute: [f64; 3] = std::array::from_fn(|i| origin[i] + f64::from(value[i]));
            AbsolutePosition::new(absolute).check()?;
            // Keep the low local detail lost by the absolute addition, rather than
            // re-deriving it from the newly rounded f32 projection after every shift.
            let residual = std::array::from_fn(|i| f64::from(value[i]) - (absolute[i] - origin[i]));
            (Some(absolute), residual)
        } else {
            (None, [0.; 3])
        };
        Ok(RootSample {
            local,
            absolute,
            residual,
        })
    }
    fn baseline_sample(&self) -> Result<RootSample> {
        match self.baseline {
            Baseline::Local { position, origin } => self.local_sample(position, origin),
            Baseline::Exact {
                position: absolute,
                residual,
            } => {
                if self.absolute_component.is_some() {
                    AbsolutePosition::new(absolute).check()?;
                }
                let local = checked_local(std::array::from_fn(|i| {
                    absolute[i] - self.current_origin[i] + residual[i]
                }))?;
                Ok(RootSample {
                    local,
                    absolute: self.absolute_component.as_ref().map(|_| absolute),
                    residual,
                })
            }
        }
    }
    pub fn sample(&self, value: [f32; 3]) -> Result<RootSample> {
        self.local_sample(value, self.key_origin)
    }
    pub fn write(&mut self, world: &mut World, target: Entity, sample: RootSample) -> Result<()> {
        self.absolute_component = coordinates::write_captured_translation(
            world,
            target,
            &self.absolute_component,
            sample.local,
            sample.absolute,
            sample.residual,
        )?;
        Ok(())
    }
    pub fn restore(&mut self, world: &mut World, target: Entity) -> Result<()> {
        self.write(world, target, self.baseline_sample()?)
    }
    pub fn component_owned(&self, world: &World, target: Entity) -> bool {
        world.get::<AbsolutePosition>(target) == self.absolute_component.as_ref()
    }
    pub fn valid(&self) -> bool {
        !self.conflicted
    }
    pub fn invalidate(&mut self) {
        self.conflicted = true;
    }
    /// Accept only coordinates changes to both fields, never a foreign component write.
    pub fn shifted(
        &mut self,
        from: [f64; 3],
        to: [f64; 3],
        actual: &RootRebase,
        expected: Value,
    ) -> Option<(Value, Value)> {
        if self.current_origin != from
            || expected != Value::Vector(actual.before)
            || self.absolute_component != actual.absolute_before
            || actual.after.iter().any(|v| !v.is_finite())
        {
            self.conflicted = true;
        }
        self.current_origin = to;
        let baseline = match self.baseline_sample() {
            Ok(sample) => sample.local,
            Err(_) => {
                self.conflicted = true;
                return None;
            }
        };
        if self.conflicted {
            return None;
        }
        self.absolute_component = actual.absolute_after.clone();
        Some((Value::Vector(baseline), Value::Vector(actual.after)))
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
