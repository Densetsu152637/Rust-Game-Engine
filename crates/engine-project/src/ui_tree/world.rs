use super::*;
use crate::{
    EngineEvent,
    content::{FontAtlas, Texture},
    emit_event,
    runtime::{AuthoredId, Draw},
};
use bevy_ecs::prelude::{Entity, World};
use std::{collections::BTreeMap, sync::Arc};

pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<UiRoot>(app, "engine.UiRoot", 1)?;
    app.world_mut().insert_resource(GameUi::default());
    Ok(())
}

fn roots(world: &World) -> Vec<(String, Entity, UiRoot)> {
    let mut roots: Vec<_> = world
        .iter_entities()
        .filter_map(|e| {
            let root = e.get::<UiRoot>()?;
            let key = e
                .get::<AuthoredId>()
                .map(|a| a.id.clone())
                .unwrap_or_else(|| format!("entity:{}", e.id().to_bits()));
            Some((key, e.id(), root.clone()))
        })
        .collect();
    roots.sort_by(|a, b| (a.2.layer, &a.0).cmp(&(b.2.layer, &b.0)));
    roots
}

/// Called once per simulation tick with UiModel::gameplay_frame after the modal
/// panel update. Queued events are delivered by the runtime's event dispatcher.
pub fn tick_world(
    world: &mut World,
    ctx: &UiTickContext<'_>,
    input: &crate::ActionFrame,
) -> Result<()> {
    let sources = roots(world);
    let mut roots: Vec<_> = sources
        .iter()
        .map(|(id, _, root)| (id.clone(), root.clone()))
        .collect();
    let mut state = world.remove_resource::<GameUi>().unwrap_or_default();
    let result = state.tick_with(&mut roots, input, ctx);
    world.insert_resource(state);
    let events = result?;
    for ((_, entity, previous), (_, root)) in sources.iter().zip(roots) {
        if &root != previous {
            world.entity_mut(*entity).insert(root);
        }
    }
    for event in events {
        let source = sources
            .iter()
            .find(|(id, _, _)| id == &event.root)
            .map(|(_, entity, _)| *entity);
        emit_event(
            world,
            EngineEvent {
                name: event.name,
                source,
                target: source,
                payload: serde_json::json!({"node":event.node,"action":event.action,"value":event.value}),
            },
        )?;
    }
    Ok(())
}

pub fn extract_world(
    world: &World,
    viewport: [u32; 2],
    fonts: &BTreeMap<String, FontAtlas>,
    textures: &BTreeMap<String, Arc<Texture>>,
) -> Result<Vec<Draw>> {
    let roots = roots(world);
    if roots.len() > MAX_UI_ROOTS {
        return Err(invalid("UI exceeds 64 roots"));
    }
    let ui = world.get_resource::<GameUi>();
    let text = BitmapTextBackend::new(fonts, textures);
    let mut draws = vec![];
    let mut ids = std::collections::BTreeSet::new();
    for (id, _, root) in roots {
        if !ids.insert(id.clone()) {
            return Err(invalid("Duplicate UI root ID"));
        }
        let state = ui.map(|ui| ui.render_state(&id)).unwrap_or_default();
        draws.extend(root.extract_draws_with(&id, viewport, &state, &text, textures)?);
    }
    Ok(draws)
}
