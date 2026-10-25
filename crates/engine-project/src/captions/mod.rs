//! Captions and subtitles (`docs/accessibility.md#captions-and-subtitles`).
//!
//! * [`CaptionTrack`] is the `caption_track` asset: speakers and timed cues,
//!   loaded with bounds and typed `captions.*` errors ([`load_caption_track`]).
//!   [`import::import_captions`] converts WebVTT and SRT text into one.
//! * [`Captions`] is the runtime resource: play tracks at an offset, seek, stop,
//!   push ad-hoc cues, and read the cues active at the current time. The
//!   authored component is [`CaptionPlayer`] (`engine.CaptionPlayer`).
//! * The cues are presented as a generated `UiRoot` on [`CAPTION_LAYER`] by
//!   [`present`], which applies the player's caption preferences from
//!   `engine.AccessibilitySettings`.
use crate::{Error, Result, schema::read_bounded, ui_tree::validate_markup};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

pub mod import;
mod player;
mod present;

pub use player::{
    ActiveCue, AdHocCue, CAPTION_PLAYER_COMPONENT, CaptionClock, CaptionEvent, CaptionPlayer,
    CaptionStyle, Captions, PlayOptions, PlaybackId, PlaybackInfo, descriptor, validate_component,
};
pub use present::{
    CAPTION_LAYER, CAPTION_ROOT_ID, CaptionSettings, build_root, cue_markup, visible,
};
pub(crate) use present::{register, tick};

/// Asset kind of a caption track.
pub const CAPTION_TRACK_KIND: &str = "caption_track";
/// Largest caption file read.
pub const MAX_CAPTION_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_CAPTION_CUES: usize = 20_000;
pub const MAX_CAPTION_SPEAKERS: usize = 64;
/// Longest cue text in bytes (markup included).
pub const MAX_CAPTION_TEXT: usize = 1024;
/// Latest time, in seconds, a cue may reach (about 11.5 days).
pub const MAX_CAPTION_TIME: f64 = 1_000_000.0;
/// Most cues shown at once (`captionMaxLines` is bounded by it).
pub const MAX_CAPTION_LINES: u32 = 8;

fn invalid(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

fn identifier(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// What a cue is, for filtering and display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionCategory {
    /// Speech: shown with the speaker label.
    #[default]
    Dialogue,
    /// A sound described in words; shown in square brackets and hidden when
    /// the player turns sound-effect captions off.
    SoundEffect,
    /// Music described in words; handled like a sound effect.
    Music,
}

impl CaptionCategory {
    /// Whether the player's sound-effect preference governs this category.
    pub const fn is_non_speech(self) -> bool {
        !matches!(self, Self::Dialogue)
    }
}

fn white() -> [f32; 4] {
    [1.0; 4]
}

/// Who speaks a cue.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionSpeaker {
    pub id: String,
    /// Display name; a message reference (`@key`) is localized.
    pub name: String,
    /// Straight-alpha colour of the speaker label.
    #[serde(default = "white")]
    pub color: [f32; 4],
}

fn zero() -> i32 {
    0
}

/// One timed caption. Visible while `start <= t < end`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionCue {
    pub id: String,
    /// Seconds from the start of the track (inclusive).
    pub start: f64,
    /// Seconds from the start of the track (exclusive); after `start`.
    pub end: f64,
    /// Speaker ID from the track's `speakers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Text in the rich-text markup of `docs/text.md`, or a message reference
    /// (`@key`), localized when shown.
    pub text: String,
    /// Higher priorities survive when more cues overlap than fit.
    #[serde(default = "zero", skip_serializing_if = "is_zero")]
    pub priority: i32,
    #[serde(default, skip_serializing_if = "is_dialogue")]
    pub category: CaptionCategory,
}

fn is_zero(value: &i32) -> bool {
    *value == 0
}
fn is_dialogue(value: &CaptionCategory) -> bool {
    *value == CaptionCategory::Dialogue
}

fn one() -> u32 {
    1
}

/// The `caption_track` asset (JSON).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionTrack {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub speakers: Vec<CaptionSpeaker>,
    /// Cues in any order; overlaps are allowed.
    pub cues: Vec<CaptionCue>,
}

