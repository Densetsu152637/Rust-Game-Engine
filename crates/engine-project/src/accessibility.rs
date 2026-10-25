//! Accessibility settings (`engine.AccessibilitySettings`, `docs/accessibility.md`).
//!
//! One authored component and one native resource. The component is data a
//! scene or `component_set` edits; [`AccessibilityControl`] is the resource
//! native code reads and writes. Every UI tick [`sync`] copies a changed
//! component into the resource and applies the resource: reduced motion and
//! the high-contrast theme reach [`UiStyleSettings`], the screen-reader hint
//! reaches `GameUi`, and [`UiScaling`] scales layout, hit testing and text.
use crate::{
    ComponentDescriptor, Error, Result,
    ui_tree::{GameUi, UiRoot, UiStyleSettings, UiTextBackend, UiTextKind, UiTextRequest},
};
use bevy_ecs::prelude::{Component, Entity, Resource, World};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

pub const ACCESSIBILITY_COMPONENT: &str = "engine.AccessibilitySettings";
pub const MIN_UI_SCALE: f32 = 0.5;
pub const MAX_UI_SCALE: f32 = 4.0;
pub const MIN_TEXT_SCALE: f32 = 0.5;
pub const MAX_TEXT_SCALE: f32 = 4.0;
/// Largest `minTextSize` in logical pixels.
pub const MAX_MIN_TEXT_SIZE: f32 = 96.0;
pub const MIN_FOCUS_THICKNESS: f32 = 1.0;
pub const MAX_FOCUS_THICKNESS: f32 = 16.0;

fn invalid(message: impl Into<String>) -> Error {
    Error::new("accessibility.invalid", message)
}

/// User accessibility preferences. Attach as `engine.AccessibilitySettings`
/// to any entity; the lowest entity carrying it wins.
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct AccessibilitySettings {
    pub version: u32,
    /// Multiplier of the UI's logical pixels: layout, hit testing and drawing
    /// all scale together (`0.5..=4`).
    pub ui_scale: f32,
    /// Multiplier of every node's `fontScale` (`0.5..=4`).
    pub text_scale: f32,
    /// Smallest on-screen line height, in window logical pixels (after UI
    /// scale), that text may have; `0` disables the floor.
    pub min_text_size: f32,
    /// Transitions become instant and non-essential animations stop.
    pub reduced_motion: bool,
    /// A screen reader is in use: build the accessibility tree every tick.
    /// The platform bridge also turns the tree on when one connects.
    pub screen_reader: bool,
    /// Thickness of the keyboard focus ring in logical pixels (`1..=16`).
    pub focus_thickness: f32,
    /// Draw the focus ring as a light ring inside a dark one, visible on any
    /// background.
    pub focus_high_contrast: bool,
    /// Name of a `UiTheme` made the active theme while set (roots that name
    /// their own theme keep it). Clearing it restores the previous theme.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high_contrast_theme: Option<String>,
}

impl Default for AccessibilitySettings {
    fn default() -> Self {
        Self {
            version: 1,
            ui_scale: 1.0,
            text_scale: 1.0,
            min_text_size: 0.0,
            reduced_motion: false,
            screen_reader: false,
            focus_thickness: 2.0,
            focus_high_contrast: false,
            high_contrast_theme: None,
        }
    }
}

impl AccessibilitySettings {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(invalid("Unsupported AccessibilitySettings version"));
        }
        let within = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        if !within(self.ui_scale, MIN_UI_SCALE, MAX_UI_SCALE) {
            return Err(invalid("uiScale must be within 0.5..=4"));
        }
        if !within(self.text_scale, MIN_TEXT_SCALE, MAX_TEXT_SCALE) {
            return Err(invalid("textScale must be within 0.5..=4"));
        }
        if !within(self.min_text_size, 0.0, MAX_MIN_TEXT_SIZE) {
            return Err(invalid("minTextSize must be within 0..=96"));
        }
        if !within(
            self.focus_thickness,
            MIN_FOCUS_THICKNESS,
            MAX_FOCUS_THICKNESS,
        ) {
            return Err(invalid("focusThickness must be within 1..=16"));
        }
        if let Some(name) = &self.high_contrast_theme
            && !((1..=64).contains(&name.len())
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
        {
            return Err(invalid("highContrastTheme is not a valid theme name"));
        }
        Ok(())
    }

    /// Scaling the UI step applies for these settings.
    pub fn scaling(&self) -> UiScaling {
        UiScaling {
            ui_scale: self.ui_scale,
            text_scale: self.text_scale,
            min_text_size: self.min_text_size,
        }
    }
}

