//! The runtime caption controller and the `engine.CaptionPlayer` component.
use super::{
    CaptionCategory, CaptionSpeaker, CaptionTrack, MAX_CAPTION_TIME, invalid, validate_cue_text,
};
use crate::{ComponentDescriptor, Result, schema::Project};
use bevy_ecs::prelude::{Component, Resource};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub const CAPTION_PLAYER_COMPONENT: &str = "engine.CaptionPlayer";
/// Playbacks and ad-hoc cues alive at once.
const MAX_PLAYBACKS: usize = 32;
const MAX_ADHOC: usize = 64;
const MAX_ERRORS: usize = 16;

/// Caller-supplied playback clock: the track position in seconds, or `None`
/// to hold the last position (for example an audio source's playback
/// position, see `docs/accessibility.md`).
pub type CaptionClock = Arc<dyn Fn() -> Option<f64> + Send + Sync>;

/// Handle of a running track.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct PlaybackId(pub u64);

#[derive(Clone, Default)]
pub struct PlayOptions {
    /// Starting position in seconds.
    pub offset: f64,
    /// Restart at 0 after the last cue (ignored with a custom clock).
    pub looping: bool,
    /// Drive the position from this clock instead of the runtime tick time.
    pub clock: Option<CaptionClock>,
}

/// A cue pushed from code, shown for `duration` seconds of tick time.
#[derive(Clone, Debug, PartialEq)]
pub struct AdHocCue {
    pub speaker: Option<CaptionSpeaker>,
    /// Markup or a message reference (`@key`).
    pub text: String,
    pub duration: f64,
    pub category: CaptionCategory,
    pub priority: i32,
}

impl AdHocCue {
    pub fn new(text: impl Into<String>, duration: f64) -> Self {
        Self {
            speaker: None,
            text: text.into(),
            duration,
            category: CaptionCategory::Dialogue,
            priority: 0,
        }
    }
    pub fn spoken(mut self, speaker: CaptionSpeaker) -> Self {
        self.speaker = Some(speaker);
        self
    }
}

/// A cue visible now, with its speaker resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveCue {
    /// Stable key: `<playback>:<cue id>` or `adhoc:<n>`.
    pub key: String,
    pub playback: Option<PlaybackId>,
    pub cue: String,
    pub speaker: Option<CaptionSpeaker>,
    /// Authored text (markup or `@key`), not yet localized.
    pub text: String,
    pub category: CaptionCategory,
    pub priority: i32,
    /// Track time at which the cue starts (infinite for ad-hoc cues).
    pub start: f64,
    /// Order in which cues became visible (lower is older).
    pub since: u64,
}

/// `captions.cue_started` / `captions.cue_ended`, also emitted as engine events.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionEvent {
    pub started: bool,
    pub cue: ActiveCue,
}

impl CaptionEvent {
    pub fn name(&self) -> &'static str {
        if self.started {
            "captions.cue_started"
        } else {
            "captions.cue_ended"
        }
    }
    pub fn payload(&self) -> serde_json::Value {
        json!({"cue": self.cue.cue, "playback": self.cue.playback.map(|p| p.0),
            "speaker": self.cue.speaker.as_ref().map(|s| s.id.clone()),
            "text": self.cue.text, "category": self.cue.category})
    }
}

/// Read-only view of a running track.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackInfo {
    pub id: PlaybackId,
    pub track: String,
    pub position: f64,
    pub looping: bool,
}

struct Playback {
    id: PlaybackId,
    track_id: String,
    track: Arc<CaptionTrack>,
    position: f64,
    looping: bool,
    clock: Option<CaptionClock>,
}

struct AdHoc {
    serial: u64,
    cue: AdHocCue,
    remaining: f64,
}

