use super::{
    localize,
    resolve::{NodeStates, any_themed, effective_for_render, states_by_root},
    stylesheet::SharedTheme,
    *,
};
use crate::{
    EngineEvent,
    accessibility::UiScaling,
    content::{FontAtlas, Texture},
    emit_event,
    localization::Localization,
    runtime::{AuthoredId, Draw},
};
use bevy_ecs::{
    prelude::{Entity, Resource, World},
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
    world.insert_resource(UiTickEvents::default());
    world.insert_resource(UiStyleSettings::default());
    world.insert_resource(UiStyleRuntime::default());
    world.insert_resource(UiDataContext::default());
    world.insert_resource(UiBindingState::default());
    world.insert_resource(UiWorldPanels::default());
    Ok(())
}

/// The widget events produced by the last UI tick (screen-space roots and
/// world panels), for native code that reacts to them without an event handler.
/// Replaced every tick; the same events are also delivered as engine events.
#[derive(Clone, Debug, Default, Resource)]
pub struct UiTickEvents(pub Vec<UiWidgetEvent>);

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
    /// Accessibility scaling of this tick.
    pub scaling: UiScaling,
    /// Widget events of this tick.
    pub events: Vec<UiWidgetEvent>,
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
            scaling: UiScaling::default(),
            events: vec![],
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
    ctx.text.begin_frame();
    let scaling = crate::accessibility::sync(world);
    let mut parts = TickParts::take(world);
    parts.scaling = scaling;
    let result = tick_all(world, &mut parts, ctx, input);
    let events = std::mem::take(&mut parts.events);
    parts.put(world);
    world.insert_resource(UiTickEvents(events));
    result
}

fn tick_all(
    world: &mut World,
    parts: &mut TickParts,
    ctx: &UiTickContext<'_>,
    input: &crate::ActionFrame,
) -> Result<()> {
    if world
        .get_resource::<Localization>()
        .is_some_and(Localization::take_ui_dirty)
    {
        // The locale (or its strings) changed: message bindings re-render once.
        parts.bindings.invalidate_strings();
    }
    parts.bindings.sync_components(world, &mut parts.data)?;
    let sources = roots(world);
    let mut live: BTreeSet<String> = sources.iter().map(|(id, _, _)| id.clone()).collect();
    let mut ui = std::mem::take(&mut parts.ui);
    // UI scale: lay out and hit-test in a smaller viewport; drawing scales back up.
    let scaling = parts.scaling;
    let scaled_ctx = UiTickContext {
        viewport: scaling.layout_viewport(ctx.viewport),
        ..*ctx
    };
    let mut scaled_input = input.clone();
    scaled_input.pointer = input.pointer.map(|v| v / scaling.ui_scale);
    ui.set_scale(scaling.ui_scale);
    let result = if scaling.ui_scale == 1.0 {
        process(world, parts, &mut ui, sources, ctx, input)
    } else {
        process(world, parts, &mut ui, sources, &scaled_ctx, &scaled_input)
    };
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
    let localization = world.get_resource::<Localization>().cloned();
    let themed = authored
        .iter()
        .any(|(_, root)| parts.settings.theme_name_for(root).is_some());
    let states = if themed {
        states_by_root(ui)
    } else {
        BTreeMap::new()
    };
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
        // Resolved text lives only in the effective root, never in the component.
        let resolved = localization
            .as_ref()
            .and_then(|l| localize::localize_root(&resolved, l))
            .unwrap_or(resolved);
        effective.push((id.clone(), resolved));
        themes.push(theme);
    }
    for (_, root) in &mut effective {
        parts.scaling.apply_text(root, ctx.text);
    }
    let events = ui.tick_with(&mut effective, input, ctx)?;
    // Interaction edits widget values on the effective roots; carry them over.
    for ((_, authored), (_, effective)) in authored.iter_mut().zip(&effective) {
        for (a, e) in authored.tree.nodes.iter_mut().zip(&effective.tree.nodes) {
            if a.widget != e.widget {
                let mut merged = e.widget.clone();
                localize::restore_authored(&mut merged, &a.widget);
                if a.widget != merged {
                    a.widget = merged;
                }
            }
        }
    }
    for (id, root) in &authored {
        parts.bindings.write_back(id, root, &mut parts.data)?;
    }
    let states = if themed {
        states_by_root(ui)
    } else {
        BTreeMap::new()
    };
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
    parts.events.extend(events.iter().cloned());
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
    extract_world_with(
        world,
        viewport,
        &BitmapTextBackend::new(fonts, textures),
        textures,
    )
}

/// [`extract_world`] with an explicit text backend (for example a
/// [`SelectingTextBackend`] that adds shaped TrueType text). Calls
/// `text.begin_frame()` once.
pub fn extract_world_with(
    world: &World,
    viewport: [u32; 2],
    text: &dyn UiTextBackend,
    textures: &BTreeMap<String, Arc<Texture>>,
) -> Result<Vec<Draw>> {
    text.begin_frame();
    let roots = roots(world);
    if roots.len() > MAX_UI_ROOTS {
        return Err(invalid("UI exceeds 64 roots"));
    }
    let ui = world.get_resource::<GameUi>();
    let by_root = if any_themed(world, roots.iter().map(|(_, _, root)| root)) {
        ui.map(states_by_root).unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    let data = world.get_resource::<UiDataContext>();
    let bindings = world.get_resource::<UiBindingState>();
    let localization = world.get_resource::<Localization>();
    let settings = world.get_resource::<crate::accessibility::AccessibilityControl>();
    let scaling = UiScaling::of(world);
    let layout_viewport = scaling.layout_viewport(viewport);
    let mut draws = vec![];
    let mut ids = BTreeSet::new();
    for (id, _, root) in roots {
        if !ids.insert(id.clone()) {
            return Err(invalid("Duplicate UI root ID"));
        }
        let mut state = ui.map(|ui| ui.render_state(&id)).unwrap_or_default();
        let effective = effective_for_render(world, &id, &root, &by_root)?;
        state.themed = effective.is_some();
        if let Some(control) = settings {
            state.focus_thickness = control.settings.focus_thickness;
            state.focus_contrast = control.settings.focus_high_contrast;
        }
        let drawn = effective.as_ref().unwrap_or(&root);
        let localized = localization.and_then(|l| localize::localize_root(drawn, l));
        let drawn = localized.as_ref().unwrap_or(drawn);
        let scaled_text = scaling.text_scaled(drawn, text);
        let drawn = scaled_text.as_ref().unwrap_or(drawn);
        if let (Some(data), Some(bindings)) = (data, bindings) {
            state.item_texts = bindings.item_texts(drawn, data);
        }
        let mut root_draws =
            drawn.extract_draws_with(&id, layout_viewport, &state, text, textures)?;
        if scaling.ui_scale != 1.0 {
            root_draws = scale_draws(root_draws, scaling.ui_scale);
        }
        draws.extend(root_draws);
    }
    draws.extend(world_panel::extract(world, text, textures)?);
    Ok(draws)
}

/// Scale screen-space draws from layout units to window logical pixels.
fn scale_draws(draws: Vec<Draw>, scale: f32) -> Vec<Draw> {
    draws
        .into_iter()
        .map(|mut draw| {
            let mut mesh = (*draw.mesh).clone();
            for vertex in &mut mesh.vertices {
                vertex.position[0] *= scale;
                vertex.position[1] *= scale;
            }
            draw.mesh = Arc::new(mesh);
            draw.clip = draw.clip.map(|c| c.map(|v| v * scale));
            draw
        })
        .collect()
}
