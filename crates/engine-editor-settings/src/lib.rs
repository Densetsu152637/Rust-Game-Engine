//! Per-project editor settings stored as `editor.json` beside `project.json`.
//!
//! The document is versioned (`schema`), rejects unknown fields, and every
//! numeric value and collection is bounded so a hostile or corrupt file cannot
//! drive the editor outside a sane range. The editor and the MCP tools share
//! this one validator.
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
};

/// File name of the settings document inside the project root.
pub const SETTINGS_FILE: &str = "editor.json";
/// Current document version.
pub const SCHEMA_VERSION: u32 = 1;
/// Largest accepted document, in bytes.
pub const MAX_DOCUMENT_BYTES: u64 = 256 * 1024;
/// Most bookmarks a document may hold.
pub const MAX_BOOKMARKS: usize = 64;
/// Longest bookmark name, in characters.
pub const MAX_BOOKMARK_NAME: usize = 64;

/// A settings validation or I/O failure with a precise message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError(pub String);
impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SettingsError {}
type Result<T> = std::result::Result<T, SettingsError>;
fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(SettingsError(message.into()))
}

/// Which manipulator the scene view shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GizmoMode {
    #[default]
    Translate,
    Rotate,
    Scale,
}
/// Whether gizmo axes follow the world or the active entity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GizmoSpace {
    #[default]
    World,
    Local,
}
/// Where a multi-selection gizmo is anchored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GizmoPivot {
    /// Centroid of the selected entities' world positions.
    #[default]
    Center,
    /// World position of the active (most recently selected) entity.
    Active,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Snapping {
    pub enabled: bool,
    /// World units per translate step.
    pub translate_step: f32,
    /// Degrees per rotate step.
    pub rotate_degrees: f32,
    /// Scale-factor increment per scale step.
    pub scale_step: f32,
}
impl Default for Snapping {
    fn default() -> Self {
        Self {
            enabled: false,
            translate_step: 0.5,
            rotate_degrees: 15.0,
            scale_step: 0.1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GridSettings {
    pub visible: bool,
    /// World units between grid lines.
    pub spacing: f32,
}
impl Default for GridSettings {
    fn default() -> Self {
        Self {
            visible: true,
            spacing: 1.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GizmoSettings {
    pub mode: GizmoMode,
    pub space: GizmoSpace,
    pub pivot: GizmoPivot,
}

/// A named scene-view camera.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraBookmark {
    pub name: String,
    pub three_dimensional: bool,
    pub center: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub pixels_per_unit: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorSettings {
    pub schema: u32,
    #[serde(default)]
    pub snapping: Snapping,
    #[serde(default)]
    pub grid: GridSettings,
    #[serde(default)]
    pub gizmo: GizmoSettings,
    #[serde(default)]
    pub bookmarks: Vec<CameraBookmark>,
}
impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            snapping: Snapping::default(),
            grid: GridSettings::default(),
            gizmo: GizmoSettings::default(),
            bookmarks: Vec::new(),
        }
    }
}

fn check_range(name: &str, value: f32, min: f32, max: f32) -> Result<()> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        fail(format!("{name} must be a finite number in {min}..={max}"))
    }
}

impl CameraBookmark {
    pub fn validate(&self) -> Result<()> {
        let length = self.name.chars().count();
        if length == 0 || length > MAX_BOOKMARK_NAME || self.name.chars().any(char::is_control) {
            return fail(format!(
                "bookmark name must be 1..={MAX_BOOKMARK_NAME} characters without control characters"
            ));
        }
        for value in self.center {
            check_range("bookmark center", value, -1.0e6, 1.0e6)?;
        }
        check_range("bookmark yaw", self.yaw, -1.0e4, 1.0e4)?;
        check_range("bookmark pitch", self.pitch, -1.57, 1.57)?;
        check_range("bookmark distance", self.distance, 0.05, 10_000.0)?;
        check_range(
            "bookmark pixels_per_unit",
            self.pixels_per_unit,
            0.05,
            4096.0,
        )
    }
}

impl EditorSettings {
    /// Checks version, ranges and collection bounds.
    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA_VERSION {
            return fail(format!(
                "unsupported editor settings schema {} (expected {SCHEMA_VERSION})",
                self.schema
            ));
        }
        check_range(
            "snapping.translate_step",
            self.snapping.translate_step,
            0.001,
            1000.0,
        )?;
        check_range(
            "snapping.rotate_degrees",
            self.snapping.rotate_degrees,
            0.1,
            180.0,
        )?;
        check_range("snapping.scale_step", self.snapping.scale_step, 0.001, 10.0)?;
        check_range("grid.spacing", self.grid.spacing, 0.001, 1000.0)?;
        if self.bookmarks.len() > MAX_BOOKMARKS {
            return fail(format!("at most {MAX_BOOKMARKS} bookmarks are allowed"));
        }
        for (index, bookmark) in self.bookmarks.iter().enumerate() {
            bookmark.validate()?;
            if self.bookmarks[..index]
                .iter()
                .any(|other| other.name == bookmark.name)
            {
                return fail(format!("duplicate bookmark name `{}`", bookmark.name));
            }
        }
        Ok(())
    }

