//! Deterministic authored timelines. Scene bindings and their authority belong to the host.
mod clock;
mod timeline;
pub use clock::{Direction, EventCrossing, Player, SeekEvents, State};
pub use timeline::{
    CameraCut, Channel, CompiledTimeline, Key, MAX_BINDINGS, Marker, Property, Sample, Sampling,
    Timeline, Track, Value,
};
