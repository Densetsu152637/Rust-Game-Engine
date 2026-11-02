//! Single-dependency facade for the Rust game engine.
//!
//! A game depends on this one crate instead of naming every `engine-*` crate:
//!
//! ```toml
//! [dependencies]
//! engine = { git = "https://github.com/Densetsu152637/Rust-Game-Engine.git", tag = "v0.1.0" }
//! ```
//!
//! This crate contains no engine behaviour. It re-exports the public API of the
//! workspace crates a game uses, so every type comes from the same copy of each crate
//! (one Rust type universe), and forwards the optional Cargo features of those crates.
//! See `docs/using-the-engine.md` in the repository for setup, versioning and
//! engine co-development.
//!
//! # Modules
//!
//! Each workspace crate is a module named after it without the `engine-` prefix
//! (`engine::project` is `engine-project`, `engine::mesh_lod` is `engine-mesh-lod`):
//!
//! | Module | Contents |
//! | --- | --- |
//! | [`log`] | Logging: `ENGINE_LOG` filtering and bounded queryable history. |
//! | [`ecs`] | ECS layer: Bevy ECS re-exports, buffered values, render-world extraction. |
//! | [`core`] | `EngineApp`, stages, fixed ticks, run control and profiling. |
//! | [`project`] | Authored projects, scenes, the component catalog, `Runtime`, native scripts and persistence. |
//! | [`assets`] | Asset import and typed generational asset ownership. |
//! | [`audio`] | PCM clips, the mixer, spatial sources and device output. |
//! | [`acoustics`] | Acoustic world and propagation. |
//! | [`ai`] | Behavior trees, state trees and AI runtimes. |
//! | [`animation`] | Skeletal poses, clips, crossfades and skinning. |
//! | [`destruction`] | Fracture and destruction. |
//! | [`fluids`] | Fluid simulation. |
//! | [`foliage`] | Foliage scattering and instancing. |
//! | [`gameplay`] | Abilities, inventory, quests and dialogue. |
//! | [`graph`] | Shared node-graph document model. |
//! | [`locale`] | Localization with Fluent string tables. |
//! | [`material_graph`] | Material graphs. |
//! | [`mesh_lod`] | Mesh level-of-detail generation. |
//! | [`navigation`] | NavMesh baking, pathfinding and path following. |
//! | [`network`] | Authoritative networked sessions. |
//! | [`net_predict`] | Client-side prediction and reconciliation. |
//! | [`net_quic`] | QUIC transport. |
//! | [`net_replication`] | State replication. |
//! | [`net_voice`] | Networked voice transport. |
//! | [`pcg`] | Procedural content generation. |
//! | [`physics`] | Rapier 2D/3D bodies, contacts, queries and joints. |
//! | [`script`] | Shared script core: host functions, capabilities and budgets. |
//! | [`sequencer`] | Cinematic sequencer. |
//! | [`streaming`] | World streaming. |
//! | [`text`] | Text shaping, layout and glyph atlases. |
//! | [`terrain`] | Heightfield terrain. |
//! | [`tilemap`] | Tilemaps. |
//! | [`video`] | Video decoding and playback. |
//! | [`water`] | Water surfaces. |
//! | [`xpbd`] | Position-based cloth and soft bodies. |
//! | [`desktop`] | winit/wgpu desktop host, headless runner, GPU renderer, render pipeline (feature `desktop`) |
//! | `ml` | Machine-learning inference (not available on `wasm32`) |
//!
//! `engine::bevy_ecs` and `engine::anyhow` are the exact crates the engine uses;
//! derive `Component`/`Resource` through them so derives resolve against the right copy.
//!
//! # Features
//!
//! | Feature | Default | Effect |
//! | --- | --- | --- |
//! | `desktop` | yes | Builds [`desktop`] (window, input, GPU rendering, audio output) |
//! | `text-scripting` | yes | Rhai text scripts (`engine.TextScript`) |
//! | `dlss` | no | NVIDIA DLSS Super Resolution; implies `desktop`, builds without the NGX SDK, which DLSS then reports as missing |
//! | `system-clipboard` | no | OS clipboard for game text inputs; implies `desktop` |
//! | `deterministic-physics` | no | Bit-identical cross-platform physics maths |
//!
//! The desktop host depends on `engine-project` with its default features, so
//! `text-scripting` is effectively on whenever `desktop` is.
//!
//! # Prelude
//!
//! [`prelude`] holds what a typical game crate's `configure` function and native
//! scripts need: the app and ECS types, and the runtime script and event entry points.
//!
//! ```
//! use engine::prelude::*;
//!
//! #[derive(Component)]
//! struct Health(u32);
//!
//! fn configure(app: &mut EngineApp) -> Result<()> {
//!     app.world_mut().spawn(Health(3));
//!     Ok(())
//! }
//!
//! let mut app = EngineApp::new();
//! configure(&mut app).unwrap();
//! ```

pub use engine_acoustics as acoustics;
pub use engine_ai as ai;
pub use engine_animation as animation;
pub use engine_assets as assets;
pub use engine_audio as audio;
pub use engine_core as core;
pub use engine_core::{AppState, EngineApp, Result, RuntimeControl, Stage, Time};
pub use engine_core::{anyhow, bevy_ecs};
pub use engine_destruction as destruction;
pub use engine_ecs as ecs;
pub use engine_fluids as fluids;
pub use engine_foliage as foliage;
pub use engine_gameplay as gameplay;
pub use engine_graph as graph;
pub use engine_locale as locale;
pub use engine_log as log;
pub use engine_material_graph as material_graph;
pub use engine_mesh_lod as mesh_lod;
#[cfg(not(target_arch = "wasm32"))]
pub use engine_ml as ml;
pub use engine_navigation as navigation;
pub use engine_net_predict as net_predict;
pub use engine_net_quic as net_quic;
pub use engine_net_replication as net_replication;
pub use engine_net_voice as net_voice;
pub use engine_network as network;
pub use engine_pcg as pcg;
pub use engine_physics as physics;
pub use engine_project as project;
pub use engine_script as script;
pub use engine_sequencer as sequencer;
pub use engine_streaming as streaming;
pub use engine_terrain as terrain;
pub use engine_text as text;
pub use engine_tilemap as tilemap;
pub use engine_video as video;
pub use engine_water as water;
pub use engine_xpbd as xpbd;

/// The winit/wgpu desktop host: `run`, `RunOptions`, the GPU renderer, input and the
/// render pipeline (`desktop::render_pipeline`, `desktop::wgpu`, `desktop::winit`).
#[cfg(feature = "desktop")]
pub use engine_desktop as desktop;

/// Names a typical game needs in `configure` and native scripts.
pub mod prelude {
    pub use engine_core::bevy_ecs;
    pub use engine_core::prelude::*;
    pub use engine_project::{
        ActionFrame, EngineEvent, NativeScript, Project, Runtime, Transform, register_component,
        register_event_handler, register_script,
    };
}
