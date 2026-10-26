use anyhow::{Result, ensure};
use engine_animation::{Clip, Interpolation, LocalPose, RotationTrack, VectorTrack};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_BINDINGS: usize = 128;
const MAX_TRACKS: usize = 256;
const MAX_KEYS: usize = 4096;
const MAX_MARKERS: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sampling {
    Step,
    #[default]
    Linear,
}
impl From<Sampling> for Interpolation {
    fn from(value: Sampling) -> Self {
        match value {
            Sampling::Step => Self::Step,
            Sampling::Linear => Self::Linear,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Property {
    Translation,
    Rotation,
    Scale,
    CameraFov,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key<T> {
    pub time: f32,
    pub value: T,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel<T> {
    pub keys: Vec<Key<T>>,
    #[serde(default)]
    pub sampling: Sampling,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "property", rename_all = "snake_case", deny_unknown_fields)]
pub enum Track {
    Translation {
        binding: usize,
        channel: Channel<[f32; 3]>,
    },
    Rotation {
        binding: usize,
        channel: Channel<[f32; 4]>,
    },
    Scale {
        binding: usize,
        channel: Channel<[f32; 3]>,
    },
    CameraFov {
        binding: usize,
        channel: Channel<f32>,
    },
}
impl Track {
    pub fn target(&self) -> (usize, Property) {
        match self {
            Self::Translation { binding, .. } => (*binding, Property::Translation),
            Self::Rotation { binding, .. } => (*binding, Property::Rotation),
            Self::Scale { binding, .. } => (*binding, Property::Scale),
            Self::CameraFov { binding, .. } => (*binding, Property::CameraFov),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraCut {
    pub time: f32,
    pub binding: usize,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    pub time: f32,
    pub id: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    pub version: u32,
    pub duration: f32,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub camera_cuts: Vec<CameraCut>,
    #[serde(default)]
    pub events: Vec<Marker>,
}
impl Default for Timeline {
    fn default() -> Self {
        Self {
            version: 1,
            duration: 1.,
            tracks: vec![],
            camera_cuts: vec![],
            events: vec![],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Vector([f32; 3]),
    Rotation([f32; 4]),
    Scalar(f32),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub binding: usize,
    pub property: Property,
    pub value: Value,
}
#[derive(Clone, Debug)]
pub struct CompiledTimeline {
    document: Timeline,
    clip: Clip,
    bindings: usize,
}
fn keys<T>(channel: &Channel<T>, duration: f32, total: &mut usize) -> Result<()> {
    *total += channel.keys.len();
    ensure!(
        !channel.keys.is_empty() && *total <= MAX_KEYS,
        "invalid timeline key count"
    );
    ensure!(
        channel
            .keys
            .iter()
            .all(|k| k.time.is_finite() && (0. ..=duration).contains(&k.time))
            && channel.keys.windows(2).all(|p| p[0].time < p[1].time),
        "keys must have finite, strictly increasing in-range times"
    );
    Ok(())
}
fn vector(channel: &Channel<[f32; 3]>) -> Result<VectorTrack> {
    VectorTrack::new(
        channel.keys.iter().map(|k| k.time).collect(),
        channel.keys.iter().map(|k| Vec3::from(k.value)).collect(),
        channel.sampling.into(),
    )
}
impl Timeline {
    pub fn compile(&self, bindings: usize) -> Result<CompiledTimeline> {
        ensure!(
            self.version == 1
                && self.duration.is_finite()
                && self.duration > 0.
                && self.duration <= 1_000_000.,
            "invalid timeline version/duration"
        );
        ensure!(
            (1..=MAX_BINDINGS).contains(&bindings)
                && self.tracks.len() <= MAX_TRACKS
                && self.events.len() <= MAX_MARKERS
                && self.camera_cuts.len() <= MAX_MARKERS,
            "timeline capacity exceeded"
        );
        let mut seen = BTreeSet::new();
        let mut channels = BTreeMap::<usize, engine_animation::Track>::new();
        let mut total = 0;
        for track in &self.tracks {
            let (binding, property) = track.target();
            ensure!(
                binding < bindings && seen.insert((binding, property)),
                "invalid/duplicate timeline target"
            );
            let pose = channels
                .entry(binding)
                .or_insert_with(|| engine_animation::Track {
                    joint: binding,
                    translation: None,
                    rotation: None,
                    scale: None,
                });
            match track {
                Track::Translation { channel, .. } => {
                    keys(channel, self.duration, &mut total)?;
                    ensure!(
                        channel.keys.iter().all(|k| k
                            .value
                            .iter()
                            .all(|v| v.is_finite() && v.abs() <= 1_000_000_000.)),
                        "translation key outside finite world limits"
                    );
                    pose.translation = Some(vector(channel)?);
                }
                Track::Scale { channel, .. } => {
                    keys(channel, self.duration, &mut total)?;
                    ensure!(
                        channel.keys.iter().all(|k| k
                            .value
                            .iter()
                            .all(|v| v.is_finite() && *v > 1e-6 && *v <= 1_000_000.)),
                        "scale keys must be positive finite values"
                    );
                    pose.scale = Some(vector(channel)?);
                }
                Track::Rotation { channel, .. } => {
                    keys(channel, self.duration, &mut total)?;
                    pose.rotation = Some(RotationTrack::new(
                        channel.keys.iter().map(|k| k.time).collect(),
                        channel
                            .keys
                            .iter()
                            .map(|k| Quat::from_array(k.value))
                            .collect(),
                        channel.sampling.into(),
                    )?);
                }
                Track::CameraFov { channel, .. } => {
                    keys(channel, self.duration, &mut total)?;
                    ensure!(
                        channel.keys.iter().all(|k| k.value.is_finite()
                            && k.value > 0.01
                            && k.value < std::f32::consts::PI - 0.01),
                        "invalid perspective FOV key"
                    );
                }
            }
        }
        ensure!(
            self.camera_cuts.iter().all(|c| c.time.is_finite()
                && (0. ..=self.duration).contains(&c.time)
                && c.binding < bindings)
                && self.camera_cuts.windows(2).all(|p| p[0].time < p[1].time),
            "camera cuts must have increasing in-range times and valid bindings"
        );
        ensure!(
            self.events.iter().all(|e| e.time.is_finite()
                && (0. ..=self.duration).contains(&e.time)
                && !e.id.is_empty()
                && e.id.len() <= 128
                && !e.id.chars().any(char::is_control)),
            "invalid timeline marker"
        );
        let mut document = self.clone();
        // Stable sort preserves author order for simultaneous events in both directions.
        document.events.sort_by(|a, b| a.time.total_cmp(&b.time));
        let clip = Clip::new(
            self.duration,
            bindings,
            channels.into_values().collect(),
            vec![],
        )?;
        Ok(CompiledTimeline {
            document,
            clip,
            bindings,
        })
    }
}
impl CompiledTimeline {
    pub fn document(&self) -> &Timeline {
        &self.document
    }
    pub fn duration(&self) -> f64 {
        f64::from(self.document.duration)
    }
    pub fn bindings(&self) -> usize {
        self.bindings
    }
    pub fn sample(&self, time: f64) -> Result<Vec<Sample>> {
        ensure!(
            time.is_finite() && time >= 0. && time <= self.duration(),
            "invalid timeline sample position"
        );
        let time = time as f32;
        let poses = self
            .clip
            .sample(time, false, &vec![LocalPose::default(); self.bindings])?;
        self.document
            .tracks
            .iter()
            .map(|track| {
                let (binding, property) = track.target();
                let pose = &poses[binding];
                let value = match track {
                    Track::Translation { .. } => Value::Vector(pose.translation.to_array()),
                    Track::Rotation { .. } => Value::Rotation(pose.rotation.to_array()),
                    Track::Scale { .. } => Value::Vector(pose.scale.to_array()),
                    Track::CameraFov { channel, .. } => Value::Scalar(scalar(channel, time)),
                };
                Ok(Sample {
                    binding,
                    property,
                    value,
                })
            })
            .collect()
    }
    /// Cuts are sampled state, so reverse playback and silent seeks select correctly.
    pub fn camera_at(&self, time: f64) -> Option<usize> {
        self.document
            .camera_cuts
            .iter()
            .rev()
            .find(|c| f64::from(c.time) <= time)
            .map(|c| c.binding)
    }
}
fn scalar(channel: &Channel<f32>, time: f32) -> f32 {
    let after = channel.keys.partition_point(|k| k.time <= time);
    let before = after.saturating_sub(1);
    let a = &channel.keys[before];
    if after == 0 || after == channel.keys.len() || channel.sampling == Sampling::Step {
        return a.value;
    }
    let b = &channel.keys[after];
    a.value + (b.value - a.value) * ((time - a.time) / (b.time - a.time))
}
