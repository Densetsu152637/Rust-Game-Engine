//! Deterministic swept AABB collision. Touch alone is not overlap; contacts sort by ID.
use crate::{Error, Result};
use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub type Vec2 = [f32; 2];
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Body {
    pub position: Vec2,
    pub size: Vec2,
    pub offset: Vec2,
    pub trigger: bool,
    pub layer: u32,
    pub mask: u32,
}
impl Default for Body {
    fn default() -> Self {
        Self {
            position: [0.0; 2],
            size: [1.0; 2],
            offset: [0.0; 2],
            trigger: false,
            layer: 1,
            mask: u32::MAX,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MoveResult {
    pub position: Vec2,
    pub contacts: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerPhase {
    Enter,
    Stay,
    Exit,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerEvent {
    pub first: String,
    pub second: String,
    pub phase: TriggerPhase,
}
#[derive(Clone, Debug, Default, Resource)]
pub struct World {
    bodies: BTreeMap<String, Body>,
    previous: BTreeSet<(String, String)>,
}
fn valid(v: Vec2) -> bool {
    v.iter().all(|v| v.is_finite() && v.abs() <= 1e6)
}
fn valid_body(b: &Body) -> bool {
    valid(b.position)
        && valid(b.size)
        && valid(b.offset)
        && b.size.iter().all(|v| *v > 0.0)
        && b.layer != 0
        && b.mask != 0
}
fn matches(a: &Body, b: &Body) -> bool {
    a.layer & b.mask != 0 && b.layer & a.mask != 0
}
fn bounds(b: &Body) -> ([f64; 2], [f64; 2]) {
    let mut low = [0.0; 2];
    let mut high = [0.0; 2];
    for axis in 0..2 {
        let center = b.position[axis] as f64 + b.offset[axis] as f64;
        low[axis] = center - b.size[axis] as f64 / 2.0;
        high[axis] = center + b.size[axis] as f64 / 2.0;
    }
    (low, high)
}
fn intersects(a: &Body, b: &Body) -> bool {
    let (a0, a1) = bounds(a);
    let (b0, b1) = bounds(b);
    a0[0] < b1[0] && a1[0] > b0[0] && a0[1] < b1[1] && a1[1] > b0[1]
}
#[derive(Clone, Copy)]
struct Hit {
    time: f64,
    axes: [bool; 2],
}
impl Default for Hit {
    fn default() -> Self {
        Self {
            time: 2.0,
            axes: [false; 2],
        }
    }
}
fn sweep(a: &Body, b: &Body, delta: Vec2) -> Hit {
    let (a0, a1) = bounds(a);
    let (b0, b1) = bounds(b);
    let mut enter = [0.0; 2];
    let mut leave = [0.0; 2];
    for axis in 0..2 {
        if delta[axis] == 0.0 {
            if a1[axis] <= b0[axis] || a0[axis] >= b1[axis] {
                return Hit::default();
            }
            enter[axis] = f64::NEG_INFINITY;
            leave[axis] = f64::INFINITY;
        } else {
            let t1 = (b0[axis] - a1[axis]) / delta[axis] as f64;
            let t2 = (b1[axis] - a0[axis]) / delta[axis] as f64;
            enter[axis] = t1.min(t2);
            leave[axis] = t1.max(t2);
        }
    }
    let time = enter[0].max(enter[1]);
    if !(0.0..=1.0).contains(&time) || time >= leave[0].min(leave[1]) {
        return Hit::default();
    }
    Hit {
        time,
        axes: [enter[0] >= enter[1], enter[1] >= enter[0]],
    }
}
impl World {
    pub const MAXIMUM_BODIES: usize = 1024;
    pub fn upsert(&mut self, id: impl Into<String>, body: Body) -> Result<()> {
        let id = id.into();
        if id.is_empty() || id.len() > 256 || !valid_body(&body) {
            return Err(Error::new(
                "collision.body.invalid",
                "Require ID, finite bounded coordinates, positive size and nonzero layer/mask",
            ));
        }
        if !self.bodies.contains_key(&id) && self.bodies.len() >= Self::MAXIMUM_BODIES {
            return Err(Error::new("collision.capacity", "Body capacity exceeded"));
        }
        self.bodies.insert(id, body);
        Ok(())
    }
    pub fn remove(&mut self, id: &str) -> bool {
        self.bodies.remove(id).is_some()
    }
    pub fn clear(&mut self) {
        self.bodies.clear();
        self.previous.clear();
    }
    pub fn body(&self, id: &str) -> Option<Body> {
        self.bodies.get(id).copied()
    }
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.bodies.keys().map(String::as_str)
    }
    pub fn query(&self, area: Body, ignore: &str) -> Result<Vec<String>> {
        if !valid_body(&area) {
            return Err(Error::new("collision.query.invalid", "Invalid query body"));
        }
        Ok(self
            .bodies
            .iter()
            .filter(|(id, body)| {
                id.as_str() != ignore && matches(&area, body) && intersects(&area, body)
            })
            .map(|(id, _)| id.clone())
            .collect())
    }
    pub fn overlaps(&self, id: &str) -> Result<Vec<String>> {
        self.query(
            self.body(id)
                .ok_or_else(|| Error::new("collision.body.unknown", "Unknown body"))?,
            id,
        )
    }
    pub fn is_safe(&self, body: Body, ignore: &str) -> Result<bool> {
        if !valid_body(&body) {
            return Err(Error::new(
                "collision.body.invalid",
                "Invalid candidate body",
            ));
        }
        Ok(!self
            .bodies
            .iter()
            .any(|(id, b)| id != ignore && !b.trigger && matches(&body, b) && intersects(&body, b)))
    }
    pub fn find_safe(
        &self,
        mut body: Body,
        radius: f32,
        step: f32,
        ignore: &str,
    ) -> Result<Option<Vec2>> {
        if !valid_body(&body)
            || !radius.is_finite()
            || !step.is_finite()
            || !(0.0..=1e6).contains(&radius)
            || step <= 0.0
            || radius / step > 64.0
        {
            return Err(Error::new(
                "collision.search.invalid",
                "Require nonnegative radius, positive step and at most 64 rings",
            ));
        }
        let origin = body.position;
        if self.is_safe(body, ignore)? {
            return Ok(Some(origin));
        }
        let d = std::f64::consts::FRAC_1_SQRT_2;
        for ring in 1..=(radius as f64 / step as f64).ceil() as u32 {
            let distance = (radius as f64).min(ring as f64 * step as f64);
            for direction in [
                [-1.0, 0.0],
                [0.0, -1.0],
                [0.0, 1.0],
                [1.0, 0.0],
                [-d, -d],
                [-d, d],
                [d, -d],
                [d, d],
            ] {
                body.position = [
                    (origin[0] as f64 + distance * direction[0]) as f32,
                    (origin[1] as f64 + distance * direction[1]) as f32,
                ];
                if valid_body(&body) && self.is_safe(body, ignore)? {
                    return Ok(Some(body.position));
                }
            }
        }
        Ok(None)
    }
    pub fn move_body(&mut self, id: &str, mut delta: Vec2) -> Result<MoveResult> {
        let mut candidate = self
            .body(id)
            .ok_or_else(|| Error::new("collision.body.unknown", "Unknown body"))?;
        if !valid(delta) {
            return Err(Error::new("collision.move.invalid", "Invalid displacement"));
        }
        let mut contacts = BTreeSet::new();
        if candidate.trigger {
            for (axis, d) in delta.iter().enumerate() {
                candidate.position[axis] += d;
            }
        } else {
            let mut resolved = false;
            for _ in 0..32 {
                let mut smallest = f64::INFINITY;
                let mut correction = [0.0; 2];
                let mut obstacle = None;
                for (other_id, other) in &self.bodies {
                    if other_id == id
                        || other.trigger
                        || !matches(&candidate, other)
                        || !intersects(&candidate, other)
                    {
                        continue;
                    }
                    let (a0, a1) = bounds(&candidate);
                    let (b0, b1) = bounds(other);
                    for (index, shift) in
                        [b0[0] - a1[0], b1[0] - a0[0], b0[1] - a1[1], b1[1] - a0[1]]
                            .into_iter()
                            .enumerate()
                    {
                        if shift.abs() < smallest {
                            smallest = shift.abs();
                            correction = [0.0; 2];
                            correction[index / 2] = shift as f32;
                            obstacle = Some(other_id.clone());
                        }
                    }
                }
                let Some(obstacle) = obstacle else {
                    resolved = true;
                    break;
                };
                for (axis, c) in correction.iter().enumerate() {
                    candidate.position[axis] += c;
                }
                contacts.insert(obstacle.clone());
                if intersects(&candidate, &self.bodies[&obstacle]) {
                    for (axis, c) in correction.iter().enumerate() {
                        if *c != 0.0 {
                            candidate.position[axis] = if *c < 0.0 {
                                candidate.position[axis].next_down()
                            } else {
                                candidate.position[axis].next_up()
                            };
                        }
                    }
                }
            }
            if !resolved {
                return Err(Error::new(
                    "collision.penetration",
                    "Initial overlap could not be resolved",
                ));
            }
            for _ in 0..3 {
                if delta == [0.0; 2] {
                    break;
                }
                let mut earliest = Hit::default();
                let mut hit_ids = Vec::new();
                for (other_id, other) in &self.bodies {
                    if other_id == id || other.trigger || !matches(&candidate, other) {
                        continue;
                    }
                    let hit = sweep(&candidate, other, delta);
                    if hit.time < earliest.time {
                        earliest = hit;
                        hit_ids = vec![other_id.clone()];
                    } else if hit.time <= 1.0 && hit.time == earliest.time {
                        for axis in 0..2 {
                            earliest.axes[axis] |= hit.axes[axis];
                        }
                        hit_ids.push(other_id.clone());
                    }
                }
                let fraction = earliest.time.min(1.0);
                for (axis, d) in delta.iter().enumerate() {
                    candidate.position[axis] =
                        (candidate.position[axis] as f64 + *d as f64 * fraction) as f32;
                }
                if earliest.time > 1.0 {
                    break;
                }
                contacts.extend(hit_ids.iter().cloned());
                for other_id in &hit_ids {
                    if intersects(&candidate, &self.bodies[other_id]) {
                        for (axis, d) in delta.iter().enumerate() {
                            if earliest.axes[axis] && *d != 0.0 {
                                candidate.position[axis] = if *d > 0.0 {
                                    candidate.position[axis].next_down()
                                } else {
                                    candidate.position[axis].next_up()
                                };
                            }
                        }
                    }
                }
                for (axis, d) in delta.iter_mut().enumerate() {
                    *d = if earliest.axes[axis] {
                        0.0
                    } else {
                        (*d as f64 * (1.0 - fraction)) as f32
                    };
                }
            }
        }
        if !valid_body(&candidate) || (!candidate.trigger && !self.is_safe(candidate, id)?) {
            return Err(Error::new(
                "collision.move.bounds",
                "Movement cannot represent a safe bounded contact",
            ));
        }
        self.bodies.insert(id.to_owned(), candidate);
        Ok(MoveResult {
            position: candidate.position,
            contacts: contacts.into_iter().collect(),
        })
    }
    pub fn advance_triggers(&mut self) -> Vec<TriggerEvent> {
        let bodies: Vec<_> = self.bodies.iter().collect();
        let mut current = BTreeSet::new();
        for (a, (a_id, a_body)) in bodies.iter().enumerate() {
            for (b_id, b_body) in bodies.iter().skip(a + 1) {
                if (a_body.trigger || b_body.trigger)
                    && matches(a_body, b_body)
                    && intersects(a_body, b_body)
                {
                    current.insert(((*a_id).clone(), (*b_id).clone()));
                }
            }
        }
        let events = current
            .union(&self.previous)
            .map(|(first, second)| TriggerEvent {
                first: first.clone(),
                second: second.clone(),
                phase: if current.contains(&(first.clone(), second.clone())) {
                    if self.previous.contains(&(first.clone(), second.clone())) {
                        TriggerPhase::Stay
                    } else {
                        TriggerPhase::Enter
                    }
                } else {
                    TriggerPhase::Exit
                },
            })
            .collect();
        self.previous = current;
        events
    }
}
pub fn follow_velocity(from: Vec2, target: Vec2, speed: f32, separation: f32) -> Result<Vec2> {
    if !valid(from)
        || !valid(target)
        || !speed.is_finite()
        || !(0.0..=1e6).contains(&speed)
        || !separation.is_finite()
        || !(0.0..=1e6).contains(&separation)
    {
        return Err(Error::new(
            "gameplay.follow.invalid",
            "Invalid follow parameters",
        ));
    }
    let x = target[0] as f64 - from[0] as f64;
    let y = target[1] as f64 - from[1] as f64;
    let distance = x.hypot(y);
    if distance <= separation as f64 || distance == 0.0 {
        Ok([0.0; 2])
    } else {
        Ok([
            (x / distance * speed as f64) as f32,
            (y / distance * speed as f64) as f32,
        ])
    }
}
pub fn flock_target(center: Vec2, index: usize, count: usize, radius: f32) -> Result<Vec2> {
    if !valid(center)
        || count == 0
        || count > World::MAXIMUM_BODIES
        || index >= count
        || !radius.is_finite()
        || !(0.0..=1e6).contains(&radius)
    {
        return Err(Error::new(
            "gameplay.flock.invalid",
            "Invalid flock parameters",
        ));
    }
    let angle = std::f64::consts::TAU * index as f64 / count as f64;
    let result = [
        (center[0] as f64 + radius as f64 * angle.cos()) as f32,
        (center[1] as f64 + radius as f64 * angle.sin()) as f32,
    ];
    if !valid(result) {
        return Err(Error::new(
            "gameplay.flock.bounds",
            "Flock target exceeds bounds",
        ));
    }
    Ok(result)
}