/// State of the platform screen-reader bridge, in the style of
/// `UpscalerStatus`: why it is not available is part of the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum AccessBridgeStatus {
    /// No bridge exists (headless runs, hosts that did not install one).
    Unavailable(String),
    /// Installed; no assistive technology has connected.
    Idle,
    /// An assistive technology is connected and receives the tree.
    Connected,
}

impl Default for AccessBridgeStatus {
    fn default() -> Self {
        Self::Unavailable("no platform accessibility bridge is installed".into())
    }
}

/// Native accessibility resource: the effective settings and the bridge status.
#[derive(Resource, Clone, Debug, Default)]
pub struct AccessibilityControl {
    pub settings: AccessibilitySettings,
    /// Published by the platform bridge (the desktop runner).
    pub bridge: AccessBridgeStatus,
}

impl AccessibilityControl {
    /// Replace the settings after validating them.
    pub fn set(&mut self, settings: AccessibilitySettings) -> Result<()> {
        settings.validate()?;
        self.settings = settings;
        Ok(())
    }
    pub fn set_ui_scale(&mut self, scale: f32) -> Result<()> {
        self.set(AccessibilitySettings {
            ui_scale: scale,
            ..self.settings.clone()
        })
    }
    pub fn set_text_scale(&mut self, scale: f32) -> Result<()> {
        self.set(AccessibilitySettings {
            text_scale: scale,
            ..self.settings.clone()
        })
    }
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.settings.reduced_motion = reduced;
    }
}

/// Bookkeeping of what [`sync`] last applied. Starts from the default
/// settings, so default settings never override values native code set
/// directly on `UiStyleSettings`.
#[derive(Resource)]
pub struct AccessibilityRuntime {
    last_component: Option<AccessibilitySettings>,
    applied: Option<AccessibilitySettings>,
    /// Active theme before the high-contrast theme replaced it.
    saved_theme: Option<Option<String>>,
}

impl Default for AccessibilityRuntime {
    fn default() -> Self {
        Self {
            last_component: None,
            applied: Some(AccessibilitySettings::default()),
            saved_theme: None,
        }
    }
}

pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<AccessibilitySettings>(app, ACCESSIBILITY_COMPONENT, 1)?;
    let world = app.world_mut();
    world.insert_resource(AccessibilityControl::default());
    world.insert_resource(AccessibilityRuntime::default());
    crate::remap_ui::register(app)?;
    Ok(())
}

/// Typed validation of an authored component value.
pub fn validate_component(name: &str, data: &serde_json::Value) -> Result<()> {
    if name == ACCESSIBILITY_COMPONENT {
        serde_json::from_value::<AccessibilitySettings>(data.clone())?.validate()?;
    }
    Ok(())
}

pub fn descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: ACCESSIBILITY_COMPONENT.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "version":{"type":"integer","enum":[1]},
            "uiScale":{"type":"number","minimum":MIN_UI_SCALE,"maximum":MAX_UI_SCALE},
            "textScale":{"type":"number","minimum":MIN_TEXT_SCALE,"maximum":MAX_TEXT_SCALE},
            "minTextSize":{"type":"number","minimum":0,"maximum":MAX_MIN_TEXT_SIZE},
            "reducedMotion":{"type":"boolean"},
            "screenReader":{"type":"boolean"},
            "focusThickness":{"type":"number","minimum":MIN_FOCUS_THICKNESS,"maximum":MAX_FOCUS_THICKNESS},
            "focusHighContrast":{"type":"boolean"},
            "highContrastTheme":{"type":"string","minLength":1,"maxLength":64}
        }}),
        defaults: serde_json::to_value(AccessibilitySettings::default())
            .expect("defaults serialize"),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}

