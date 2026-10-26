//! Environment queries (EQS-like): generate candidate points or actors around a
//! querier, filter and score them with tests, and select the best.
//!
//! * [`template`]: the authored [`QueryTemplate`] (generator, tests, selection).
//! * [`run`]: [`QueryRun`], the deterministic time-sliced executor, and the
//!   [`QueryHost`] seam (navmesh, line of sight, actors, smart object slots).
//!
//! See `docs/ai-environment-queries.md`.
pub mod run;
pub mod template;

pub use run::{Item, Progress, QueryContext, QueryError, QueryHost, QueryResult, QueryRun, Scored};
pub use template::{
    ActorSource, Compare, Direction, Facing, ForwardTag, Generator, MAX_ITEMS, MAX_RESULTS,
    MAX_TESTS, Named, QueryTemplate, Ref, Select, Test,
};
