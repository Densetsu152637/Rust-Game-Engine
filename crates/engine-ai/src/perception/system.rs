//! The perception update: stimuli in, per-perceiver memory and events out.
//!
//! Every update processes, in this order, for each perceiver (ascending id):
//!
//! 1. **Sight**: each target (ascending id) inside the cone and range is a
//!    candidate. A candidate needs a line-of-sight ray; at most `max_rays` rays
//!    are cast per update, served round-robin over the candidate pairs in
//!    `(perceiver, target)` order, resuming after the last pair served. Pairs not
//!    served keep their previous result (unknown counts as blocked).
//! 2. **Hearing** and **damage**: noises and damage reports delivered this update.
//! 3. **Memory**: stimuli sensed this update refresh their record (age 0); other
//!    records age by `dt`, are marked lost when they were sensed before, and are
//!    forgotten once `age >= max_age`.
//!
//! Nothing here reads a clock or an RNG: equal inputs give equal outputs.
use super::config::{
    ANONYMOUS, Affiliation, AffiliationFilter, Id, Sense, SensorConfig, SightConfig, TeamTable,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Most records one perceiver keeps (one per subject and sense).
pub const MAX_RECORDS: usize = 64;
/// Most events one update reports; the rest are counted as dropped.
pub const MAX_EVENTS: usize = 1024;
/// Tolerance when comparing accumulated seconds with authored durations.
const EPS: f64 = 1e-6;

/// The seam to the game: occlusion rays and hearing attenuation.
pub trait PerceptionHost {
    /// Whether the straight segment `from -> to` is free of occluders.
    /// `perceiver` and `target` identify the pair so hosts can ignore their own
    /// colliders.
    fn line_of_sight(&mut self, perceiver: Id, target: Id, from: [f32; 3], to: [f32; 3]) -> bool;

    /// Multiplier in `[0, 1]` applied to a noise's loudness for one listener. The
    /// default hears every noise unchanged; an acoustics-aware host attenuates by
    /// occlusion here. `source` is `None` for anonymous noises.
    fn hearing_attenuation(
        &mut self,
        listener: Id,
        listener_position: [f32; 3],
        source: Option<Id>,
        source_position: [f32; 3],
    ) -> f32 {
        let _ = (listener, listener_position, source, source_position);
        1.0
    }
}

/// One perceiver this update.
#[derive(Clone, Debug)]
pub struct PerceiverInput {
    pub id: Id,
    pub position: [f32; 3],
    /// World-space forward direction (any non-zero length).
    pub forward: [f32; 3],
    pub config: SensorConfig,
}

/// Something sight can see.
#[derive(Clone, Copy, Debug)]
pub struct TargetInput {
    pub id: Id,
    pub position: [f32; 3],
    pub team: u32,
    /// Height of the point sight aims at above `position` (world +Y).
    pub height_offset: f32,
}

/// A noise made this update.
#[derive(Clone, Copy, Debug)]
pub struct Noise {
    pub source: Option<Id>,
    pub position: [f32; 3],
    /// Nominal loudness in `[0, 1]` (larger values are clamped by the strength).
    pub loudness: f32,
    /// Distance at which the noise fades to nothing.
    pub range: f32,
    pub team: u32,
}

/// Damage taken this update.
#[derive(Clone, Copy, Debug)]
pub struct Damage {
    pub victim: Id,
    pub instigator: Option<Id>,
    /// Where the instigator was (the victim learns it).
    pub position: [f32; 3],
    pub amount: f32,
    /// Team of the instigator.
    pub team: u32,
}

/// Everything one update consumes.
#[derive(Clone, Debug, Default)]
pub struct UpdateInput {
    pub perceivers: Vec<PerceiverInput>,
    pub targets: Vec<TargetInput>,
    pub noises: Vec<Noise>,
    pub damages: Vec<Damage>,
    /// Sight rays allowed this update.
    pub max_rays: usize,
}

/// What happened to a memory record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A stimulus was sensed (new, or again after being lost or remembered).
    Sensed,
    /// Sight lost a target that stays remembered.
    Lost,
    /// A remembered stimulus expired.
    Forgotten,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sensed => "sensed",
            Self::Lost => "lost",
            Self::Forgotten => "forgotten",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PerceptionEvent {
    pub kind: EventKind,
    pub perceiver: Id,
    pub subject: Id,
    pub sense: Sense,
    pub position: [f32; 3],
    pub strength: f32,
}

/// What one update did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpdateReport {
    pub events: Vec<PerceptionEvent>,
    pub rays_cast: usize,
    /// Candidate pairs that waited for a later update.
    pub rays_deferred: usize,
    pub dropped_events: usize,
}