/// Copy a changed component into the resource and apply the resource to the
/// style settings and the UI. Called at the start of every UI tick; returns
/// the scaling to use for it.
pub(crate) fn sync(world: &mut World) -> UiScaling {
    if !world.contains_resource::<AccessibilityControl>() {
        return UiScaling::default();
    }
    let component: Option<AccessibilitySettings> = world
        .iter_entities()
        .filter_map(|e| e.get::<AccessibilitySettings>().map(|s| (e.id(), s)))
        .min_by_key(|(id, _): &(Entity, _)| id.to_bits())
        .map(|(_, s)| s.clone());
    let mut runtime = world
        .remove_resource::<AccessibilityRuntime>()
        .unwrap_or_default();
    let mut control = world.resource_mut::<AccessibilityControl>();
    if component != runtime.last_component {
        if let Some(settings) = &component
            && settings.validate().is_ok()
        {
            control.settings = settings.clone();
        }
        runtime.last_component = component;
    }
    if control.settings.validate().is_err() {
        // Native code wrote an invalid value directly: keep the last applied one.
        if let Some(applied) = &runtime.applied {
            control.settings = applied.clone();
        } else {
            control.settings = AccessibilitySettings::default();
        }
    }
    let settings = control.settings.clone();
    let platform = control.bridge == AccessBridgeStatus::Connected;
    if runtime.applied.as_ref() != Some(&settings) {
        if let Some(mut style) = world.get_resource_mut::<UiStyleSettings>() {
            let before = runtime.applied.as_ref();
            if before.is_none_or(|b| b.reduced_motion != settings.reduced_motion) {
                style.set_reduced_motion(settings.reduced_motion);
            }
            let wanted = settings.high_contrast_theme.as_deref();
            let had = before.and_then(|b| b.high_contrast_theme.as_deref());
            if wanted != had {
                match wanted {
                    Some(name) => {
                        if runtime.saved_theme.is_none() {
                            runtime.saved_theme = Some(style.active_theme().map(str::to_owned));
                        }
                        style.set_active_theme(name);
                    }
                    None => match runtime.saved_theme.take() {
                        Some(Some(previous)) => style.set_active_theme(previous),
                        Some(None) => style.clear_active_theme(),
                        None => {}
                    },
                }
            }
        }
        runtime.applied = Some(settings.clone());
    }
    if let Some(mut ui) = world.get_resource_mut::<GameUi>() {
        ui.set_accessibility_hint(settings.screen_reader || platform);
    }
    world.insert_resource(runtime);
    settings.scaling()
}

/// UI scale, text scale and the text-size floor for one tick or extraction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiScaling {
    pub ui_scale: f32,
    pub text_scale: f32,
    pub min_text_size: f32,
}

impl Default for UiScaling {
    fn default() -> Self {
        Self {
            ui_scale: 1.0,
            text_scale: 1.0,
            min_text_size: 0.0,
        }
    }
}

impl UiScaling {
    /// The scaling in force for `world` (identity without the resource).
    pub fn of(world: &World) -> Self {
        world
            .get_resource::<AccessibilityControl>()
            .map(|c| c.settings.scaling())
            .unwrap_or_default()
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Size the UI is laid out in: the window size divided by the UI scale,
    /// rounded up so the scaled UI always covers the window.
    pub fn layout_viewport(&self, viewport: [u32; 2]) -> [u32; 2] {
        if self.ui_scale == 1.0 {
            return viewport;
        }
        viewport.map(|v| ((v as f32 / self.ui_scale).ceil() as u32).max(1))
    }

    /// Multiply every node's `fontScale` by the text scale and raise it so
    /// the text is at least `min_text_size` pixels tall on screen. The
    /// authored root is not changed.
    pub fn apply_text(&self, root: &mut UiRoot, text: &dyn UiTextBackend) {
        if self.text_scale == 1.0 && self.min_text_size <= 0.0 {
            return;
        }
        // Line height of each font at fontScale 1, measured once per font.
        let mut unit: BTreeMap<String, f32> = BTreeMap::new();
        for node in &mut root.tree.nodes {
            let mut scale = node.style.font_scale * self.text_scale;
            if self.min_text_size > 0.0 {
                let line = *unit.entry(node.style.font.clone()).or_insert_with(|| {
                    text.measure(&UiTextRequest {
                        text: "Mg",
                        kind: UiTextKind::Plain,
                        font: &node.style.font,
                        scale: 1.0,
                        color: [1.0; 4],
                        max_width: None,
                    })
                    .map(|m| m.line_height)
                    .unwrap_or(0.0)
                });
                if line > 0.0 {
                    scale = scale.max(self.min_text_size / self.ui_scale / line);
                }
            }
            node.style.font_scale = scale.clamp(0.01, 16.0);
        }
    }

    /// [`Self::apply_text`] on a clone, or `None` when nothing would change.
    pub fn text_scaled(&self, root: &UiRoot, text: &dyn UiTextBackend) -> Option<UiRoot> {
        if self.text_scale == 1.0 && self.min_text_size <= 0.0 {
            return None;
        }
        let mut scaled = root.clone();
        self.apply_text(&mut scaled, text);
        Some(scaled)
    }
}
