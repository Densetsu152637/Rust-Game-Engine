//! Caption presentation: the generated `UiRoot` and the per-tick driver.
use super::{
    ActiveCue, CaptionPlayer, Captions, MAX_CAPTION_LINES,
    player::{CaptionStyle, PlayOptions},
};
use crate::{
    EngineEvent, Result,
    accessibility::{AccessibilityControl, AccessibilitySettings},
    localization::Localization,
    runtime::AuthoredId,
    ui_tree::{
        AccessLive, AccessRole, UiAccessibility, UiAlign, UiEdges, UiJustify, UiLayout, UiLength,
        UiNode, UiRoot,
    },
};
use bevy_ecs::prelude::{Entity, World};

/// Layer of the caption `UiRoot`. High enough to sit above game UI, below the
/// modal-dialog layer (`UI_DIALOG_LAYER`) and popup layer (`UI_POPUP_LAYER`) the
/// UI draws dialogs and dropdown popups on.
pub const CAPTION_LAYER: i32 = 1_000_000;
/// Authored ID (and so the event/root key) of the generated root.
pub const CAPTION_ROOT_ID: &str = "captions";

/// The caption preferences of `engine.AccessibilitySettings`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptionSettings {
    pub enabled: bool,
    pub text_scale: f32,
    pub background_opacity: f32,
    pub speaker_names: bool,
    pub max_lines: u32,
    pub sound_effects: bool,
}

impl Default for CaptionSettings {
    fn default() -> Self {
        AccessibilitySettings::default().caption_settings()
    }
}

