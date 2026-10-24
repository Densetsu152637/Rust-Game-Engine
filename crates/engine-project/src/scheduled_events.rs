//! Deterministic event inputs for bounded runs. IDs resolve in the active scene
//! immediately before the requested zero-based tick, never during file parsing.
use crate::{EngineEvent, Error, EventLimits, Result, Runtime, emit_events};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs::File, io::Read, path::Path};

pub const MAX_SCHEDULE_BYTES: usize = 1024 * 1024;
pub const MAX_SCHEDULE_EVENTS: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScheduledEvent {
    pub tick: u64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub payload: Value,
}
/// Ordered JSON array. Equal-tick entries retain their authored FIFO order.
/// Constructors reject out-of-order ticks rather than silently reordering input.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(transparent)]
pub struct EventSchedule(Vec<ScheduledEvent>);
impl EventSchedule {
    pub fn new(events: Vec<ScheduledEvent>) -> Result<Self> {
        if events.len() > MAX_SCHEDULE_EVENTS {
            return Err(invalid("Schedule exceeds 4096 events"));
        }
        let mut tick = None;
        let mut per_tick = 0;
        for event in &events {
            if tick.is_some_and(|previous| event.tick < previous) {
                return Err(invalid("Schedule ticks must be nondecreasing"));
            }
            if tick == Some(event.tick) {
                per_tick += 1;
            } else {
                per_tick = 1;
            }
            tick = Some(event.tick);
            if per_tick > EventLimits::default().queued {
                return Err(invalid("Schedule exceeds 1024 events in one tick"));
            }
            let valid = |name: &str, max| {
                !name.is_empty() && name.len() <= max && !name.chars().any(char::is_control)
            };
            if !valid(&event.name, 128)
                || [&event.source, &event.target]
                    .into_iter()
                    .flatten()
                    .any(|id| !valid(id, 256))
            {
                return Err(invalid("Invalid event name or stable entity ID"));
            }
            check_size(&event.payload, EventLimits::default().payload_bytes)?;
        }
        check_size(&events, MAX_SCHEDULE_BYTES)?;
        Ok(Self(events))
    }
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_SCHEDULE_BYTES {
            return Err(invalid("Schedule exceeds 1 MiB"));
        }
        Self::new(serde_json::from_slice(bytes)?)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_SCHEDULE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        Self::from_json(&bytes).map_err(|error| error.at(path, "$"))
    }
    pub fn validate_run(&self, ticks: u64) -> Result<()> {
        if self.0.last().is_some_and(|event| event.tick >= ticks) {
            return Err(invalid("Scheduled event tick is outside bounded run"));
        }
        Ok(())
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn entries(&self) -> &[ScheduledEvent] {
        &self.0
    }
    pub fn at_tick(&self, tick: u64) -> &[ScheduledEvent] {
        let start = self.0.partition_point(|event| event.tick < tick);
        let end = self.0.partition_point(|event| event.tick <= tick);
        &self.0[start..end]
    }
}
fn invalid(message: &str) -> Error {
    Error::new("event.schedule.invalid", message)
}
fn check_size(value: &impl Serialize, limit: usize) -> Result<usize> {
    struct LimitWriter(usize);
    impl std::io::Write for LimitWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.0 {
                return Err(std::io::Error::other("event JSON size limit exceeded"));
            }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = LimitWriter(limit);
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| invalid("Event JSON size limit exceeded"))?;
    Ok(limit - writer.0)
}
impl Runtime {
    /// Enqueue one complete scheduled tick before calling `tick`. Resolution and
    /// capacity checks are atomic with respect to enqueueing. This is deterministic
    /// input to this runtime, not a protocol for controlling a separate live process.
    pub fn enqueue_scheduled_events(&mut self, schedule: &EventSchedule, tick: u64) -> Result<()> {
        let mut events = Vec::new();
        for entry in schedule.at_tick(tick) {
            let resolve = |id: &Option<String>| -> Result<_> {
                id.as_ref()
                    .map(|id| {
                        self.entity(id).ok_or_else(|| {
                            Error::new(
                                "event.schedule.entity_missing",
                                format!(
                                    "Tick {tick}: entity {id:?} is absent from the active scene"
                                ),
                            )
                        })
                    })
                    .transpose()
            };
            events.push(EngineEvent {
                name: entry.name.clone(),
                source: resolve(&entry.source)?,
                target: resolve(&entry.target)?,
                payload: entry.payload.clone(),
            });
        }
        emit_events(self.app_mut().world_mut(), events)
    }
}

/// Bounded chronological history of actual deliveries, including script/collision
/// events. After either cap is reached, collection stops but the saturating total
/// count continues. Serialized record bytes are capped at 1 MiB and 4096 records.
#[derive(Default, Serialize)]
pub struct EventTrace {
    pub events: Vec<ScheduledEvent>,
    pub count: u64,
    pub truncated: bool,
    #[serde(skip)]
    bytes: usize,
}
impl EventTrace {
    pub fn capture(&mut self, runtime: &Runtime, tick: u64) {
        #[derive(Serialize)]
        struct Borrowed<'a> {
            tick: u64,
            #[serde(flatten)]
            event: &'a crate::events::EventRecord,
        }
        for event in runtime.delivered_events() {
            self.count = self.count.saturating_add(1);
            if self.truncated {
                continue;
            }
            if self.events.len() >= MAX_SCHEDULE_EVENTS {
                self.truncated = true;
                continue;
            }
            match check_size(
                &Borrowed { tick, event },
                MAX_SCHEDULE_BYTES.saturating_sub(self.bytes),
            ) {
                Ok(size) => {
                    self.bytes += size;
                    self.events.push(ScheduledEvent {
                        tick,
                        name: event.name.clone(),
                        source: event.source.clone(),
                        target: event.target.clone(),
                        payload: event.payload.clone(),
                    });
                }
                Err(_) => self.truncated = true,
            }
        }
    }
}
impl Runtime {
    /// Stable-ID delivery records for the most recently executed tick. Borrowing
    /// avoids copying large payloads when a host's trace has already reached its cap.
    pub fn delivered_events(&self) -> &[crate::events::EventRecord] {
        crate::events::delivered_records(self.app().world())
    }
}
