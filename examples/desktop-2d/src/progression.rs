//! Fixture-owned pure progression. No presentation operation awards progress.
use engine_project::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const PASSAGE: &str = "passage:sample";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub version: u32,
    #[serde(default)]
    pub content_version: u32,
    pub variant: String,
    pub room: String,
    #[serde(deserialize_with = "flags")]
    pub events: BTreeSet<String>,
    pub quests: BTreeMap<String, String>,
    #[serde(deserialize_with = "flags")]
    pub reveals: BTreeSet<String>,
    #[serde(default, deserialize_with = "provenance_flags")]
    pub provenance: BTreeMap<String, BTreeSet<String>>,
    pub scene: String,
    pub flock: bool,
    pub followers: u32,
    pub phase: u32,
    pub mode: String,
    #[serde(default)]
    pub position: Option<SavedPosition>,
    #[serde(default)]
    pub accepted_choice: Option<u32>,
    #[serde(default)]
    pub heal_completions: u32,
    #[serde(default)]
    pub restore_pending: bool,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedPosition {
    pub x: f32,
    pub y: f32,
}
impl Progress {
    pub fn new(variant: &str) -> Result<Self> {
        if !matches!(variant, "male" | "female") {
            return Err(invalid("unknown fixture variant"));
        }
        Ok(Self {
            version: 2,
            content_version: 1,
            variant: variant.into(),
            room: "room:A".into(),
            events: BTreeSet::new(),
            quests: BTreeMap::from([("sample".into(), "available".into())]),
            reveals: BTreeSet::new(),
            provenance: BTreeMap::new(),
            scene: "idle".into(),
            flock: false,
            followers: 2,
            phase: 0,
            mode: "interactive".into(),
            position: None,
            accepted_choice: None,
            heal_completions: 0,
            restore_pending: false,
        })
    }
    pub fn restore(value: serde_json::Value) -> Result<Self> {
        let mut restored: Self = serde_json::from_value(value)?;
        if restored.version == 1 {
            restored.version = 2;
            restored.content_version = 1;
        }
        restored.validate()?;
        Ok(restored)
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 2 || self.content_version != 1 {
            return Err(invalid("unsupported snapshot/content version"));
        }
        if !matches!(self.variant.as_str(), "male" | "female")
            || !matches!(self.room.as_str(), "room:A" | "room:B")
        {
            return Err(invalid("unknown variant or room ID"));
        }
        if !matches!(
            self.quests.get("sample").map(String::as_str),
            Some("available" | "active" | "completed" | "reward_recorded")
        ) {
            return Err(invalid("unknown quest state"));
        }
        if self.reveals.iter().any(|p| p != PASSAGE) {
            return Err(invalid("unknown passage ID"));
        }
        if self
            .position
            .is_some_and(|p| !p.x.is_finite() || !p.y.is_finite())
        {
            return Err(invalid("invalid saved position"));
        }
        if self.accepted_choice.is_some_and(|c| c != 1 && c != 2) {
            return Err(invalid("invalid saved choice"));
        }
        Ok(())
    }
    pub fn event(&mut self, event: &str) -> Result<bool> {
        if self.events.contains(event) {
            return Ok(false);
        }
        if event == "quest:reward" && self.quests["sample"] != "completed" {
            return Ok(false);
        }
        if event == "scene:healed" && !self.events.contains("scene:injury") {
            return Err(invalid("healing requires authored injury"));
        }
        self.events.insert(event.into());
        match event {
            "quest:accept" => {
                if self.quests["sample"] == "available" {
                    self.quests.insert("sample".into(), "active".into());
                }
                if self.events.contains("quest:objective") && self.quests["sample"] == "active" {
                    self.quests.insert("sample".into(), "completed".into());
                }
            }
            "quest:objective" => {
                if self.quests["sample"] == "active" {
                    self.quests.insert("sample".into(), "completed".into());
                }
            }
            "quest:reward" => {
                self.quests
                    .insert("sample".into(), "reward_recorded".into());
                return Ok(self.reveal("quest:sample"));
            }
            "story:reveal" => return Ok(self.reveal("story:sample")),
            "encounter:defeat" => self.scene = "retry".into(),
            "scene:injury" => self.scene = "injury".into(),
            "scene:healed" => self.scene = "idle".into(),
            _ => {}
        }
        Ok(true)
    }
    fn reveal(&mut self, source: &str) -> bool {
        let changed = self.reveals.insert(PASSAGE.into());
        self.provenance
            .entry(PASSAGE.into())
            .or_default()
            .insert(source.into());
        changed
    }
    pub fn page(&self) -> String {
        if self.reveals.contains(PASSAGE) {
            format!(
                "DEVELOPER PASSAGE A\nOriginal placeholder text. No scripture.\nA shared reward retains its stable ID and both sources.\nUnicode coverage: Ω 十\n{}",
                "Long page: scroll to inspect clipped text and return safely.\n".repeat(12)
            )
        } else {
            "UNREVEALED\n[locked placeholder]\nNo readable passage is exposed. Confirm or Back to exit.".into()
        }
    }
}
fn invalid(message: &str) -> Error {
    Error::new("example.progress.invalid", message)
}

