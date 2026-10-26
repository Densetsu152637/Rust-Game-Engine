//! Acoustic propagation and authored audio for the runtime.
//!
//! Authored components (`engine.AcousticEnvironment`, `AcousticMaterial`,
//! `AcousticGeometry`, `AcousticVolume`, `AcousticSource`, `AudioEmitter`,
//! `AudioListener`), world extraction from meshes, 2D colliders and volumes, the
//! lifecycle of the propagation service, `acoustics.*` events and status, and the
//! native hooks. See `docs/acoustics.md` and `docs/spatial-audio-design.md`.
use crate::{Error, Project, Result, Scene};

mod air;
mod analysis;
mod collect;
mod components;
mod descriptors;
mod service;
mod volumes;

pub use air::{air_np_per_m, iso9613_db_per_m};
pub use analysis::{
    MAX_BAKED_DOCUMENT_BYTES, MAX_CONTRIBUTORS, MAX_FINDINGS, MAX_QUERY_REFLECTIONS,
    MAX_QUERY_REVERB_RAYS, MAX_QUERY_SOURCES, QueryListener, QueryQuality, QueryRequest,
    QuerySource, SceneAcoustics, bake_scene, inspect_scene, load_scene, query_scene,
    validate_document,
};
pub use collect::{
    BakeInclude, Collected, FootprintShape, FootprintSpec, InstanceSpec, MeshLookup, ProxySpec,
    VolumeGeometry, VolumeSpec, WorldBuild, build_world, collect, find_environment, to_document,
};
pub use components::*;
pub use descriptors::descriptors;
pub use service::{
    AcousticControl, AcousticHooks, AcousticHost, AcousticOutputRate, AcousticUnits,
    EVENT_BACKEND_CHANGED, EVENT_FAILED, EVENT_OCCLUSION_CHANGED, EVENT_WORLD_REBUILT,
    MAX_EVENTS_PER_TICK, OCCLUDED_BELOW, Occlusion, REBUILD_DEBOUNCE_S, VISIBLE_ABOVE,
    mixer_environment, propagation_settings, set_bus_effect, set_param_hook, status, warnings,
};
pub(crate) use service::{shutdown, start, tick};
pub use volumes::{Room, RoomSet, rooms};

pub(crate) fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}

/// Register the native adapters of every component in this module.
pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<AcousticEnvironment>(app, ENVIRONMENT, 1)?;
    crate::register_component::<AcousticMaterialSpec>(app, MATERIAL, 1)?;
    crate::register_component::<AcousticGeometry>(app, GEOMETRY, 1)?;
    crate::register_component::<AcousticVolume>(app, VOLUME, 1)?;
    crate::register_component::<AcousticSource>(app, SOURCE, 1)?;
    crate::register_component::<AudioEmitter>(app, EMITTER, 1)?;
    crate::register_component::<AudioListener>(app, LISTENER, 1)
}

/// Scene-level checks: at most one environment, and asset references of the
/// right kind (`audio` clips, `mesh` geometry, `acoustics` baked worlds).
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    let mut environments = 0;
    for entity in &scene.entities {
        for (name, value) in &entity.components.custom {
            let reference = |pointer: &str, kind: &str| -> Result<()> {
                match value.data.pointer(pointer).and_then(|v| v.as_str()) {
                    Some(id) => project.require_asset(id, kind).map(|_| ()),
                    None => Ok(()),
                }
            };
            match name.as_str() {
                ENVIRONMENT => {
                    environments += 1;
                    reference("/geometry/asset", "acoustics")?;
                }
                EMITTER => {
                    if value
                        .data
                        .get("clip")
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| !s.is_empty())
                    {
                        reference("/clip", "audio")?;
                    }
                    reference("/graph", "graph")?;
                }
                GEOMETRY | VOLUME => reference("/mesh", "mesh")?,
                _ => {}
            }
        }
    }
    if environments > 1 {
        return Err(Error::new(
            "acoustics.environment.multiple",
            "At most one engine.AcousticEnvironment per scene",
        ));
    }
    Ok(())
}