    /// Parses and validates a document. Unknown fields are rejected.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            return fail(format!(
                "{SETTINGS_FILE} exceeds {MAX_DOCUMENT_BYTES} bytes"
            ));
        }
        let settings: Self = serde_json::from_slice(bytes)
            .map_err(|error| SettingsError(format!("invalid {SETTINGS_FILE}: {error}")))?;
        settings.validate()?;
        Ok(settings)
    }

    /// Path of the document for a project root directory.
    pub fn path(root: &Path) -> PathBuf {
        root.join(SETTINGS_FILE)
    }

    /// Loads `editor.json` from a project root. A missing file yields defaults;
    /// a present but invalid file is an error (never silently replaced).
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path(root);
        match std::fs::metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => fail(format!("cannot stat {}: {error}", path.display())),
            Ok(meta) if meta.len() > MAX_DOCUMENT_BYTES => fail(format!(
                "{SETTINGS_FILE} exceeds {MAX_DOCUMENT_BYTES} bytes"
            )),
            Ok(_) => {
                let bytes = std::fs::read(&path).map_err(|error| {
                    SettingsError(format!("cannot read {}: {error}", path.display()))
                })?;
                Self::parse(&bytes)
            }
        }
    }

    /// Serializes to the canonical pretty form.
    pub fn to_json(&self) -> Result<String> {
        self.validate()?;
        let mut text = serde_json::to_string_pretty(self)
            .map_err(|error| SettingsError(error.to_string()))?;
        text.push('\n');
        Ok(text)
    }

    /// Validates then writes atomically (temporary file in the same directory,
    /// then rename over the target).
    pub fn save(&self, root: &Path) -> Result<()> {
        let text = self.to_json()?;
        let path = Self::path(root);
        let temporary = root.join(format!("{SETTINGS_FILE}.tmp"));
        std::fs::write(&temporary, text).map_err(|error| {
            SettingsError(format!("cannot write {}: {error}", temporary.display()))
        })?;
        std::fs::rename(&temporary, &path).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            SettingsError(format!("cannot replace {}: {error}", path.display()))
        })
    }

    /// Applies a partial JSON object over this document. Top-level sections
    /// (`snapping`, `grid`, `gizmo`) merge field by field; `bookmarks`, when
    /// present, replaces the list. The result is validated; on error `self` is
    /// unchanged.
    pub fn merged(&self, patch: &serde_json::Value) -> Result<Self> {
        let serde_json::Value::Object(patch) = patch else {
            return fail("settings patch must be a JSON object");
        };
        let mut document =
            serde_json::to_value(self).map_err(|error| SettingsError(error.to_string()))?;
        let object = document
            .as_object_mut()
            .ok_or_else(|| SettingsError("settings are not an object".into()))?;
        for (key, value) in patch {
            match (object.get_mut(key), value) {
                (Some(serde_json::Value::Object(section)), serde_json::Value::Object(update)) => {
                    for (field, value) in update {
                        section.insert(field.clone(), value.clone());
                    }
                }
                _ => {
                    object.insert(key.clone(), value.clone());
                }
            }
        }
        let merged: Self = serde_json::from_value(document)
            .map_err(|error| SettingsError(format!("invalid settings patch: {error}")))?;
        merged.validate()?;
        Ok(merged)
    }

    pub fn bookmark(&self, name: &str) -> Option<&CameraBookmark> {
        self.bookmarks.iter().find(|bookmark| bookmark.name == name)
    }

    /// Saves a bookmark, replacing one with the same name.
    pub fn save_bookmark(&mut self, bookmark: CameraBookmark) -> Result<()> {
        bookmark.validate()?;
        if let Some(existing) = self.bookmarks.iter_mut().find(|b| b.name == bookmark.name) {
            *existing = bookmark;
            return Ok(());
        }
        if self.bookmarks.len() >= MAX_BOOKMARKS {
            return fail(format!("at most {MAX_BOOKMARKS} bookmarks are allowed"));
        }
        self.bookmarks.push(bookmark);
        Ok(())
    }

    /// Removes a bookmark; returns whether one existed.
    pub fn delete_bookmark(&mut self, name: &str) -> bool {
        let before = self.bookmarks.len();
        self.bookmarks.retain(|bookmark| bookmark.name != name);
        self.bookmarks.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bookmark(name: &str) -> CameraBookmark {
        CameraBookmark {
            name: name.into(),
            three_dimensional: true,
            center: [1.0, 2.0, 3.0],
            yaw: 0.5,
            pitch: -0.25,
            distance: 12.0,
            pixels_per_unit: 16.0,
        }
    }

    #[test]
    fn defaults_validate_and_round_trip() {
        let settings = EditorSettings::default();
        settings.validate().unwrap();
        let back = EditorSettings::parse(settings.to_json().unwrap().as_bytes()).unwrap();
        assert_eq!(back, settings);
    }

    #[test]
    fn unknown_fields_wrong_schema_and_out_of_range_are_rejected() {
        for text in [
            r#"{"schema":1,"extra":1}"#,
            r#"{"schema":1,"snapping":{"mystery":1}}"#,
            r#"{"schema":2}"#,
            r#"{"schema":1,"snapping":{"translate_step":0}}"#,
            r#"{"schema":1,"snapping":{"rotate_degrees":181}}"#,
            r#"{"schema":1,"grid":{"spacing":-1}}"#,
            r#"{"schema":1,"bookmarks":[{"name":"","three_dimensional":false,"center":[0,0,0],"yaw":0,"pitch":0,"distance":1,"pixels_per_unit":1}]}"#,
            r#"{"schema":1,"bookmarks":[{"name":"a","three_dimensional":false,"center":[0,0,0],"yaw":0,"pitch":2,"distance":1,"pixels_per_unit":1}]}"#,
            "not json",
        ] {
            assert!(EditorSettings::parse(text.as_bytes()).is_err(), "{text}");
        }
    }

    #[test]
    fn bookmark_lifecycle_and_bounds() {
        let mut settings = EditorSettings::default();
        settings.save_bookmark(bookmark("top")).unwrap();
        let mut moved = bookmark("top");
        moved.distance = 20.0;
        settings.save_bookmark(moved).unwrap();
        assert_eq!(settings.bookmarks.len(), 1);
        assert_eq!(settings.bookmark("top").unwrap().distance, 20.0);
        assert!(settings.delete_bookmark("top"));
        assert!(!settings.delete_bookmark("top"));
        for index in 0..MAX_BOOKMARKS {
            settings
                .save_bookmark(bookmark(&format!("b{index}")))
                .unwrap();
        }
        assert!(settings.save_bookmark(bookmark("overflow")).is_err());
        let duplicate = EditorSettings {
            bookmarks: vec![bookmark("a"), bookmark("a")],
            ..Default::default()
        };
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn merge_updates_sections_field_wise_and_rejects_bad_patches() {
        let settings = EditorSettings::default();
        let merged = settings
            .merged(&serde_json::json!({
                "snapping": {"enabled": true, "translate_step": 2.0},
                "gizmo": {"mode": "rotate"}
            }))
            .unwrap();
        assert!(merged.snapping.enabled);
        assert_eq!(merged.snapping.translate_step, 2.0);
        assert_eq!(merged.snapping.rotate_degrees, 15.0);
        assert_eq!(merged.gizmo.mode, GizmoMode::Rotate);
        assert!(
            settings
                .merged(&serde_json::json!({"grid": {"spacing": 0}}))
                .is_err()
        );
        assert!(settings.merged(&serde_json::json!({"unknown": 1})).is_err());
        assert!(settings.merged(&serde_json::json!(3)).is_err());
    }

    #[test]
    fn save_load_round_trip_and_missing_file_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            EditorSettings::load(dir.path()).unwrap(),
            EditorSettings::default()
        );
        let mut settings = EditorSettings::default();
        settings.snapping.enabled = true;
        settings.save_bookmark(bookmark("hero")).unwrap();
        settings.save(dir.path()).unwrap();
        assert_eq!(EditorSettings::load(dir.path()).unwrap(), settings);
        std::fs::write(EditorSettings::path(dir.path()), b"{\"schema\":1,\"bad\":0}").unwrap();
        assert!(EditorSettings::load(dir.path()).is_err());
    }
}
