//! WebVTT and SRT import into a [`CaptionTrack`].
//!
//! Supported: cue timings (`hh:mm:ss.mmm`, `mm:ss.mmm`, SRT's `hh:mm:ss,mmm`),
//! optional cue identifiers, multi-line text, `<v Speaker>` voice tags (WebVTT),
//! `<b>`, `<i>` and `<u>` (mapped to `[b]`, `[i]`, `[u]`), other tags removed,
//! the HTML entities `&amp; &lt; &gt; &nbsp;`. A cue whose whole text is in square
//! brackets (`[door creaks]`) becomes a `sound_effect` cue without the brackets
//! (the presenter adds them back). `NOTE`, `STYLE` and `REGION` blocks and cue
//! settings after the end time are ignored. Speakers are created from voice
//! tags, with colours cycling through a palette.
use super::{
    CaptionCategory, CaptionCue, CaptionSpeaker, CaptionTrack, MAX_CAPTION_CUES,
    MAX_CAPTION_FILE_BYTES, MAX_CAPTION_SPEAKERS, MAX_CAPTION_TEXT, invalid,
};
use crate::Result;
use std::{collections::BTreeMap, path::Path};

const PALETTE: [[f32; 4]; 6] = [
    [1.0, 1.0, 0.6, 1.0],
    [0.6, 1.0, 1.0, 1.0],
    [0.7, 1.0, 0.7, 1.0],
    [1.0, 0.7, 1.0, 1.0],
    [1.0, 0.8, 0.5, 1.0],
    [0.8, 0.8, 1.0, 1.0],
];

/// Import the text of a WebVTT (starts with `WEBVTT`) or SRT file.
/// Errors: `captions.import.*`.
pub fn import_captions(text: &str) -> Result<CaptionTrack> {
    if text.len() as u64 > MAX_CAPTION_FILE_BYTES {
        return Err(invalid(
            "captions.file.too_large",
            format!("Caption file exceeds {MAX_CAPTION_FILE_BYTES} bytes"),
        ));
    }
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut speakers: Vec<CaptionSpeaker> = vec![];
    let mut by_name: BTreeMap<String, String> = BTreeMap::new();
    let mut cues: Vec<CaptionCue> = vec![];
    for block in text.split("\n\n") {
        let lines: Vec<&str> = block.lines().map(str::trim_end).collect();
        if lines.first().is_some_and(|l| {
            l.starts_with("NOTE") || l.starts_with("STYLE") || l.starts_with("REGION")
        }) {
            continue;
        }
        let Some(timing) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let (start, end) = parse_timing(lines[timing])?;
        let raw = lines[timing + 1..].join("\n");
        if raw.trim().is_empty() {
            continue;
        }
        if cues.len() >= MAX_CAPTION_CUES {
            return Err(invalid(
                "captions.import.bounds",
                format!("More than {MAX_CAPTION_CUES} cues"),
            ));
        }
        let (voice, body) = convert_text(&raw);
        let mut category = CaptionCategory::Dialogue;
        let mut body = body.trim().to_owned();
        if voice.is_none()
            && body.starts_with("\\[")
            && body.ends_with("\\]")
            && body.matches("\\[").count() == 1
        {
            body = body[2..body.len() - 2].trim().to_owned();
            category = CaptionCategory::SoundEffect;
        }
        if body.is_empty() || body.len() > MAX_CAPTION_TEXT {
            return Err(invalid(
                "captions.import.text",
                format!("Cue {} text is empty or too long", cues.len() + 1),
            ));
        }
        let speaker = match voice {
            Some(name) => {
                let id = by_name.get(&name).cloned().map_or_else(
                    || {
                        if speakers.len() >= MAX_CAPTION_SPEAKERS {
                            return Err(invalid(
                                "captions.import.bounds",
                                format!("More than {MAX_CAPTION_SPEAKERS} speakers"),
                            ));
                        }
                        let id = speaker_id(&name, speakers.len());
                        speakers.push(CaptionSpeaker {
                            id: id.clone(),
                            name: name.clone(),
                            color: PALETTE[speakers.len() % PALETTE.len()],
                        });
                        by_name.insert(name.clone(), id.clone());
                        Ok(id)
                    },
                    Ok,
                )?;
                Some(id)
            }
            None => None,
        };
        cues.push(CaptionCue {
            id: format!("cue-{}", cues.len() + 1),
            start,
            end,
            speaker,
            text: body,
            priority: 0,
            category,
        });
    }
    let track = CaptionTrack {
        version: 1,
        speakers,
        cues,
    };
    track.validate()?;
    Ok(track)
}

