//! Validated procedural DSP graphs using the shared authored graph document.
mod compile;
mod runtime;
mod schema;
mod stream;
pub use compile::{CompiledGraph, compile};
pub use runtime::{GraphInstance, ParameterUpdate};
pub use schema::kind;
pub use stream::{ProceduralState, ProceduralStatus, ProceduralStream};
pub const MAX_NODES: usize = 256;
pub const MAX_PARAMETERS: usize = 64;
pub const MAX_RENDER_FRAMES: usize = 4096;
pub const MAX_DELAY_FRAMES: usize = 1_048_576;
pub const MAX_SIGNAL: f32 = 1_000_000.;