/// Presentation style of the generated caption UI.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionStyle {
    /// Font key of the caption text (empty uses the UI default).
    pub font: String,
    /// Distance from the bottom of the UI viewport (safe-area margin), pixels.
    pub bottom_margin: f32,
    /// Widest a caption may be, as a fraction of the viewport width.
    pub max_width: f32,
    /// Colour of the text.
    pub text_color: [f32; 4],
    /// Background colour; its alpha is replaced by the player's opacity.
    pub background: [f32; 3],
    /// Draw cues as rich text (coloured speaker names, `[b]`/`[i]`/`[u]`).
    /// The runtime clears it when the project has no `font_face` assets: the
    /// bitmap-font text path has no `[...]` markup, so cues are drawn as plain text.
    pub rich_text: bool,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        Self {
            font: String::new(),
            bottom_margin: 48.0,
            max_width: 0.8,
            text_color: [1.0; 4],
            background: [0.0; 3],
            rich_text: true,
        }
    }
}

/// Authored caption player: plays a `caption_track` asset when the entity
/// exists. The lowest entity carrying it also supplies the presentation style.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct CaptionPlayer {
    pub version: u32,
    /// `caption_track` asset ID; empty plays nothing.
    pub track: String,
    /// Start playing when the component appears or changes.
    pub autoplay: bool,
    #[serde(rename = "loop")]
    pub looping: bool,
    /// Starting position in seconds.
    pub offset: f64,
    /// Font key of the caption text.
    pub font: String,
    /// Safe-area margin below the captions, in UI pixels.
    pub bottom_margin: f32,
    /// Widest caption, as a fraction of the viewport (`0.2..=1`).
    pub max_width: f32,
}

impl Default for CaptionPlayer {
    fn default() -> Self {
        Self {
            version: 1,
            track: String::new(),
            autoplay: true,
            looping: false,
            offset: 0.0,
            font: String::new(),
            bottom_margin: 48.0,
            max_width: 0.8,
        }
    }
}

impl CaptionPlayer {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(invalid("captions.player.invalid", m));
        if self.version != 1 {
            return bad("Unsupported CaptionPlayer version");
        }
        if self.track.len() > 64
            || !self
                .track
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return bad("track must be a caption_track asset ID of at most 64 characters");
        }
        if !self.offset.is_finite() || !(0.0..=MAX_CAPTION_TIME).contains(&self.offset) {
            return bad("offset must be a finite number of seconds within 0..=1000000");
        }
        if self.font.len() > 128 || self.font.chars().any(char::is_control) {
            return bad("font must be at most 128 characters");
        }
        if !self.bottom_margin.is_finite() || !(0.0..=2000.0).contains(&self.bottom_margin) {
            return bad("bottomMargin must be within 0..=2000");
        }
        if !self.max_width.is_finite() || !(0.2..=1.0).contains(&self.max_width) {
            return bad("maxWidth must be within 0.2..=1");
        }
        Ok(())
    }
}

pub fn descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: CAPTION_PLAYER_COMPONENT.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "version":{"type":"integer","enum":[1]},
            "track":{"type":"string","maxLength":64},
            "autoplay":{"type":"boolean"},
            "loop":{"type":"boolean"},
            "offset":{"type":"number","minimum":0,"maximum":MAX_CAPTION_TIME},
            "font":{"type":"string","maxLength":128},
            "bottomMargin":{"type":"number","minimum":0,"maximum":2000},
            "maxWidth":{"type":"number","minimum":0.2,"maximum":1}
        }}),
        defaults: serde_json::to_value(CaptionPlayer::default()).expect("defaults serialize"),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}

/// Typed validation of an authored component value.
pub fn validate_component(name: &str, data: &serde_json::Value) -> Result<()> {
    if name == CAPTION_PLAYER_COMPONENT {
        serde_json::from_value::<CaptionPlayer>(data.clone())
            .map_err(|e| invalid("captions.player.invalid", e.to_string()))?
            .validate()?;
    }
    Ok(())
}

/// Runtime caption controller (a resource).
///
/// Time advances with [`Captions::advance`], which the runtime calls every tick
/// with the tick's delta; a playback may use its own [`CaptionClock`] instead.
#[derive(Resource, Default)]
pub struct Captions {
    tracks: BTreeMap<String, Arc<CaptionTrack>>,
    playbacks: Vec<Playback>,
    adhoc: Vec<AdHoc>,
    next_id: u64,
    next_since: u64,
    active: Vec<ActiveCue>,
    /// Presentation style; the `CaptionPlayer` component overwrites it.
    pub style: CaptionStyle,
    pub(super) players: BTreeMap<u64, (CaptionPlayer, Option<PlaybackId>)>,
    pub(super) errors: Vec<String>,
    pub(super) entity: Option<bevy_ecs::prelude::Entity>,
    pub(super) shown: Option<crate::ui_tree::UiRoot>,
}

