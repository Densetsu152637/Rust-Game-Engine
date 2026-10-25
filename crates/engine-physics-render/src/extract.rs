//! Extraction of the physics render data into the render world.
//!
//! The render features read the *extracted* world (`FeatureContext::world`), which only holds
//! components registered with `EngineApp::extract_component`. The physics render data lives in
//! simulation-world resources (`PhysicsRenderData`, the destruction state, the physics service
//! behind the debug view), so [`extract_physics_render`] copies what the features need into a
//! [`PhysicsRenderExtract`] resource of the render world during the `Extract` stage.
//! [`install_physics_extraction`] adds it to an `EngineApp`.

use crate::config::PhysicsRenderConfig;
use bevy_ecs::prelude::*;
use engine_core::{EngineApp, ExtractionSet, RenderWorld, Stage};
use engine_project::Transform;
use engine_project::simulation::physics::{
    DebugSegment, Fractured, FragmentRender, PhysicsRenderData, debug_view,
    deformable_debug_segments, fragment_mesh, fragments,
};
use glam::{Mat4, Quat, Vec3};
use std::sync::Arc;

/// Whether the debug segments are available this frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DebugState {
    /// `debug_view` produced segments.
    On,
    /// No segments, with the engine's reason (for example `engine.PhysicsSettings.debug` is
    /// off or there is no physics service).
    #[default]
    Off,
    /// Extraction was disabled by [`PhysicsRenderConfig::enabled`].
    Disabled,
}

/// One live destruction fragment to draw.
#[derive(Clone, Debug)]
pub struct FragmentDraw {
    pub owner: Entity,
    pub fragment: Entity,
    pub world_from_local: Mat4,
    pub mesh: Arc<FragmentRender>,
}

/// Physics render data copied into the render world.
#[derive(Resource, Clone, Debug, Default)]
pub struct PhysicsRenderExtract {
    /// Changes whenever any content changed; features skip re-uploads when it is unchanged.
    pub revision: u64,
    pub data: PhysicsRenderData,
    /// Debug-view and deformable debug segments, at most `max_debug_segments`.
    pub debug: Vec<DebugSegment>,
    pub debug_truncated: bool,
    pub debug_state: DebugState,
    /// Why `debug` is empty when `debug_state` is not `On`.
    pub debug_reason: Option<String>,
    pub fragments: Vec<FragmentDraw>,
    pub fragments_truncated: bool,
    /// `Fractured` entities (the simulation world's ids) whose intact mesh should not draw.
    pub fractured: Vec<Entity>,
}

/// Builds the extract from the simulation world. `previous` is the last extract; unchanged
/// physics data is moved from it instead of cloned again.
pub fn build_extract(
    world: &mut World,
    config: &PhysicsRenderConfig,
    previous: Option<PhysicsRenderExtract>,
) -> PhysicsRenderExtract {
    let mut previous = previous;
    let enabled = config.enabled;
    let mut out = PhysicsRenderExtract::default();
    let wants_data = enabled.cloth || enabled.soft_bodies || enabled.fluid;
    let mut data_changed = false;
    if wants_data {
        match world.get_resource::<PhysicsRenderData>() {
            Some(source) => {
                if let Some(last) = previous
                    .as_mut()
                    .filter(|p| p.data.frame == source.frame && p.data.frame != 0)
                {
                    out.data = std::mem::take(&mut last.data);
                } else {
                    out.data = source.clone();
                    data_changed = true;
                }
                if !enabled.cloth {
                    out.data.cloths.clear();
                }
                if !enabled.soft_bodies {
                    out.data.soft_bodies.clear();
                }
                if !enabled.fluid {
                    out.data.fluids.clear();
                }
            }
            None => data_changed = previous.as_ref().is_some_and(|p| p.data.frame != 0),
        }
    }
    if enabled.debug_lines {
        match debug_view(world) {
            Ok(view) => {
                let mut lines = view.lines(config.max_debug_segments);
                let mut truncated = lines.truncated;
                let room = config.max_debug_segments - lines.segments.len();
                match deformable_debug_segments(world, room) {
                    Ok(extra) => {
                        truncated |= extra.truncated;
                        lines.segments.extend(extra.segments);
                    }
                    Err(error) => out.debug_reason = Some(error.to_string()),
                }
                out.debug = lines.segments;
                out.debug_truncated = truncated;
                out.debug_state = DebugState::On;
            }
            Err(error) => {
                out.debug_state = DebugState::Off;
                out.debug_reason = Some(error.to_string());
            }
        }
    } else {
        out.debug_state = DebugState::Disabled;
    }
    if enabled.fragments {
        let owners: Vec<Entity> = world
            .query_filtered::<Entity, With<Fractured>>()
            .iter(world)
            .collect();
        out.fractured = owners.clone();
        'owners: for owner in owners {
            for info in fragments(world, owner) {
                if out.fragments.len() >= config.max_fragments {
                    out.fragments_truncated = true;
                    break 'owners;
                }
                let (Some(mesh), Some(transform)) = (
                    fragment_mesh(world, info.entity),
                    world.get::<Transform>(info.entity),
                ) else {
                    continue;
                };
                out.fragments.push(FragmentDraw {
                    owner,
                    fragment: info.entity,
                    world_from_local: transform_matrix(transform),
                    mesh,
                });
            }
        }
    }
    let animated = !out.debug.is_empty() || !out.fragments.is_empty();
    let base = previous.as_ref().map_or(0, |p| p.revision);
    out.revision = if data_changed || animated || previous.is_none() {
        base + 1
    } else {
        base
    };
    out
}

/// World matrix of an authored transform.
pub fn transform_matrix(transform: &Transform) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::from(transform.scale),
        Quat::from_array(transform.rotation).normalize(),
        Vec3::from(transform.position),
    )
}

/// The `Extract`-stage system: reads [`PhysicsRenderConfig`] (defaults when absent) and stores
/// the extract in the render world.
pub fn extract_physics_render(world: &mut World) {
    let config = world
        .get_resource::<PhysicsRenderConfig>()
        .cloned()
        .unwrap_or_default();
    let previous = world
        .resource_mut::<RenderWorld>()
        .world_mut()
        .remove_resource::<PhysicsRenderExtract>();
    let extract = build_extract(world, &config, previous);
    world
        .resource_mut::<RenderWorld>()
        .world_mut()
        .insert_resource(extract);
}

/// Validates `config`, inserts it as a resource of the simulation world and schedules
/// [`extract_physics_render`] in the `Extract` stage. Call once while configuring the app.
pub fn install_physics_extraction(
    app: &mut EngineApp,
    config: PhysicsRenderConfig,
) -> anyhow::Result<()> {
    config.validate()?;
    app.world_mut().insert_resource(config);
    app.schedule_mut(Stage::Extract).add_systems(
        extract_physics_render
            .after(ExtractionSet::Cleanup)
            .ambiguous_with_all(),
    );
    Ok(())
}
