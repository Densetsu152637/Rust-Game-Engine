//! The single style-resolution step: theme selection, cascade, motion and the
//! effective `UiRoot` that layout, interaction and rendering consume. Roots
//! without a theme bypass it entirely.
use super::{
    animation::{NodeMotion, RootMotion},
    stylesheet::{Computed, SharedTheme, UiComposedTheme, UiTheme, UiThemeSet},
    *,
};
use bevy_ecs::prelude::{Resource, World};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Global style settings (resource). Per-root authored fields (`UiRoot::theme`,
/// `UiRoot::reduced_motion`) override them.
#[derive(Clone, Debug, Default, PartialEq, Resource)]
pub struct UiStyleSettings {
    themes: UiThemeSet,
    active_theme: Option<String>,
    reduced_motion: bool,
}

impl UiStyleSettings {
    /// Register a native theme; an `engine.UiTheme` entity of the same name
    /// takes precedence.
    pub fn register_theme(&mut self, theme: UiTheme) -> Result<()> {
        self.themes.insert(theme)
    }

    pub fn themes(&self) -> &UiThemeSet {
        &self.themes
    }

    /// Theme used by roots that do not name one; switching takes effect on the
    /// next tick for every such root (transitions play where rules define them).
    pub fn set_active_theme(&mut self, name: impl Into<String>) {
        self.active_theme = Some(name.into());
    }

    pub fn clear_active_theme(&mut self) {
        self.active_theme = None;
    }

    pub fn active_theme(&self) -> Option<&str> {
        self.active_theme.as_deref()
    }

    /// Global reduced-motion preference: transitions become instant and
    /// non-essential animations stop. The accessibility settings write this.
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced_motion = reduced;
    }

    pub fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    /// Reduced motion for `root`: its authored override, else the global value.
    pub fn reduced_motion_for(&self, root: &UiRoot) -> bool {
        root.reduced_motion.unwrap_or(self.reduced_motion)
    }

    /// Theme `root` uses: its own name, else the active theme.
    pub fn theme_name_for<'a>(&'a self, root: &'a UiRoot) -> Option<&'a str> {
        root.theme.as_deref().or(self.active_theme.as_deref())
    }
}

/// Per-root motion state and composed-theme cache (resource).
#[derive(Default, Resource)]
pub struct UiStyleRuntime {
    motion: BTreeMap<String, RootMotion>,
    composed: BTreeMap<String, (Vec<UiTheme>, SharedTheme)>,
}

impl std::fmt::Debug for UiStyleRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiStyleRuntime")
            .field("roots", &self.motion.keys().collect::<Vec<_>>())
            .finish()
    }
}

fn find_theme<'a>(
    world: &'a World,
    settings: &'a UiStyleSettings,
    name: &str,
) -> Result<&'a UiTheme> {
    let mut found: Option<&UiTheme> = None;
    for entity in world.iter_entities() {
        if let Some(theme) = entity.get::<UiTheme>()
            && theme.name == name
            && found.replace(theme).is_some()
        {
            return Err(Error::new(
                "ui.theme.duplicate",
                format!("More than one UiTheme entity is named `{name}`"),
            ));
        }
    }
    found
        .or_else(|| settings.themes.get(name))
        .ok_or_else(|| Error::new("ui.theme.unknown", format!("Unknown UI theme `{name}`")))
}

/// The `extends` chain of `name`, base first, from entities then settings.
fn chain<'a>(
    world: &'a World,
    settings: &'a UiStyleSettings,
    name: &str,
) -> Result<Vec<&'a UiTheme>> {
    let mut chain: Vec<&UiTheme> = vec![];
    let mut cursor = Some(name.to_owned());
    while let Some(current) = cursor {
        if chain.len() >= stylesheet::MAX_THEME_DEPTH || chain.iter().any(|t| t.name == current) {
            return Err(Error::new(
                "ui.theme.cycle",
                "Theme extends chain too deep or cyclic (limit 4)",
            ));
        }
        let theme = find_theme(world, settings, &current)?;
        cursor = theme.extends.clone();
        chain.push(theme);
    }
    chain.reverse();
    Ok(chain)
}

/// Per-node interaction flags of one root, by node ID.
pub(super) type NodeStates = BTreeMap<String, UiStateFlags>;

/// `GameUi` flags grouped by root.
pub(super) fn states_by_root(ui: &GameUi) -> BTreeMap<String, NodeStates> {
    let mut out: BTreeMap<String, NodeStates> = BTreeMap::new();
    for (key, flags) in ui.states() {
        out.entry(key.root.clone())
            .or_default()
            .insert(key.node.clone(), *flags);
    }
    out
}

impl UiStyleRuntime {
    /// Composed theme for `name`, cached while its chain is unchanged.
    pub(super) fn theme(
        &mut self,
        world: &World,
        settings: &UiStyleSettings,
        name: &str,
    ) -> Result<SharedTheme> {
        let chain = chain(world, settings, name)?;
        if let Some((cached, theme)) = self.composed.get(name)
            && cached.len() == chain.len()
            && cached.iter().zip(&chain).all(|(a, b)| a == *b)
        {
            return Ok(theme.clone());
        }
        let theme = Arc::new(UiComposedTheme::from_chain(&chain)?);
        self.composed.insert(
            name.to_owned(),
            (chain.into_iter().cloned().collect(), theme.clone()),
        );
        Ok(theme)
    }