impl CaptionTrack {
    /// Validate bounds, IDs, times, speaker references and markup.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(invalid(
                "captions.track.version",
                "Unsupported caption track version",
            ));
        }
        if self.speakers.len() > MAX_CAPTION_SPEAKERS {
            return Err(invalid(
                "captions.track.bounds",
                format!("A track has at most {MAX_CAPTION_SPEAKERS} speakers"),
            ));
        }
        if self.cues.len() > MAX_CAPTION_CUES {
            return Err(invalid(
                "captions.track.bounds",
                format!("A track has at most {MAX_CAPTION_CUES} cues"),
            ));
        }
        let mut speakers = BTreeSet::new();
        for speaker in &self.speakers {
            if !identifier(&speaker.id) || !speakers.insert(speaker.id.as_str()) {
                return Err(invalid(
                    "captions.speaker.id",
                    format!("Speaker ID `{}` is invalid or duplicated", speaker.id),
                ));
            }
            if speaker.name.is_empty()
                || speaker.name.len() > 128
                || speaker.name.chars().any(char::is_control)
            {
                return Err(invalid(
                    "captions.speaker.name",
                    format!("Speaker `{}` needs a name of 1..=128 bytes", speaker.id),
                ));
            }
            if speaker
                .color
                .iter()
                .any(|c| !c.is_finite() || !(0.0..=1.0).contains(c))
            {
                return Err(invalid(
                    "captions.speaker.color",
                    format!(
                        "Speaker `{}` colour channels must be within 0..=1",
                        speaker.id
                    ),
                ));
            }
        }
        let mut ids = BTreeSet::new();
        for cue in &self.cues {
            if !identifier(&cue.id) || !ids.insert(cue.id.as_str()) {
                return Err(invalid(
                    "captions.cue.id",
                    format!("Cue ID `{}` is invalid or duplicated", cue.id),
                ));
            }
            let in_range = |t: f64| t.is_finite() && (0.0..=MAX_CAPTION_TIME).contains(&t);
            if !in_range(cue.start) || !in_range(cue.end) {
                return Err(invalid(
                    "captions.cue.time",
                    format!(
                        "Cue `{}` times must be within 0..={MAX_CAPTION_TIME} seconds",
                        cue.id
                    ),
                ));
            }
            if cue.end <= cue.start {
                return Err(invalid(
                    "captions.cue.range",
                    format!("Cue `{}` must end after it starts", cue.id),
                ));
            }
            if let Some(speaker) = &cue.speaker
                && !speakers.contains(speaker.as_str())
            {
                return Err(invalid(
                    "captions.speaker.unknown",
                    format!("Cue `{}` names unknown speaker `{speaker}`", cue.id),
                ));
            }
            validate_cue_text(&cue.id, &cue.text)?;
            if !(-1_000_000..=1_000_000).contains(&cue.priority) {
                return Err(invalid(
                    "captions.cue.priority",
                    format!("Cue `{}` priority must be within +-1000000", cue.id),
                ));
            }
        }
        Ok(())
    }

    /// Speaker by ID.
    pub fn speaker(&self, id: &str) -> Option<&CaptionSpeaker> {
        self.speakers.iter().find(|s| s.id == id)
    }

    /// End of the last cue in seconds (`0` for an empty track).
    pub fn duration(&self) -> f64 {
        self.cues.iter().map(|c| c.end).fold(0.0, f64::max)
    }

    /// Cues visible at `time`: `start <= time < end`, in track order.
    pub fn active_at(&self, time: f64) -> Vec<&CaptionCue> {
        self.cues
            .iter()
            .filter(|c| c.start <= time && time < c.end)
            .collect()
    }
}

/// Text rules shared by cues and ad-hoc cues.
pub(crate) fn validate_cue_text(owner: &str, text: &str) -> Result<()> {
    if text.is_empty()
        || text.len() > MAX_CAPTION_TEXT
        || text.chars().any(|c| c.is_control() && c != '\n')
    {
        return Err(invalid(
            "captions.cue.text",
            format!(
                "Cue `{owner}` text must be 1..={MAX_CAPTION_TEXT} bytes without control characters"
            ),
        ));
    }
    if !text.starts_with('@') {
        validate_markup(text).map_err(|e| {
            invalid(
                "captions.cue.markup",
                format!("Cue `{owner}` markup is invalid: {e}"),
            )
        })?;
    }
    Ok(())
}

/// Parse and validate a caption track from JSON bytes.
pub fn parse_caption_track(bytes: &[u8]) -> Result<CaptionTrack> {
    if bytes.len() as u64 > MAX_CAPTION_FILE_BYTES {
        return Err(invalid(
            "captions.file.too_large",
            format!("Caption file exceeds {MAX_CAPTION_FILE_BYTES} bytes"),
        ));
    }
    let track: CaptionTrack = serde_json::from_slice(bytes).map_err(|e| {
        invalid(
            "captions.track.parse",
            format!("Invalid caption track: {e}"),
        )
    })?;
    track.validate()?;
    Ok(track)
}

/// Load and validate a `caption_track` file. Errors carry `captions.*` codes
/// and the file path.
pub fn load_caption_track(path: &Path) -> Result<CaptionTrack> {
    let bytes = read_bounded(path, MAX_CAPTION_FILE_BYTES).map_err(|error| {
        if error
            .diagnostics
            .first()
            .is_some_and(|d| d.code == "project.file.too_large")
        {
            invalid(
                "captions.file.too_large",
                format!("Caption file exceeds {MAX_CAPTION_FILE_BYTES} bytes"),
            )
            .at(path, "")
        } else {
            error
        }
    })?;
    parse_caption_track(&bytes).map_err(|error| {
        let first = error.diagnostics.first().cloned();
        match first {
            Some(d) => Error::new(&d.code, d.message).at(path, ""),
            None => error,
        }
    })
}
