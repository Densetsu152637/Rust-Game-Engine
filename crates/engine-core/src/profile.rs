//! Bounded CPU wall-clock measurements. No clock values enter simulation state.

use crate::Stage;
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{collections::VecDeque, time::Duration};

pub const MAX_PROFILE_CAPACITY: usize = 10_000;

#[derive(Debug, Clone, Serialize)]
pub struct StageTiming {
    pub stage: Stage,
    pub duration_ns: u64,
    pub succeeded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameProfile {
    pub tick: u64,
    pub duration_ns: u64,
    /// All live ECS entities, including Bevy's internal entities.
    pub simulation_entities: u32,
    /// All live entities in the render world, including internal entities.
    pub render_entities: u32,
    pub succeeded: bool,
    pub stages: Vec<StageTiming>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageProfile {
    pub stage: Stage,
    pub calls: u64,
    pub failures: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub last_ns: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileSnapshot {
    pub schema: u32,
    pub capacity: usize,
    pub frames_recorded: u64,
    pub frames_dropped: u64,
    pub failed_frames: u64,
    /// Oldest to newest. Startup and standalone shutdown are in stage aggregates.
    pub frames: Vec<FrameProfile>,
    pub stages: Vec<StageProfile>,
}

pub(crate) struct Profiler {
    capacity: usize,
    frames_recorded: u64,
    frames_dropped: u64,
    failed_frames: u64,
    frames: VecDeque<FrameProfile>,
    current: Option<FrameProfile>,
    stages: [StageProfile; 5],
}

impl Default for Profiler {
    fn default() -> Self {
        Self {
            capacity: 120,
            frames_recorded: 0,
            frames_dropped: 0,
            failed_frames: 0,
            frames: VecDeque::new(),
            current: None,
            stages: [
                Stage::Startup,
                Stage::Update,
                Stage::Extract,
                Stage::Render,
                Stage::Shutdown,
            ]
            .map(|stage| StageProfile {
                stage,
                calls: 0,
                failures: 0,
                total_ns: 0,
                max_ns: 0,
                last_ns: 0,
            }),
        }
    }
}

impl Profiler {
    pub(crate) fn set_capacity(&mut self, capacity: usize) -> Result<()> {
        ensure!(
            capacity <= MAX_PROFILE_CAPACITY,
            "profile capacity exceeds {MAX_PROFILE_CAPACITY}"
        );
        self.capacity = capacity;
        while self.frames.len() > capacity {
            self.frames.pop_front();
            self.frames_dropped = self.frames_dropped.saturating_add(1);
        }
        self.frames.shrink_to_fit();
        Ok(())
    }

    pub(crate) fn begin_frame(&mut self, tick: u64) {
        self.current = Some(FrameProfile {
            tick,
            duration_ns: 0,
            simulation_entities: 0,
            render_entities: 0,
            succeeded: true,
            stages: Vec::with_capacity(4),
        });
    }

    pub(crate) fn record_stage(&mut self, stage: Stage, elapsed: Duration, succeeded: bool) {
        let duration_ns = nanoseconds(elapsed);
        let aggregate = self
            .stages
            .iter_mut()
            .find(|entry| entry.stage == stage)
            .expect("all stages exist");
        aggregate.calls = aggregate.calls.saturating_add(1);
        aggregate.failures = aggregate.failures.saturating_add(u64::from(!succeeded));
        aggregate.total_ns = aggregate.total_ns.saturating_add(duration_ns);
        aggregate.max_ns = aggregate.max_ns.max(duration_ns);
        aggregate.last_ns = duration_ns;
        if let Some(frame) = &mut self.current {
            frame.stages.push(StageTiming {
                stage,
                duration_ns,
                succeeded,
            });
        }
    }

    pub(crate) fn end_frame(
        &mut self,
        elapsed: Duration,
        simulation_entities: u32,
        render_entities: u32,
        succeeded: bool,
    ) {
        let mut frame = self
            .current
            .take()
            .expect("frame started before completion");
        frame.duration_ns = nanoseconds(elapsed);
        frame.simulation_entities = simulation_entities;
        frame.render_entities = render_entities;
        frame.succeeded = succeeded;
        self.frames_recorded = self.frames_recorded.saturating_add(1);
        self.failed_frames = self.failed_frames.saturating_add(u64::from(!succeeded));
        if self.capacity == 0 {
            self.frames_dropped = self.frames_dropped.saturating_add(1);
            return;
        }
        if self.frames.len() == self.capacity {
            self.frames.pop_front();
            self.frames_dropped = self.frames_dropped.saturating_add(1);
        }
        self.frames.push_back(frame);
    }

    pub(crate) fn snapshot(&self) -> ProfileSnapshot {
        ProfileSnapshot {
            schema: 1,
            capacity: self.capacity,
            frames_recorded: self.frames_recorded,
            frames_dropped: self.frames_dropped,
            failed_frames: self.failed_frames,
            frames: self.frames.iter().cloned().collect(),
            stages: self.stages.to_vec(),
        }
    }
}

fn nanoseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}