fn parse_flags(value: serde_json::Value) -> std::result::Result<BTreeSet<String>, String> {
    match value {
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "flag ID must be a string".to_owned())
            })
            .collect(),
        serde_json::Value::Object(values) => values
            .into_iter()
            .filter_map(|(id, value)| match value.as_bool() {
                Some(true) => Some(Ok(id)),
                Some(false) => None,
                None => Some(Err("flag values must be boolean".to_owned())),
            })
            .collect(),
        _ => Err("progress flags must be an array or object".into()),
    }
}
fn flags<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeSet<String>, D::Error> {
    parse_flags(serde_json::Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}
fn provenance_flags<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, BTreeSet<String>>, D::Error> {
    let values = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|(id, value)| {
            parse_flags(value)
                .map(|flags| (id, flags))
                .map_err(serde::de::Error::custom)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fresh(variant: &str) -> Progress {
        Progress::new(variant).unwrap()
    }
    fn completed(variant: &str) -> Progress {
        let mut s = fresh(variant);
        s.event("quest:accept").unwrap();
        s.event("quest:objective").unwrap();
        s
    }
    fn rewarded(variant: &str) -> Progress {
        let mut s = completed(variant);
        s.event("quest:reward").unwrap();
        s.event("story:reveal").unwrap();
        s
    }
    fn legacy_v1(s: &Progress) -> serde_json::Value {
        let mut value = serde_json::to_value(s).unwrap();
        value["version"] = 1.into();
        value.as_object_mut().unwrap().remove("contentVersion");
        value
    }

    #[test]
    fn new_progress_starts_available_in_room_a_with_two_followers() {
        let s = fresh("male");
        assert_eq!((s.version, s.content_version), (2, 1));
        assert_eq!(s.room, "room:A");
        assert_eq!(s.quests["sample"], "available");
        assert_eq!(s.followers, 2);
        assert_eq!(s.scene, "idle");
        assert!(s.events.is_empty() && s.reveals.is_empty() && s.provenance.is_empty());
        assert!(s.position.is_none() && s.accepted_choice.is_none());
        assert_eq!((s.heal_completions, s.restore_pending), (0, false));
    }
    #[test]
    fn unknown_variant_is_rejected_and_both_placeholders_work() {
        assert!(Progress::new("other").is_err());
        assert!(Progress::new("").is_err());
        assert_eq!(fresh("female").variant, "female");
        assert_eq!(fresh("male").variant, "male");
    }
    #[test]
    fn locked_page_hides_text_until_reveal() {
        let mut s = fresh("male");
        assert!(s.page().starts_with("UNREVEALED"));
        assert!(!s.page().contains("Original placeholder"));
        s.event("quest:reward").unwrap();
        assert!(!s.page().contains("Original placeholder"));
        let s = rewarded("male");
        assert!(s.page().contains("Original placeholder"));
        assert!(s.page().contains('Ω') && s.page().contains('十'));
        assert!(s.page().lines().count() > 12, "long enough to scroll");
    }
    #[test]
    fn accept_makes_quest_active_and_objective_completes_it() {
        let mut s = fresh("male");
        assert!(s.event("quest:accept").unwrap());
        assert_eq!(s.quests["sample"], "active");
        assert!(s.event("quest:objective").unwrap());
        assert_eq!(s.quests["sample"], "completed");
    }
    #[test]
    fn objective_before_accept_is_remembered_and_completes_on_accept() {
        let mut s = fresh("female");
        s.event("quest:objective").unwrap();
        assert_eq!(s.quests["sample"], "available");
        s.event("quest:accept").unwrap();
        assert_eq!(s.quests["sample"], "completed");
    }
    #[test]
    fn premature_reward_is_ignored_and_not_recorded() {
        let mut s = fresh("male");
        assert!(!s.event("quest:reward").unwrap());
        assert!(!s.events.contains("quest:reward"));
        s.event("quest:accept").unwrap();
        assert!(!s.event("quest:reward").unwrap());
        assert_eq!(s.quests["sample"], "active");
        assert!(s.reveals.is_empty());
    }
    #[test]
    fn reward_is_awarded_once_with_provenance() {
        let mut s = completed("male");
        assert!(s.event("quest:reward").unwrap());
        assert_eq!(s.quests["sample"], "reward_recorded");
        assert!(!s.event("quest:reward").unwrap());
        assert!(s.reveals.contains(PASSAGE));
        assert_eq!(s.provenance[PASSAGE].len(), 1);
        assert!(s.provenance[PASSAGE].contains("quest:sample"));
    }
    #[test]
    fn duplicate_acceptance_reports_no_change() {
        let mut s = fresh("male");
        assert!(s.event("quest:accept").unwrap());
        assert!(!s.event("quest:accept").unwrap());
        assert_eq!(s.quests["sample"], "active");
    }
    #[test]
    fn shared_passage_keeps_both_quest_and_story_sources() {
        for story_first in [false, true] {
            let mut s = completed("male");
            if story_first {
                s.event("story:reveal").unwrap();
                s.event("quest:reward").unwrap();
            } else {
                s.event("quest:reward").unwrap();
                s.event("story:reveal").unwrap();
            }
            assert_eq!(s.reveals.len(), 1);
            assert_eq!(s.provenance[PASSAGE].len(), 2);
            assert!(s.page().contains("Original placeholder"));
        }
    }
    #[test]
    fn story_reveal_is_idempotent() {
        let mut s = fresh("male");
        assert!(s.event("story:reveal").unwrap());
        assert!(!s.event("story:reveal").unwrap());
        assert_eq!(s.reveals.len(), 1);
    }
    #[test]
    fn defeat_enters_retry_without_healing() {
        let mut s = fresh("male");
        s.event("encounter:defeat").unwrap();
        assert_eq!(s.scene, "retry");
        assert!(!s.events.contains("scene:healed"));
        assert_eq!(s.followers, 2);
    }
    #[test]
    fn healing_requires_the_authored_injury() {
        let mut s = fresh("male");
        s.event("encounter:defeat").unwrap();
        assert!(s.event("scene:healed").is_err());
        assert_eq!(s.scene, "retry");
        s.event("scene:injury").unwrap();
        assert_eq!(s.scene, "injury");
        assert!(s.event("scene:healed").unwrap());
        assert_eq!(s.scene, "idle");
        assert!(!s.event("scene:healed").unwrap());
    }
    #[test]
    fn serialization_round_trips_for_both_variants() {
        for variant in ["male", "female"] {
            let s = rewarded(variant);
            let value = serde_json::to_value(&s).unwrap();
            assert_eq!(value["contentVersion"], 1);
            assert_eq!(Progress::restore(value).unwrap(), s);
        }
    }
    #[test]
    fn version_one_snapshots_migrate_to_two() {
        let migrated = Progress::restore(legacy_v1(&rewarded("female"))).unwrap();
        assert_eq!((migrated.version, migrated.content_version), (2, 1));
        assert_eq!(migrated.variant, "female");
    }
    #[test]
    fn legacy_boolean_id_maps_migrate_and_drop_false_entries() {
        let mut legacy = legacy_v1(&fresh("male"));
        legacy["events"] = json!({"quest:accept": true, "quest:objective": true, "ignored": false});
        legacy["reveals"] = json!({"passage:sample": true});
        legacy["provenance"] =
            json!({"passage:sample": {"quest:sample": true, "story:sample": true}});
        let restored = Progress::restore(legacy).unwrap();
        assert!(restored.events.contains("quest:accept"));
        assert!(!restored.events.contains("ignored"));
        assert_eq!(restored.provenance[PASSAGE].len(), 2);
    }
    #[test]
    fn unsupported_versions_are_rejected() {
        for version in [0, 3, 99] {
            let mut value = serde_json::to_value(fresh("male")).unwrap();
            value["version"] = version.into();
            assert!(Progress::restore(value).is_err(), "version {version}");
        }
        let mut value = serde_json::to_value(fresh("male")).unwrap();
        value["contentVersion"] = 7.into();
        assert!(Progress::restore(value).is_err());
    }
    #[test]
    fn unknown_ids_are_rejected_on_validation() {
        let mut s = fresh("male");
        s.reveals.insert("passage:missing".into());
        assert!(s.validate().is_err());
        let mut s = fresh("male");
        s.room = "room:C".into();
        assert!(s.validate().is_err());
        let mut s = fresh("male");
        s.variant = "unknown".into();
        assert!(s.validate().is_err());
        let mut s = fresh("male");
        s.quests.insert("sample".into(), "cheated".into());
        assert!(s.validate().is_err());
    }
    #[test]
    fn malformed_flag_values_are_rejected() {
        for events in [
            json!("text"),
            json!(5),
            json!({"quest:accept": "yes"}),
            json!([1]),
        ] {
            let mut value = serde_json::to_value(fresh("male")).unwrap();
            value["events"] = events;
            assert!(Progress::restore(value).is_err());
        }
    }
    #[test]
    fn missing_required_fields_are_rejected() {
        let mut value = serde_json::to_value(fresh("male")).unwrap();
        value.as_object_mut().unwrap().remove("quests");
        assert!(Progress::restore(value).is_err());
        assert!(Progress::restore(json!({})).is_err());
        assert!(Progress::restore(json!(null)).is_err());
    }
    #[test]
    fn saved_checkpoint_survives_restore_without_aliasing() {
        let mut s = rewarded("male");
        s.room = "room:B".into();
        s.position = Some(SavedPosition { x: 3.25, y: -2.25 });
        s.accepted_choice = Some(2);
        let mut restored = Progress::restore(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(restored.room, "room:B");
        assert_eq!(restored.position, Some(SavedPosition { x: 3.25, y: -2.25 }));
        assert_eq!(restored.accepted_choice, Some(2));
        restored.position.as_mut().unwrap().x = 99.0;
        assert_eq!(s.position.unwrap().x, 3.25);
    }
    #[test]
    fn invalid_checkpoint_data_is_rejected() {
        let mut s = fresh("male");
        s.accepted_choice = Some(99);
        assert!(s.validate().is_err());
        s.accepted_choice = Some(0);
        assert!(s.validate().is_err());
        s.accepted_choice = Some(1);
        assert!(s.validate().is_ok());
        s.position = Some(SavedPosition {
            x: f32::NAN,
            y: 0.0,
        });
        assert!(s.validate().is_err());
        s.position = Some(SavedPosition {
            x: 0.0,
            y: f32::INFINITY,
        });
        assert!(s.validate().is_err());
        let mut value = serde_json::to_value(fresh("male")).unwrap();
        value["position"] = json!({"x": "bad", "y": 0});
        assert!(Progress::restore(value).is_err());
    }
}
