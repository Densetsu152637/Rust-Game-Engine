use crate::{LocalPose, Quat, Vec3};
use anyhow::{Result, ensure};
use std::collections::BTreeSet;
#[derive(Clone, Copy, Debug)]
pub enum Interpolation {
    Step,
    Linear,
}
#[derive(Clone, Debug)]
pub struct VectorTrack {
    times: Vec<f32>,
    values: Vec<Vec3>,
    interpolation: Interpolation,
}
#[derive(Clone, Debug)]
pub struct RotationTrack {
    times: Vec<f32>,
    values: Vec<Quat>,
    interpolation: Interpolation,
}
fn validate_times(times: &[f32], count: usize) -> Result<()> {
    ensure!(
        !times.is_empty() && times.len() <= 1_000_000 && times.len() == count,
        "invalid key count"
    );
    ensure!(
        times.iter().all(|t| t.is_finite() && *t >= 0.) && times.windows(2).all(|p| p[0] < p[1]),
        "keys must have finite increasing times"
    );
    Ok(())
}
fn segment(times: &[f32], time: f32, interpolation: Interpolation) -> (usize, usize, f32) {
    let after = times.partition_point(|t| *t <= time);
    if after == 0 {
        return (0, 0, 0.);
    }
    let before = after - 1;
    if after == times.len() || matches!(interpolation, Interpolation::Step) {
        return (before, before, 0.);
    }
    (
        before,
        after,
        (time - times[before]) / (times[after] - times[before]),
    )
}
impl VectorTrack {
    pub fn new(times: Vec<f32>, values: Vec<Vec3>, interpolation: Interpolation) -> Result<Self> {
        validate_times(&times, values.len())?;
        ensure!(values.iter().all(|v| v.is_finite()), "nonfinite vector key");
        Ok(Self {
            times,
            values,
            interpolation,
        })
    }
    fn sample(&self, time: f32) -> Vec3 {
        let (a, b, t) = segment(&self.times, time, self.interpolation);
        self.values[a].lerp(self.values[b], t)
    }
    fn end(&self) -> f32 {
        self.times[self.times.len() - 1]
    }
}
impl RotationTrack {
    pub fn new(times: Vec<f32>, values: Vec<Quat>, interpolation: Interpolation) -> Result<Self> {
        validate_times(&times, values.len())?;
        ensure!(
            values
                .iter()
                .all(|v| v.is_finite() && (v.length_squared() - 1.).abs() < 0.001),
            "invalid rotation key"
        );
        Ok(Self {
            times,
            values,
            interpolation,
        })
    }
    fn sample(&self, time: f32) -> Quat {
        let (a, b, t) = segment(&self.times, time, self.interpolation);
        self.values[a].slerp(self.values[b], t).normalize()
    }
    fn end(&self) -> f32 {
        self.times[self.times.len() - 1]
    }
}
#[derive(Clone, Debug)]
pub struct Track {
    pub joint: usize,
    pub translation: Option<VectorTrack>,
    pub rotation: Option<RotationTrack>,
    pub scale: Option<VectorTrack>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ClipEvent {
    pub time: f32,
    pub name: String,
}
#[derive(Clone, Debug)]
pub struct Clip {
    duration: f32,
    joints: usize,
    tracks: Vec<Track>,
    events: Vec<ClipEvent>,
}
impl Clip {
    pub fn new(
        duration: f32,
        joints: usize,
        tracks: Vec<Track>,
        mut events: Vec<ClipEvent>,
    ) -> Result<Self> {
        ensure!(
            duration.is_finite()
                && duration > 0.
                && joints > 0
                && joints <= 4096
                && tracks.len() <= joints
                && events.len() <= 4096,
            "invalid clip limits"
        );
        let mut seen = BTreeSet::new();
        for track in &tracks {
            ensure!(
                track.joint < joints && seen.insert(track.joint),
                "invalid or duplicate joint track"
            );
            ensure!(
                track
                    .translation
                    .as_ref()
                    .is_none_or(|t| t.end() <= duration)
                    && track.rotation.as_ref().is_none_or(|t| t.end() <= duration)
                    && track.scale.as_ref().is_none_or(|t| t.end() <= duration),
                "key exceeds clip duration"
            );
        }
        ensure!(
            events.iter().all(|e| e.time.is_finite()
                && e.time >= 0.
                && e.time < duration
                && !e.name.is_empty()
                && e.name.len() <= 256),
            "invalid clip event"
        );
        events.sort_by(|a, b| a.time.total_cmp(&b.time));
        Ok(Self {
            duration,
            joints,
            tracks,
            events,
        })
    }
    pub fn duration(&self) -> f32 {
        self.duration
    }
    pub fn joint_count(&self) -> usize {
        self.joints
    }
    /// Unanimated channels retain the supplied rest pose.
    pub fn sample(&self, time: f32, looping: bool, rest: &[LocalPose]) -> Result<Vec<LocalPose>> {
        ensure!(
            time.is_finite() && time >= 0. && rest.len() == self.joints,
            "invalid sample time or pose"
        );
        let t = if looping {
            time.rem_euclid(self.duration)
        } else {
            time.min(self.duration)
        };
        let mut pose = rest.to_vec();
        for track in &self.tracks {
            let p = &mut pose[track.joint];
            if let Some(v) = &track.translation {
                p.translation = v.sample(t);
            }
            if let Some(v) = &track.rotation {
                p.rotation = v.sample(t);
            }
            if let Some(v) = &track.scale {
                p.scale = v.sample(t);
            }
        }
        for p in &pose {
            p.validate()?;
        }
        Ok(pose)
    }
    /// Events use the interval (from,to]; zero-time events fire on loop crossings.
    pub fn events_between(&self, from: f32, to: f32, looping: bool) -> Result<Vec<ClipEvent>> {
        ensure!(
            from.is_finite() && to.is_finite() && from >= 0. && to >= from,
            "invalid event interval"
        );
        if !looping {
            return Ok(self
                .events
                .iter()
                .filter(|e| e.time > from && e.time <= to.min(self.duration))
                .cloned()
                .collect());
        }
        let start = (f64::from(from) / f64::from(self.duration)).floor();
        let end = (f64::from(to) / f64::from(self.duration)).floor();
        ensure!(
            end - start <= 1024. && end <= u32::MAX as f64,
            "event interval too large"
        );
        let mut result = Vec::new();
        for cycle in start as u32..=end as u32 {
            for event in &self.events {
                let absolute = f64::from(cycle) * f64::from(self.duration) + f64::from(event.time);
                if absolute > f64::from(from) && absolute <= f64::from(to) {
                    ensure!(result.len() < 4096, "event capacity exceeded");
                    result.push(event.clone());
                }
            }
        }
        Ok(result)
    }
}