    /// Like [`Self::theme`] without touching the cache (render extraction).
    pub(super) fn theme_peek(
        &self,
        world: &World,
        settings: &UiStyleSettings,
        name: &str,
    ) -> Result<SharedTheme> {
        let chain = chain(world, settings, name)?;
        if let Some((cached, theme)) = self.composed.get(name)
            && cached.len() == chain.len()
            && cached.iter().zip(&chain).all(|(a, b)| a == *b)
        {
            return Ok(theme.clone());
        }
        Ok(Arc::new(UiComposedTheme::from_chain(&chain)?))
    }

    /// Cascade `root` against `theme` and overlay the current motion state;
    /// motion does not advance. `states` holds interaction flags by node ID.
    pub fn resolve(
        &self,
        id: &str,
        root: &UiRoot,
        theme: &UiComposedTheme,
        states: &BTreeMap<String, UiStateFlags>,
    ) -> Result<UiRoot> {
        let computed = Self::cascade(root, theme, states)?;
        Ok(self.build(id, root, &computed))
    }

    /// Cascade `root`, advance transitions and animations by `dt` seconds and
    /// return the effective root. `reduced_motion` makes transitions instant
    /// and drops non-essential animations.
    pub fn advance(
        &mut self,
        id: &str,
        root: &UiRoot,
        theme: &UiComposedTheme,
        states: &BTreeMap<String, UiStateFlags>,
        dt: f32,
        reduced_motion: bool,
    ) -> Result<UiRoot> {
        let computed = self.step(id, root, theme, states, dt, reduced_motion)?;
        Ok(self.build(id, root, &computed))
    }

    /// [`Self::advance`] without building the effective root.
    pub(super) fn advance_only(
        &mut self,
        id: &str,
        root: &UiRoot,
        theme: &UiComposedTheme,
        states: &BTreeMap<String, UiStateFlags>,
        dt: f32,
        reduced_motion: bool,
    ) -> Result<()> {
        self.step(id, root, theme, states, dt, reduced_motion)
            .map(|_| ())
    }

    fn step(
        &mut self,
        id: &str,
        root: &UiRoot,
        theme: &UiComposedTheme,
        states: &BTreeMap<String, UiStateFlags>,
        dt: f32,
        reduced_motion: bool,
    ) -> Result<Vec<Computed>> {
        let computed = Self::cascade(root, theme, states)?;
        let motion = self.motion.entry(id.to_owned()).or_default();
        let mut live = BTreeSet::new();
        for (node, c) in root.tree.nodes.iter().zip(&computed) {
            let animation = c
                .animation
                .as_ref()
                .and_then(|a| theme.keyframes(&a.name).map(|k| (a, k)));
            if c.transition.is_empty() && animation.is_none() {
                continue;
            }
            live.insert(node.id.as_str());
            motion.nodes.entry(node.id.clone()).or_default().step(
                &c.style,
                &c.transition,
                animation,
                dt,
                reduced_motion,
            );
        }
        motion.nodes.retain(|node, _| live.contains(node.as_str()));
        Ok(computed)
    }

    /// Whether any node of root `id` has a transition or animation in flight.
    pub fn is_animating(&self, id: &str) -> bool {
        self.motion
            .get(id)
            .is_some_and(|m| m.nodes.values().any(|n| !n.idle()))
    }

    /// Forget all motion state.
    pub fn reset(&mut self) {
        self.motion.clear();
    }

    /// Keep only the roots in `ids`.
    pub(super) fn retain_roots(&mut self, ids: &BTreeSet<String>) {
        self.motion.retain(|id, _| ids.contains(id));
    }

    fn cascade(
        root: &UiRoot,
        theme: &UiComposedTheme,
        states: &BTreeMap<String, UiStateFlags>,
    ) -> Result<Vec<Computed>> {
        let hierarchy = root.tree.hierarchy()?;
        let flags: Vec<UiStateFlags> = root
            .tree
            .nodes
            .iter()
            .map(|n| states.get(&n.id).copied().unwrap_or_default())
            .collect();
        theme.cascade(root, &hierarchy, &flags)
    }

    fn build(&self, id: &str, root: &UiRoot, computed: &[Computed]) -> UiRoot {
        let motion = self.motion.get(id);
        let mut out = root.clone();
        for (node, c) in out.tree.nodes.iter_mut().zip(computed) {
            let mut style = c.style.clone();
            if let Some(m) = motion.and_then(|m| m.nodes.get(&node.id)) {
                NodeMotion::overlay(m, &mut style);
            }
            node.style = style;
        }
        out
    }
}

/// Whether any of `roots` is drawn from a theme.
pub(super) fn any_themed<'a>(world: &World, mut roots: impl Iterator<Item = &'a UiRoot>) -> bool {
    world
        .get_resource::<UiStyleSettings>()
        .is_some_and(|s| roots.any(|r| s.theme_name_for(r).is_some()))
}

/// Effective root for drawing: the cascade of the root's theme with current
/// interaction flags and motion; `None` when the root uses no theme.
pub(super) fn effective_for_render(
    world: &World,
    id: &str,
    root: &UiRoot,
    states: &BTreeMap<String, NodeStates>,
) -> Result<Option<UiRoot>> {
    let Some(settings) = world.get_resource::<UiStyleSettings>() else {
        return Ok(None);
    };
    let Some(name) = settings.theme_name_for(root) else {
        return Ok(None);
    };
    let fallback = UiStyleRuntime::default();
    let runtime = world.get_resource::<UiStyleRuntime>().unwrap_or(&fallback);
    let theme = runtime.theme_peek(world, settings, name)?;
    let none = NodeStates::new();
    runtime
        .resolve(id, root, &theme, states.get(id).unwrap_or(&none))
        .map(Some)
}