impl Captions {
    /// Load every `caption_track` asset of the project.
    pub fn from_project(project: &Project) -> Result<Self> {
        let mut captions = Self::default();
        for asset in project
            .assets
            .values()
            .filter(|a| a.kind == super::CAPTION_TRACK_KIND)
        {
            let track = super::load_caption_track(&project.resolve_asset(&asset.id)?)?;
            captions.tracks.insert(asset.id.clone(), Arc::new(track));
        }
        Ok(captions)
    }

    /// Register a track under `id` (replacing one of that name).
    pub fn insert_track(&mut self, id: impl Into<String>, track: CaptionTrack) -> Result<()> {
        track.validate()?;
        let id = id.into();
        if id.is_empty() || id.len() > 64 {
            return Err(invalid(
                "captions.track.id",
                "Track ID must be 1..=64 bytes",
            ));
        }
        self.tracks.insert(id, Arc::new(track));
        Ok(())
    }

    pub fn track(&self, id: &str) -> Option<&Arc<CaptionTrack>> {
        self.tracks.get(id)
    }

    /// Start a track. `captions.track.unknown` when it is not registered,
    /// `captions.playback.limit` past 32 concurrent playbacks.
    pub fn play(&mut self, track: &str, options: PlayOptions) -> Result<PlaybackId> {
        let found = self.tracks.get(track).cloned().ok_or_else(|| {
            invalid(
                "captions.track.unknown",
                format!("No caption track `{track}`"),
            )
        })?;
        if self.playbacks.len() >= MAX_PLAYBACKS {
            return Err(invalid(
                "captions.playback.limit",
                format!("At most {MAX_PLAYBACKS} caption tracks play at once"),
            ));
        }
        if !options.offset.is_finite() || options.offset < 0.0 {
            return Err(invalid(
                "captions.playback.offset",
                "Offset must be a non-negative finite number of seconds",
            ));
        }
        self.next_id += 1;
        let id = PlaybackId(self.next_id);
        self.playbacks.push(Playback {
            id,
            track_id: track.to_owned(),
            track: found,
            position: options.offset,
            looping: options.looping && options.clock.is_none(),
            clock: options.clock,
        });
        Ok(id)
    }

    /// Stop a playback; its cues end on the next [`Self::advance`].
    pub fn stop(&mut self, id: PlaybackId) -> bool {
        let before = self.playbacks.len();
        self.playbacks.retain(|p| p.id != id);
        self.playbacks.len() != before
    }

