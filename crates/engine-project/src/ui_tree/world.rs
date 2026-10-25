use super::{
    resolve::{NodeStates, effective_for_render, states_by_root},
    stylesheet::SharedTheme,
    *,
};
use crate::{
    EngineEvent,
    content::{FontAtlas, Texture},
    emit_event,
    runtime::{AuthoredId, Draw},
};
use bevy_ecs::{
    prelude::{Entity, World},
    world::EntityRef,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<UiRoot>(app, "engine.UiRoot", 1)?;
    crate::register_component::<UiTheme>(app, "engine.UiTheme", 1)?;
    crate::register_component::<UiData>(app, "engine.UiData", 1)?;
    crate::register_component::<UiWorldPanel>(app, "engine.UiWorldPanel", 1)?;
    let world = app.world_mut();
    world.insert_resource(GameUi::default());
    world.insert_resource(UiStyleSettings::default());
    world.insert_resource(UiStyleRuntime::default());
    world.insert_resource(UiDataContext::default());
    world.insert_resource(UiBindingState::default());
    world.insert_resource(UiWorldPanels::default());
    Ok(())
}

/// Forget motion, binding and world-panel interaction state (scene change,
/// shutdown). The `UiDataContext` and the settings are application state and
/// are kept.
pub fn reset_world(world: &mut World) {
    if let Some(mut style) = world.get_resource_mut::<UiStyleRuntime>() {
        style.reset();
    }
    if let Some(mut bindings) = world.get_resource_mut::<UiBindingState>() {
        bindings.reset();
    }
    if let Some(mut panels) = world.get_resource_mut::<UiWorldPanels>() {
        panels.clear();
    }
}

/// Stable key of a root entity: its authored ID, else a per-entity key.
pub(super) fn root_key(entity: &EntityRef) -> String {
    entity
        .get::<AuthoredId>()
        .map(|a| a.id.clone())
        .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()))
}

/// Screen-space roots; entities with a `UiWorldPanel` are drawn in the world.
fn roots(world: &World) -> Vec<(String, Entity, UiRoot)> {
    let mut roots: Vec<_> = world
        .iter_entities()
        .filter_map(|e| {
            let root = e.get::<UiRoot>()?;
            if e.get::<UiWorldPanel>().is_some() {
                return None;
            }
            Some((root_key(&e), e.id(), root.clone()))
        })
        .collect();
    roots.sort_by(|a, b| (a.2.layer, &a.0).cmp(&(b.2.layer, &b.0)));
    roots
}

/// The UI resources, taken out of the world for one tick.
#[derive(Default)]
pub(super) struct TickParts {
    pub ui: GameUi,
    pub style: UiStyleRuntime,
    pub bindings: UiBindingState,
    pub data: UiDataContext,
    pub settings: UiStyleSettings,
    pub panels: UiWorldPanels,
}

impl TickParts {
    fn take(world: &mut World) -> Self {
        Self {
            ui: world.remove_resource().unwrap_or_default(),
            style: world.remove_resource().unwrap_or_default(),
            bindings: world.remove_resource().unwrap_or_default(),
            data: world.remove_resource().unwrap_or_default(),
            settings: world.remove_resource().unwrap_or_default(),
            panels: world.remove_resource().unwrap_or_default(),
        }
    }

    fn put(self, world: &mut World) {
        world.insert_resource(self.ui);
        world.insert_resource(self.style);
        world.insert_resource(self.bindings);
        world.insert_resource(self.data);
        world.insert_resource(self.settings);
        world.insert_resource(self.panels);
    }
}

/// Called once per simulation tick with UiModel::gameplay_frame after the modal
/// panel update. Queued events are delivered by the runtime's event dispatcher.
///
/// Per tick: `engine.UiData` components merge into the data context; one-way
/// bindings write changed values into the roots; the style step resolves each
/// themed root into an effective root (cascade, state flags from the previous
/// tick, motion) that interaction and layout run on; widget changes made by
/// interaction are copied back; two-way bindings write them into the data;
/// finally transitions and animations advance by `ctx.dt`.
pub fn tick_world(
    world: &mut World,
    ctx: &UiTickContext<'_>,
    input: &crate::ActionFrame,
) -> Result<()> {
    let mut parts = TickParts::take(world);
    let result = tick_all(world, &mut parts, ctx, input);
    parts.put(world);
    result
}