/// A memory record as seen from outside.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Known {
    pub subject: Id,
    pub sense: Sense,
    pub affiliation: Affiliation,
    pub position: [f32; 3],
    /// Strength at the last sensing, scaled down linearly with age.
    pub strength: f32,
    /// Sensed during the latest update.
    pub sensed: bool,
    /// Seconds since last sensed.
    pub age: f32,
}

#[derive(Clone, Copy, Debug)]
struct Record {
    position: [f32; 3],
    strength: f32,
    age: f64,
    sensed: bool,
    affiliation: Affiliation,
    max_age: f32,
}

impl Record {
    fn current_strength(&self) -> f32 {
        if self.sensed || self.max_age <= 0.0 {
            self.strength
        } else {
            (self.strength * (1.0 - (self.age / f64::from(self.max_age)).min(1.0) as f32)).max(0.0)
        }
    }
    fn known(&self, subject: Id, sense: Sense) -> Known {
        Known {
            subject,
            sense,
            affiliation: self.affiliation,
            position: self.position,
            strength: self.current_strength(),
            sensed: self.sensed,
            age: self.age as f32,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Memory {
    records: BTreeMap<(Id, Sense), Record>,
    /// Last line-of-sight result per target.
    los: BTreeMap<Id, bool>,
}

#[derive(Clone, Copy, Debug)]
struct Observation {
    position: [f32; 3],
    strength: f32,
    affiliation: Affiliation,
}

/// Per-perceiver stimulus memory plus the update that feeds it.
#[derive(Clone, Debug, Default)]
pub struct Perception {
    pub teams: TeamTable,
    memories: BTreeMap<Id, Memory>,
    last_served: Option<(Id, Id)>,
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

/// Whether `direction` (from the eye to the target) is within `limit` of `forward`.
fn within(sight_limit: f32, angle_limit: f32, to_target: [f32; 3], forward: [f32; 3]) -> bool {
    let distance = length(to_target);
    if distance > sight_limit + 1e-4 {
        return false;
    }
    let forward_length = length(forward);
    if distance < 1e-6 || forward_length < 1e-9 {
        return true;
    }
    let cos = (dot(to_target, forward) / (distance * forward_length)).clamp(-1.0, 1.0);
    cos.acos().to_degrees() <= angle_limit + 1e-3
}

impl Perception {
    pub fn new(teams: TeamTable) -> Self {
        Self {
            teams,
            ..Self::default()
        }
    }

    /// Perceivers that have a memory.
    pub fn perceiver_count(&self) -> usize {
        self.memories.len()
    }

    /// Every record of `perceiver`, ordered by `(subject, sense)`.
    pub fn known(&self, perceiver: Id) -> Vec<Known> {
        self.memories.get(&perceiver).map_or_else(Vec::new, |m| {
            m.records
                .iter()
                .map(|((subject, sense), r)| r.known(*subject, *sense))
                .collect()
        })
    }

    pub fn record_count(&self, perceiver: Id) -> usize {
        self.memories.get(&perceiver).map_or(0, |m| m.records.len())
    }

    /// The most relevant record among affiliations in `filter`: currently sensed
    /// first, then strongest, then freshest, then lowest subject id and sense.
    pub fn best(&self, perceiver: Id, filter: AffiliationFilter) -> Option<Known> {
        let memory = self.memories.get(&perceiver)?;
        let mut best: Option<Known> = None;
        for ((subject, sense), record) in &memory.records {
            if !filter.contains(record.affiliation) {
                continue;
            }
            let candidate = record.known(*subject, *sense);
            let better = match &best {
                None => true,
                Some(b) => {
                    (
                        candidate.sensed,
                        candidate.strength,
                        std::cmp::Reverse(candidate.age.to_bits()),
                    ) > (b.sensed, b.strength, std::cmp::Reverse(b.age.to_bits()))
                }
            };
            // Equal keys keep the earlier (lower subject, lower sense) record.
            if better {
                best = Some(candidate);
            }
        }
        best
    }

    /// Drops the memory of a perceiver that no longer exists.
    pub fn remove(&mut self, perceiver: Id) {
        self.memories.remove(&perceiver);
    }

    /// Runs one update of `dt` seconds.
    pub fn update(
        &mut self,
        dt: f32,
        input: &UpdateInput,
        host: &mut dyn PerceptionHost,
    ) -> UpdateReport {
        let mut report = UpdateReport::default();
        let mut perceivers: Vec<&PerceiverInput> = input.perceivers.iter().collect();
        perceivers.sort_by_key(|p| p.id);
        perceivers.dedup_by_key(|p| p.id);
        let mut targets: Vec<&TargetInput> = input.targets.iter().collect();
        targets.sort_by_key(|t| t.id);
        targets.dedup_by_key(|t| t.id);
        let live: BTreeSet<Id> = perceivers.iter().map(|p| p.id).collect();
        self.memories.retain(|id, _| live.contains(id));

        let rays = self.schedule_rays(&perceivers, &targets, input.max_rays, &mut report);
        let mut observations: BTreeMap<Id, BTreeMap<(Id, Sense), Observation>> = BTreeMap::new();
        for perceiver in &perceivers {
            let memory = self.memories.entry(perceiver.id).or_default();
            let mut seen = BTreeMap::new();
            if let Some(sight) = &perceiver.config.sight {
                for target in &targets {
                    if target.id == perceiver.id {
                        continue;
                    }
                    let affiliation = self.teams.affiliation(perceiver.config.team, target.team);
                    if !sight.affiliations.contains(affiliation) {
                        continue;
                    }
                    let key = (perceiver.id, target.id);
                    if !candidate(sight, perceiver, target, memory) {
                        memory.los.remove(&target.id);
                        continue;
                    }
                    if rays.contains(&key) {
                        let (from, to) = rays_endpoints(sight, perceiver, target);
                        let clear = host.line_of_sight(perceiver.id, target.id, from, to);
                        memory.los.insert(target.id, clear);
                    }
                    if memory.los.get(&target.id).copied().unwrap_or(false) {
                        seen.insert(
                            (target.id, Sense::Sight),
                            Observation {
                                position: target.position,
                                strength: 1.0,
                                affiliation,
                            },
                        );
                    }
                }
                let ids: BTreeSet<Id> = targets.iter().map(|t| t.id).collect();
                memory.los.retain(|id, _| ids.contains(id));
            }
            if let Some(hearing) = &perceiver.config.hearing {
                for noise in &input.noises {
                    if noise.source == Some(perceiver.id) {
                        continue;
                    }
                    let affiliation = self.teams.affiliation(perceiver.config.team, noise.team);
                    if !hearing.affiliations.contains(affiliation) {
                        continue;
                    }
                    let distance = length(sub(noise.position, perceiver.position));
                    let reach = noise.range.min(hearing.range);
                    if reach.is_nan() || reach <= 0.0 || distance > reach {
                        continue;
                    }
                    let attenuation = host
                        .hearing_attenuation(
                            perceiver.id,
                            perceiver.position,
                            noise.source,
                            noise.position,
                        )
                        .clamp(0.0, 1.0);
                    let strength =
                        (noise.loudness * attenuation * (1.0 - distance / reach)).clamp(0.0, 1.0);
                    if strength < hearing.threshold || strength <= 0.0 {
                        continue;
                    }
                    let subject = noise.source.unwrap_or(ANONYMOUS);
                    let entry = seen
                        .entry((subject, Sense::Hearing))
                        .or_insert(Observation {
                            position: noise.position,
                            strength: 0.0,
                            affiliation,
                        });
                    if strength > entry.strength {
                        *entry = Observation {
                            position: noise.position,
                            strength,
                            affiliation,
                        };
                    }
                }
            }
            if let Some(damage) = &perceiver.config.damage {
                for hit in input.damages.iter().filter(|d| d.victim == perceiver.id) {
                    let affiliation = self.teams.affiliation(perceiver.config.team, hit.team);
                    if !damage.affiliations.contains(affiliation) {
                        continue;
                    }
                    let subject = hit.instigator.unwrap_or(ANONYMOUS);
                    seen.insert(
                        (subject, Sense::Damage),
                        Observation {
                            position: hit.position,
                            strength: 1.0,
                            affiliation,
                        },
                    );
                }
            }
            observations.insert(perceiver.id, seen);
        }
        for perceiver in &perceivers {
            let seen = observations.remove(&perceiver.id).unwrap_or_default();
            let memory = self.memories.entry(perceiver.id).or_default();
            merge(memory, perceiver, &seen, dt, &mut report);
        }
        let dropped = report.events.len().saturating_sub(MAX_EVENTS);
        report.events.truncate(MAX_EVENTS);
        report.dropped_events += dropped;
        report
    }

    /// Picks the candidate pairs that get a ray this update.
    fn schedule_rays(
        &mut self,
        perceivers: &[&PerceiverInput],
        targets: &[&TargetInput],
        max_rays: usize,
        report: &mut UpdateReport,
    ) -> BTreeSet<(Id, Id)> {
        let mut pairs: Vec<(Id, Id)> = Vec::new();
        for perceiver in perceivers {
            let Some(sight) = &perceiver.config.sight else {
                continue;
            };
            let memory = self.memories.get(&perceiver.id);
            for target in targets {
                if target.id == perceiver.id {
                    continue;
                }
                let affiliation = self.teams.affiliation(perceiver.config.team, target.team);
                if !sight.affiliations.contains(affiliation) {
                    continue;
                }
                let empty = Memory::default();
                if candidate(sight, perceiver, target, memory.unwrap_or(&empty)) {
                    pairs.push((perceiver.id, target.id));
                }
            }
        }
        let start = match self.last_served {
            Some(last) => pairs.partition_point(|p| *p <= last),
            None => 0,
        };
        let served: BTreeSet<(Id, Id)> = pairs[start..]
            .iter()
            .chain(&pairs[..start])
            .take(max_rays)
            .copied()
            .collect();
        report.rays_cast = served.len();
        report.rays_deferred = pairs.len() - served.len();
        self.last_served = if report.rays_deferred > 0 {
            // Resume after the last pair served in rotation order.
            pairs[start..]
                .iter()
                .chain(&pairs[..start])
                .take(max_rays)
                .last()
                .copied()
        } else {
            None
        };
        served
    }
}

fn eye(sight: &SightConfig, perceiver: &PerceiverInput) -> [f32; 3] {
    let p = perceiver.position;
    [p[0], p[1] + sight.height_offset, p[2]]
}

fn rays_endpoints(
    sight: &SightConfig,
    perceiver: &PerceiverInput,
    target: &TargetInput,
) -> ([f32; 3], [f32; 3]) {
    let t = target.position;
    (
        eye(sight, perceiver),
        [t[0], t[1] + target.height_offset, t[2]],
    )
}

/// Whether the target is inside the acquire (or, when sensed, the retain) cone.
fn candidate(
    sight: &SightConfig,
    perceiver: &PerceiverInput,
    target: &TargetInput,
    memory: &Memory,
) -> bool {
    let known = memory
        .records
        .get(&(target.id, Sense::Sight))
        .is_some_and(|r| r.sensed);
    let (range, angle) = if known {
        (sight.lose_range, sight.peripheral_half_angle)
    } else {
        (sight.range, sight.half_angle)
    };
    let (from, to) = rays_endpoints(sight, perceiver, target);
    within(range, angle, sub(to, from), perceiver.forward)
}

fn merge(
    memory: &mut Memory,
    perceiver: &PerceiverInput,
    seen: &BTreeMap<(Id, Sense), Observation>,
    dt: f32,
    report: &mut UpdateReport,
) {
    let config = &perceiver.config;
    let keys: Vec<(Id, Sense)> = memory.records.keys().copied().collect();
    for key in keys {
        let Some(max_age) = config.max_age(key.1) else {
            memory.records.remove(&key);
            continue;
        };
        let record = memory.records.get_mut(&key).expect("key came from the map");
        record.max_age = max_age;
        if let Some(observation) = seen.get(&key) {
            let regained = !record.sensed;
            record.position = observation.position;
            record.strength = observation.strength;
            record.affiliation = observation.affiliation;
            record.age = 0.0;
            record.sensed = true;
            if regained {
                report
                    .events
                    .push(event(EventKind::Sensed, perceiver.id, key, record));
            }
            continue;
        }
        if record.sensed {
            record.sensed = false;
            if key.1 == Sense::Sight {
                report
                    .events
                    .push(event(EventKind::Lost, perceiver.id, key, record));
            }
        }
        record.age += f64::from(dt);
        if record.age + EPS >= f64::from(max_age) {
            report
                .events
                .push(event(EventKind::Forgotten, perceiver.id, key, record));
            memory.records.remove(&key);
        }
    }
    for (key, observation) in seen {
        if memory.records.contains_key(key) {
            continue;
        }
        let Some(max_age) = config.max_age(key.1) else {
            continue;
        };
        if memory.records.len() >= MAX_RECORDS && !evict(memory, perceiver.id, report) {
            continue;
        }
        let record = Record {
            position: observation.position,
            strength: observation.strength,
            age: 0.0,
            sensed: true,
            affiliation: observation.affiliation,
            max_age,
        };
        report
            .events
            .push(event(EventKind::Sensed, perceiver.id, *key, &record));
        memory.records.insert(*key, record);
    }
}

/// Frees one slot: the weakest remembered record (never one sensed now).
fn evict(memory: &mut Memory, perceiver: Id, report: &mut UpdateReport) -> bool {
    let victim = memory
        .records
        .iter()
        .filter(|(_, r)| !r.sensed)
        .min_by(|(ka, a), (kb, b)| {
            a.current_strength()
                .total_cmp(&b.current_strength())
                .then(b.age.total_cmp(&a.age))
                .then(kb.cmp(ka))
        })
        .map(|(k, _)| *k);
    match victim {
        Some(key) => {
            let record = memory.records.remove(&key).expect("victim exists");
            report
                .events
                .push(event(EventKind::Forgotten, perceiver, key, &record));
            true
        }
        None => false,
    }
}

fn event(kind: EventKind, perceiver: Id, key: (Id, Sense), record: &Record) -> PerceptionEvent {
    PerceptionEvent {
        kind,
        perceiver,
        subject: key.0,
        sense: key.1,
        position: record.position,
        strength: record.current_strength(),
    }
}