/// Read and import a `.vtt` or `.srt` file.
pub fn import_captions_file(path: &Path) -> Result<CaptionTrack> {
    let bytes = crate::schema::read_bounded(path, MAX_CAPTION_FILE_BYTES)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| invalid("captions.import.utf8", "Caption file is not UTF-8").at(path, ""))?;
    import_captions(&text).map_err(|e| {
        let d = e.diagnostics.first().cloned();
        match d {
            Some(d) => crate::Error::new(&d.code, d.message).at(path, ""),
            None => e,
        }
    })
}

fn speaker_id(name: &str, index: usize) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    if slug.chars().any(|c| c != '_') {
        format!("{slug}-{}", index + 1)
    } else {
        format!("speaker-{}", index + 1)
    }
}

fn parse_timing(line: &str) -> Result<(f64, f64)> {
    let bad = || {
        invalid(
            "captions.import.timing",
            format!("Invalid cue timing line `{line}`"),
        )
    };
    let (a, rest) = line.split_once("-->").ok_or_else(bad)?;
    let end = rest.split_whitespace().next().ok_or_else(bad)?;
    Ok((
        parse_timestamp(a.trim()).ok_or_else(bad)?,
        parse_timestamp(end).ok_or_else(bad)?,
    ))
}

fn parse_timestamp(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.split(':').collect();
    let (h, m, s) = match parts.as_slice() {
        [m, s] => ("0", *m, *s),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    let (secs, millis) = s.split_once([',', '.']).unwrap_or((s, "0"));
    if millis.len() > 3 || [h, m, secs, millis].iter().any(|p| p.is_empty()) {
        return None;
    }
    let digits = |p: &str| p.chars().all(|c| c.is_ascii_digit());
    if ![h, m, secs, millis].iter().all(|p| digits(p)) {
        return None;
    }
    let fraction = format!("0.{millis}").parse::<f64>().ok()?;
    Some(
        h.parse::<f64>().ok()? * 3600.0
            + m.parse::<f64>().ok()? * 60.0
            + secs.parse::<f64>().ok()?
            + fraction,
    )
}

/// Convert cue markup to engine markup; returns the first voice name.
fn convert_text(raw: &str) -> (Option<String>, String) {
    let mut out = String::new();
    let mut voice = None;
    let mut rest = raw;
    while let Some(open) = rest.find('<') {
        push_plain(&mut out, &rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            push_plain(&mut out, &rest[open..]);
            rest = "";
            break;
        };
        let tag = after[..close].trim();
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '.')
            .next()
            .unwrap_or("");
        match (name, closing) {
            ("v", false) if voice.is_none() => {
                let who = tag[1..].trim().to_owned();
                if !who.is_empty() {
                    voice = Some(who);
                }
            }
            ("b" | "i" | "u", false) => {
                out.push('[');
                out.push_str(name);
                out.push(']');
            }
            ("b" | "i" | "u", true) => {
                out.push_str("[/");
                out.push_str(name);
                out.push(']');
            }
            _ => {}
        }
        rest = &after[close + 1..];
    }
    push_plain(&mut out, rest);
    (voice, out)
}

fn push_plain(out: &mut String, text: &str) {
    let decoded = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");
    for c in decoded.chars() {
        if matches!(c, '\\' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
}