fn tick_all(
    world: &mut World,
    parts: &mut TickParts,
    ctx: &UiTickContext<'_>,
    input: &crate::ActionFrame,
) -> Result<()> {
    parts.bindings.sync_components(world, &mut parts.data)?;
    let sources = roots(world);
    let mut live: BTreeSet<String> = sources.iter().map(|(id, _, _)| id.clone()).collect();
    let mut ui = std::mem::take(&mut parts.ui);
    let result = process(world, parts, &mut ui, sources, ctx, input);
    parts.ui = ui;
    result?;
    world_panel::tick(world, parts, ctx, input, &mut live)?;
    parts.style.retain_roots(&live);
    parts.bindings.retain_roots(&live);
    Ok(())
}

/// Tick one set of roots against one `GameUi`.
pub(super) fn process(
    world: &mut World,
    parts: &mut TickParts,
    ui: &mut GameUi,
    sources: Vec<(String, Entity, UiRoot)>,
    ctx: &UiTickContext<'_>,
    input: &crate::ActionFrame,
) -> Result<()> {
    let mut authored: Vec<(String, UiRoot)> = sources
        .iter()
        .map(|(id, _, root)| (id.clone(), root.clone()))
        .collect();
    for ((id, entity, _), (_, root)) in sources.iter().zip(authored.iter_mut()) {
        parts
            .bindings
            .apply(id, entity.to_bits(), root, &parts.data);
    }
    let none = NodeStates::new();
    let states = states_by_root(ui);
    let mut themes: Vec<Option<SharedTheme>> = Vec::with_capacity(authored.len());
    let mut effective: Vec<(String, UiRoot)> = Vec::with_capacity(authored.len());
    for (id, root) in &authored {
        let theme = match parts.settings.theme_name_for(root) {
            Some(name) => Some(parts.style.theme(world, &parts.settings, name)?),
            None => None,
        };
        let resolved = match &theme {
            Some(theme) => parts
                .style
                .resolve(id, root, theme, states.get(id).unwrap_or(&none))?,
            None => root.clone(),
        };
        effective.push((id.clone(), resolved));
        themes.push(theme);
    }
    let events = ui.tick_with(&mut effective, input, ctx)?;
    // Interaction edits widget values on the effective roots; carry them over.
    for ((_, authored), (_, effective)) in authored.iter_mut().zip(&effective) {
        for (a, e) in authored.tree.nodes.iter_mut().zip(&effective.tree.nodes) {
            if a.widget != e.widget {
                a.widget = e.widget.clone();
            }
        }
    }
    for (id, root) in &authored {
        parts.bindings.write_back(id, root, &mut parts.data)?;
    }
    let states = states_by_root(ui);
    for ((id, root), theme) in authored.iter().zip(&themes) {
        if let Some(theme) = theme {
            parts.style.advance_only(
                id,
                root,
                theme,
                states.get(id).unwrap_or(&none),
                ctx.dt,
                parts.settings.reduced_motion_for(root),
            )?;
        }
    }
    for ((_, entity, previous), (_, root)) in sources.iter().zip(authored) {
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
    let by_root = ui.map(states_by_root).unwrap_or_default();
    let data = world.get_resource::<UiDataContext>();
    let bindings = world.get_resource::<UiBindingState>();
    let text = BitmapTextBackend::new(fonts, textures);
    let mut draws = vec![];
    let mut ids = BTreeSet::new();
    for (id, _, root) in roots {
        if !ids.insert(id.clone()) {
            return Err(invalid("Duplicate UI root ID"));
        }
        let mut state = ui.map(|ui| ui.render_state(&id)).unwrap_or_default();
        let effective = effective_for_render(world, &id, &root, &by_root)?;
        state.themed = effective.is_some();
        let drawn = effective.as_ref().unwrap_or(&root);
        if let (Some(data), Some(bindings)) = (data, bindings) {
            state.item_texts = bindings.item_texts(drawn, data);
        }
        draws.extend(drawn.extract_draws_with(&id, viewport, &state, &text, textures)?);
    }
    draws.extend(world_panel::extract(world, fonts, textures)?);
    Ok(draws)
}
