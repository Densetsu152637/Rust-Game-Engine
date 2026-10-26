use crate::{CompiledTimeline, Marker};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const MAX_WRAP_PER_ADVANCE: u32 = 64;
const MAX_EVENTS_PER_ADVANCE: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Stopped,
    Playing,
    Paused,
    Finished,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Forward,
    Reverse,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SeekEvents {
    #[default]
    Silent,
    Crossed,
}
#[derive(Clone, Debug, PartialEq)]
pub struct EventCrossing {
    pub marker: Marker,
    pub direction: Direction,
    pub cycle: i64,
}
#[derive(Clone, Debug)]
pub struct Player {
    timeline: Arc<CompiledTimeline>,
    position: f64,
    speed: f64,
    looping: bool,
    state: State,
    cycle: i64,
}
impl Player {
    pub fn new(timeline: Arc<CompiledTimeline>) -> Self {
        Self {
            timeline,
            position: 0.,
            speed: 1.,
            looping: false,
            state: State::Stopped,
            cycle: 0,
        }
    }
    pub fn timeline(&self) -> &CompiledTimeline {
        &self.timeline
    }
    pub fn position(&self) -> f64 {
        self.position
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn speed(&self) -> f64 {
        self.speed
    }
    pub fn cycle(&self) -> i64 {
        self.cycle
    }
    pub fn set_speed(&mut self, speed: f64) -> Result<()> {
        ensure!(
            speed.is_finite() && speed != 0. && speed.abs() <= 100.,
            "speed must be finite, nonzero, and within -100..=100"
        );
        self.speed = speed;
        Ok(())
    }
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }
    /// Play resumes the current position. Use stop/seek to restart deliberately.
    pub fn play(&mut self) {
        self.state = State::Playing;
    }
    pub fn pause(&mut self) {
        if self.state == State::Playing {
            self.state = State::Paused;
        }
    }
    pub fn stop(&mut self) {
        self.state = State::Stopped;
        self.position = 0.;
        self.cycle = 0;
    }
    /// Seek is local to one cycle and never wraps. Silent seek is the default policy.
    pub fn seek(&mut self, position: f64, policy: SeekEvents) -> Result<Vec<EventCrossing>> {
        ensure!(
            position.is_finite() && (0. ..=self.timeline.duration()).contains(&position),
            "seek outside timeline"
        );
        let mut events = Vec::new();
        if policy == SeekEvents::Crossed {
            self.collect(self.position, position, self.cycle, &mut events)?;
        }
        self.position = position;
        if self.state == State::Finished || self.state == State::Stopped {
            self.state = State::Paused;
        }
        Ok(events)
    }
    /// Bounded and atomic: a rejected delta changes neither state nor position nor events.
    pub fn advance(&mut self, delta_seconds: f64) -> Result<Vec<EventCrossing>> {
        ensure!(
            delta_seconds.is_finite() && delta_seconds >= 0.,
            "delta must be finite and nonnegative"
        );
        if self.state != State::Playing || delta_seconds == 0. {
            return Ok(vec![]);
        }
        let signed = delta_seconds * self.speed;
        ensure!(signed.is_finite(), "timeline delta overflow");
        let duration = self.timeline.duration();
        if !self.looping {
            let position = (self.position + signed).clamp(0., duration);
            let mut events = Vec::new();
            self.collect(self.position, position, self.cycle, &mut events)?;
            self.position = position;
            if (signed > 0. && position == duration) || (signed < 0. && position == 0.) {
                self.state = State::Finished;
            }
            return Ok(events);
        }
        ensure!(
            signed.abs() / duration <= f64::from(MAX_WRAP_PER_ADVANCE),
            "timeline advance exceeds 64 cycles"
        );
        let forward = signed > 0.;
        let direction = if forward {
            Direction::Forward
        } else {
            Direction::Reverse
        };
        let mut remaining = signed.abs();
        let mut position = self.position;
        let mut cycle = self.cycle;
        let mut wraps = 0;
        let mut events = Vec::new();
        while remaining > 0. {
            let boundary = if forward { duration } else { 0. };
            let distance = (boundary - position).abs();
            if remaining < distance {
                let next = if forward {
                    position + remaining
                } else {
                    position - remaining
                };
                self.collect(position, next, cycle, &mut events)?;
                position = next;
                remaining = 0.;
            } else {
                self.collect(position, boundary, cycle, &mut events)?;
                remaining -= distance;
                wraps += 1;
                ensure!(
                    wraps <= MAX_WRAP_PER_ADVANCE,
                    "timeline advance exceeds 64 wrap boundaries"
                );
                cycle = if forward {
                    cycle.checked_add(1)
                } else {
                    cycle.checked_sub(1)
                }
                .ok_or_else(|| anyhow::anyhow!("timeline cycle overflow"))?;
                position = if forward { 0. } else { duration };
                // Crossing a seam enters the other endpoint once, including exact landings.
                for marker in &self.timeline.document().events {
                    if f64::from(marker.time) == position {
                        push(&mut events, marker, direction, cycle)?;
                    }
                }
            }
        }
        self.position = position;
        self.cycle = cycle;
        Ok(events)
    }
    fn collect(
        &self,
        from: f64,
        to: f64,
        cycle: i64,
        events: &mut Vec<EventCrossing>,
    ) -> Result<()> {
        if from == to {
            return Ok(());
        }
        let markers = &self.timeline.document().events;
        if to > from {
            for marker in markers {
                let time = f64::from(marker.time);
                if time > from && time <= to {
                    push(events, marker, Direction::Forward, cycle)?;
                }
            }
        } else {
            // Reverse time groups while retaining author order inside equal-time groups.
            let mut end = markers.len();
            while end > 0 {
                let time = markers[end - 1].time;
                let mut start = end - 1;
                while start > 0 && markers[start - 1].time == time {
                    start -= 1;
                }
                if f64::from(time) >= to && f64::from(time) < from {
                    for marker in &markers[start..end] {
                        push(events, marker, Direction::Reverse, cycle)?;
                    }
                }
                end = start;
            }
        }
        Ok(())
    }
}
fn push(
    events: &mut Vec<EventCrossing>,
    marker: &Marker,
    direction: Direction,
    cycle: i64,
) -> Result<()> {
    if events.len() >= MAX_EVENTS_PER_ADVANCE {
        bail!("timeline advance exceeds 4096 events");
    }
    events.push(EventCrossing {
        marker: marker.clone(),
        direction,
        cycle,
    });
    Ok(())
}