fn hex(color: [f32; 4]) -> String {
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    if color[3] >= 1.0 {
        format!(
            "#{:02x}{:02x}{:02x}",
            byte(color[0]),
            byte(color[1]),
            byte(color[2])
        )
    } else {
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            byte(color[0]),
            byte(color[1]),
            byte(color[2]),
            byte(color[3])
        )
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    for c in text.chars() {
        if matches!(c, '\\' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The cues to show, oldest first: non-speech cues are dropped when sound-effect
/// captions are off; when more than `max_lines` overlap the highest priorities
/// (then the most recently started) stay.
pub fn visible<'a>(captions: &'a Captions, settings: &CaptionSettings) -> Vec<&'a ActiveCue> {
    let mut cues: Vec<&ActiveCue> = captions
        .active()
        .iter()
        .filter(|c| settings.sound_effects || !c.category.is_non_speech())
        .collect();
    let limit = (settings.max_lines.clamp(1, MAX_CAPTION_LINES)) as usize;
    if cues.len() > limit {
        cues.sort_by(|a, b| b.priority.cmp(&a.priority).then(b.since.cmp(&a.since)));
        cues.truncate(limit);
    }
    cues.sort_by(|a, b| a.since.cmp(&b.since).then(b.priority.cmp(&a.priority)));
    cues
}

/// Rich-text markup of one cue as shown: the localized text, bracketed for
/// non-speech cues, behind the speaker's coloured name when enabled.
pub fn cue_markup(
    cue: &ActiveCue,
    settings: &CaptionSettings,
    localization: Option<&Localization>,
) -> String {
    let resolve = |text: &str, markup: bool| {
        localization
            .and_then(|l| l.resolve_text(text, markup))
            .unwrap_or_else(|| text.to_owned())
    };
    let mut text = resolve(&cue.text, true);
    if cue.category.is_non_speech() && !text.starts_with("\\[") {
        text = format!("\\[{text}\\]");
    }
    if settings.speaker_names
        && let Some(speaker) = &cue.speaker
    {
        let name = escape(&resolve(&speaker.name, false));
        text = format!("[color={}]{name}:[/color] {text}", hex(speaker.color));
    } else if text.starts_with('@') && localization.is_some_and(Localization::active) {
        // Keep the UI's own `@key` resolution from reading resolved text.
        text.insert(0, '@');
    }
    text
}

/// The text of `markup` without tags, with escapes resolved (what a screen
/// reader should read and what a plain label shows).
pub fn plain_text(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut chars = markup.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '[' => {
                for t in chars.by_ref() {
                    if t == ']' {
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Accessible name of a cue: one line, within the name limit.
fn access_label(plain: &str) -> String {
    let mut label: String = plain
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    label.truncate(label.floor_char_boundary(crate::ui_tree::MAX_UI_LABEL));
    label
}

/// The caption UI for the cues active in `captions`.
pub fn build_root(
    captions: &Captions,
    settings: &CaptionSettings,
    localization: Option<&Localization>,
) -> UiRoot {
    let style: &CaptionStyle = &captions.style;
    let mut panel = UiNode::container("captions", UiLayout::Column)
        .with_size(UiLength::Fill, UiLength::Fill)
        .with_style(|s| {
            s.padding_edges = Some(UiEdges {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: style.bottom_margin,
            });
            s.gap = 4.0;
            s.background = [0.0; 4];
            s.justify = Some(UiJustify::End);
            s.align_items = Some(UiAlign::Center);
        });
    panel.accessibility = Some(UiAccessibility {
        label: Some("Captions".into()),
        role: Some(AccessRole::Status),
        live: Some(AccessLive::Polite),
        ..Default::default()
    });
    let mut nodes = vec![panel];
    let background = [
        style.background[0],
        style.background[1],
        style.background[2],
        settings.background_opacity.clamp(0.0, 1.0),
    ];
    for (index, cue) in visible(captions, settings).into_iter().enumerate() {
        let markup = cue_markup(cue, settings, localization);
        let plain = plain_text(&markup);
        let node = if style.rich_text {
            UiNode::rich_label(format!("captions.cue.{index}"), markup)
        } else {
            let mut text = plain.clone();
            if text.starts_with('@') && localization.is_some_and(Localization::active) {
                text.insert(0, '@');
            }
            UiNode::label(format!("captions.cue.{index}"), text)
        };
        nodes.push(
            node.child_of("captions")
                .with_access_label(access_label(&plain))
                .with_size(UiLength::Auto, UiLength::Auto)
                .with_style(|s| {
                    s.max_width = UiLength::Percent(style.max_width);
                    s.padding = 8.0;
                    s.font = style.font.clone();
                    s.font_scale = settings.text_scale;
                    s.foreground = style.text_color;
                    s.background = background;
                }),
        );
    }
    UiRoot {
        tree: crate::ui_tree::UiTree { nodes },
        layer: CAPTION_LAYER,
        ..Default::default()
    }
}

/// Start, restart and stop playbacks for the `CaptionPlayer` components.
fn sync_players(world: &mut World, captions: &mut Captions) {
    let found: std::collections::BTreeMap<u64, CaptionPlayer> = world
        .iter_entities()
        .filter_map(|e| {
            e.get::<CaptionPlayer>()
                .map(|p| (e.id().to_bits(), p.clone()))
        })
        .collect();
    let gone: Vec<u64> = captions
        .players
        .keys()
        .filter(|k| !found.contains_key(k))
        .copied()
        .collect();
    for key in gone {
        if let Some((_, Some(playback))) = captions.players.remove(&key) {
            captions.stop(playback);
        }
    }
    for (key, component) in &found {
        if captions.players.get(key).map(|(c, _)| c) == Some(component) {
            continue;
        }
        if let Some((_, Some(playback))) = captions.players.remove(key) {
            captions.stop(playback);
        }
        let mut playback = None;
        match component.validate() {
            Err(error) => captions.record_error(error.to_string()),
            Ok(()) if component.autoplay && !component.track.is_empty() => {
                match captions.play(
                    &component.track,
                    PlayOptions {
                        offset: component.offset,
                        looping: component.looping,
                        clock: None,
                    },
                ) {
                    Ok(id) => playback = Some(id),
                    Err(error) => captions.record_error(error.to_string()),
                }
            }
            Ok(()) => {}
        }
        captions.players.insert(*key, (component.clone(), playback));
    }
    if let Some((component, _)) = captions.players.values().next().cloned()
        && component.validate().is_ok()
    {
        captions.style.font = component.font;
        captions.style.bottom_margin = component.bottom_margin;
        captions.style.max_width = component.max_width;
    }
}

/// Per-tick driver (called before the UI tick): start authored players, advance
/// time, rebuild the caption `UiRoot` when it changed and queue the cue events.
pub(crate) fn tick(world: &mut World, dt: f32) -> Result<()> {
    let Some(mut captions) = world.remove_resource::<Captions>() else {
        return Ok(());
    };
    // Settings are copied from the component here too, so a change shows in the same tick.
    crate::accessibility::sync(world);
    let settings = world
        .get_resource::<AccessibilityControl>()
        .map(|c| c.settings.caption_settings())
        .unwrap_or_default();
    sync_players(world, &mut captions);
    let events = captions.advance(f64::from(dt));
    let localization = world.get_resource::<Localization>().cloned();
    let result = publish(world, &mut captions, &settings, localization.as_ref());
    world.insert_resource(captions);
    result?;
    for event in events {
        // A full event queue drops the notification, never the caption.
        let _ = crate::emit_event(
            world,
            EngineEvent {
                name: event.name().into(),
                source: None,
                target: None,
                payload: event.payload(),
            },
        );
    }
    Ok(())
}

fn publish(
    world: &mut World,
    captions: &mut Captions,
    settings: &CaptionSettings,
    localization: Option<&Localization>,
) -> Result<()> {
    let entity: Option<Entity> = captions.entity.filter(|e| world.get_entity(*e).is_ok());
    if !settings.enabled {
        if let Some(entity) = entity {
            world.entity_mut(entity).remove::<UiRoot>();
        }
        captions.shown = None;
        captions.entity = entity;
        return Ok(());
    }
    if entity.is_none() && captions.active().is_empty() {
        // Nothing to show yet: the root entity is created with the first cue.
        captions.shown = None;
        return Ok(());
    }
    let root = build_root(captions, settings, localization);
    let entity = match entity {
        Some(entity) => entity,
        None => {
            captions.shown = None;
            world
                .spawn(AuthoredId {
                    id: CAPTION_ROOT_ID.into(),
                    name: CAPTION_ROOT_ID.into(),
                })
                .id()
        }
    };
    captions.entity = Some(entity);
    if captions.shown.as_ref() != Some(&root) {
        root.validate()?;
        world.entity_mut(entity).insert(root.clone());
        captions.shown = Some(root);
    }
    Ok(())
}

/// Install the resource and the `engine.CaptionPlayer` component.
pub(crate) fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<CaptionPlayer>(app, super::CAPTION_PLAYER_COMPONENT, 1)?;
    let project = app
        .world()
        .get_resource::<crate::runtime::ProjectResource>()
        .map(|p| p.0.clone());
    let captions = match project {
        Some(project) => Captions::from_project(&project)?,
        None => Captions::default(),
    };
    app.world_mut().insert_resource(captions);
    Ok(())
}