    /// Move a playback to `position` seconds. `captions.playback.unknown` if
    /// it is not running. Takes effect on the next [`Self::advance`].
    pub fn seek(&mut self, id: PlaybackId, position: f64) -> Result<()> {
        if !position.is_finite() || position < 0.0 {
            return Err(invalid(
                "captions.playback.offset",
                "Position must be a non-negative finite number of seconds",
            ));
        }
        let playback = self
            .playbacks
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| invalid("captions.playback.unknown", "No such caption playback"))?;
        playback.position = position;
        Ok(())
    }

    /// Current position of a playback in seconds.
    pub fn position(&self, id: PlaybackId) -> Option<f64> {
        self.playbacks
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.position)
    }

    /// Running tracks.
    pub fn playing(&self) -> Vec<PlaybackInfo> {
        self.playbacks
            .iter()
            .map(|p| PlaybackInfo {
                id: p.id,
                track: p.track_id.clone(),
                position: p.position,
                looping: p.looping,
            })
            .collect()
    }

    /// Show a cue for `cue.duration` seconds of tick time.
    pub fn push(&mut self, cue: AdHocCue) -> Result<u64> {
        if !cue.duration.is_finite() || cue.duration <= 0.0 || cue.duration > 3600.0 {
            return Err(invalid(
                "captions.cue.range",
                "Ad-hoc cue duration must be within (0, 3600] seconds",
            ));
        }
        validate_cue_text("ad-hoc", &cue.text)?;
        if self.adhoc.len() >= MAX_ADHOC {
            return Err(invalid(
                "captions.cue.limit",
                format!("At most {MAX_ADHOC} ad-hoc cues at once"),
            ));
        }
        self.next_id += 1;
        let serial = self.next_id;
        self.adhoc.push(AdHoc {
            serial,
            remaining: cue.duration,
            cue,
        });
        Ok(serial)
    }

    /// Stop every playback and drop every ad-hoc cue; active cues end on the
    /// next [`Self::advance`].
    pub fn clear(&mut self) {
        self.playbacks.clear();
        self.adhoc.clear();
        for (_, playback) in self.players.values_mut() {
            *playback = None;
        }
    }

    /// Cues visible now, oldest first.
    pub fn active(&self) -> &[ActiveCue] {
        &self.active
    }

    /// Problems found while starting authored players (bounded).
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    pub(super) fn record_error(&mut self, message: String) {
        if self.errors.len() < MAX_ERRORS && !self.errors.contains(&message) {
            self.errors.push(message);
        }
    }

    /// Advance tick-driven clocks by `dt` seconds, refresh the active cues and
    /// return the cues that started or ended since the last call. `advance(0.0)`
    /// refreshes after a seek without moving time.
    pub fn advance(&mut self, dt: f64) -> Vec<CaptionEvent> {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        for playback in &mut self.playbacks {
            if let Some(clock) = &playback.clock {
                if let Some(position) = clock().filter(|p| p.is_finite() && *p >= 0.0) {
                    playback.position = position;
                }
            } else {
                playback.position += dt;
            }
            let duration = playback.track.duration();
            if playback.looping && duration > 0.0 && playback.position >= duration {
                playback.position %= duration;
            }
        }
        for item in &mut self.adhoc {
            item.remaining -= dt;
        }
        // A finished playback leaves once its last cue has been evaluated.
        let mut current: Vec<ActiveCue> = vec![];
        for playback in &self.playbacks {
            for cue in playback.track.active_at(playback.position) {
                current.push(ActiveCue {
                    key: format!("{}:{}", playback.id.0, cue.id),
                    playback: Some(playback.id),
                    cue: cue.id.clone(),
                    speaker: cue
                        .speaker
                        .as_deref()
                        .and_then(|s| playback.track.speaker(s).cloned()),
                    text: cue.text.clone(),
                    category: cue.category,
                    priority: cue.priority,
                    start: cue.start,
                    since: 0,
                });
            }
        }
        for item in self.adhoc.iter().filter(|a| a.remaining > 0.0) {
            current.push(ActiveCue {
                key: format!("adhoc:{}", item.serial),
                playback: None,
                cue: format!("adhoc-{}", item.serial),
                speaker: item.cue.speaker.clone(),
                text: item.cue.text.clone(),
                category: item.cue.category,
                priority: item.cue.priority,
                start: f64::INFINITY,
                since: 0,
            });
        }
        let previous: BTreeMap<String, ActiveCue> =
            self.active.drain(..).map(|c| (c.key.clone(), c)).collect();
        let keys: BTreeSet<&String> = current.iter().map(|c| &c.key).collect();
        let mut events = vec![];
        for (key, cue) in &previous {
            if !keys.contains(key) {
                events.push(CaptionEvent {
                    started: false,
                    cue: cue.clone(),
                });
            }
        }
        // Cues first seen now take the next order numbers, earlier start first.
        current.sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| a.key.cmp(&b.key)));
        let mut fresh: Vec<&mut ActiveCue> = vec![];
        for cue in &mut current {
            match previous.get(&cue.key) {
                Some(old) => cue.since = old.since,
                None => fresh.push(cue),
            }
        }
        for cue in fresh {
            self.next_since += 1;
            cue.since = self.next_since;
            events.push(CaptionEvent {
                started: true,
                cue: cue.clone(),
            });
        }
        current.sort_by_key(|c| c.since);
        self.active = current;
        // Drop what finished.
        self.adhoc.retain(|a| a.remaining > 0.0);
        self.playbacks
            .retain(|p| p.looping || p.clock.is_some() || p.position < p.track.duration());
        events
    }
}
