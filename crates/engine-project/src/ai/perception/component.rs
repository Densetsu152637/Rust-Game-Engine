//! The authored perception components.
use crate::{ComponentDescriptor, Error, Result};
use bevy_ecs::prelude::*;
use engine_ai::perception::{
    Affiliation, AffiliationFilter, DamageConfig, HearingConfig, SightConfig, TeamTable,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PERCEPTION_SENSOR: &str = "engine.PerceptionSensor";
pub const PERCEPTION_STIMULUS: &str = "engine.PerceptionStimulus";
pub const PERCEPTION_NOISE: &str = "engine.PerceptionNoise";
pub const PERCEPTION_SETTINGS: &str = "engine.PerceptionSettings";

/// Most team relations the settings may list.
pub const MAX_RELATIONS: usize = 256;
/// Most rays one tick may cast, whatever the settings say.
pub const MAX_RAYS_PER_TICK: u32 = 4096;

fn invalid(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

fn bounded(name: &str, value: f32, min: f32, max: f32) -> Result<()> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(invalid(
            "perception.value.invalid",
            format!("{name} must be {min} to {max}"),
        ))
    }
}

/// Blackboard keys a sensor keeps current on the entity's behavior tree or state
/// tree agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SensorBlackboard {
    /// `entity` key: authored id of the best target (cleared when none is known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// `vec3` key: last known position of the best target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
    /// `bool` key: whether the best target is sensed right now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensed: Option<String>,
    /// Affiliations the best target may have (default: hostile).
    pub affiliations: AffiliationFilter,
}

impl Default for SensorBlackboard {
    fn default() -> Self {
        Self {
            target: None,
            position: None,
            sensed: None,
            affiliations: AffiliationFilter::HOSTILE,
        }
    }
}

/// What an entity perceives with.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PerceptionSensor {
    pub enabled: bool,
    /// Team id; 0 is "no team".
    pub team: u32,
    /// Present to enable the sense (`{}` for the defaults).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sight: Option<SightConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hearing: Option<HearingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub damage: Option<DamageConfig>,
    /// Local forward axis of the entity (default -Z).
    pub forward: [f32; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blackboard: Option<SensorBlackboard>,
}

impl Default for PerceptionSensor {
    fn default() -> Self {
        Self {
            enabled: true,
            team: 0,
            sight: None,
            hearing: None,
            damage: None,
            forward: [0.0, 0.0, -1.0],
            blackboard: None,
        }
    }
}

impl PerceptionSensor {
    pub(crate) fn check(&self) -> Result<()> {
        let wrap = |r: std::result::Result<(), String>| {
            r.map_err(|m| invalid("perception.value.invalid", m))
        };
        if let Some(s) = &self.sight {
            wrap(s.validate())?;
        }
        if let Some(s) = &self.hearing {
            wrap(s.validate())?;
        }
        if let Some(s) = &self.damage {
            wrap(s.validate())?;
        }
        if self.forward.iter().any(|v| !v.is_finite())
            || self.forward.iter().map(|v| v * v).sum::<f32>() < 1e-8
        {
            return Err(invalid(
                "perception.value.invalid",
                "forward must be a finite non-zero vector",
            ));
        }
        if let Some(b) = &self.blackboard {
            for key in [&b.target, &b.position, &b.sensed].into_iter().flatten() {
                if key.is_empty() || key.len() > 64 {
                    return Err(invalid(
                        "perception.value.invalid",
                        "blackboard keys are 1-64 characters",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// What can be perceived.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PerceptionStimulus {
    pub enabled: bool,
    /// Team id; 0 is "no team".
    pub team: u32,
    /// Whether sight can see the entity.
    pub visible: bool,
    /// Height of the point sight aims at above the entity origin.
    pub height_offset: f32,
}

impl Default for PerceptionStimulus {
    fn default() -> Self {
        Self {
            enabled: true,
            team: 0,
            visible: true,
            height_offset: 0.0,
        }
    }
}

impl PerceptionStimulus {
    pub(crate) fn check(&self) -> Result<()> {
        bounded("height_offset", self.height_offset, -1000.0, 1000.0)
    }
}

/// An entity that makes noise at its position.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PerceptionNoise {
    pub enabled: bool,
    /// 0 to 1.
    pub loudness: f32,
    /// Distance at which the noise fades to nothing.
    pub range: f32,
    /// Seconds between noises; 0 makes one every tick.
    pub interval: f32,
}

impl Default for PerceptionNoise {
    fn default() -> Self {
        Self {
            enabled: true,
            loudness: 1.0,
            range: 20.0,
            interval: 1.0,
        }
    }
}

impl PerceptionNoise {
    pub(crate) fn check(&self) -> Result<()> {
        bounded("loudness", self.loudness, 0.0, 1.0)?;
        bounded(
            "range",
            self.range,
            f32::MIN_POSITIVE,
            engine_ai::perception::MAX_RANGE,
        )?;
        bounded("interval", self.interval, 0.0, 3600.0)
    }
}

/// A symmetric relation between two teams.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamRelation {
    pub a: u32,
    pub b: u32,
    pub affiliation: Affiliation,
}

/// How occluders are found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Occlusion {
    /// Ray casts against the 3D physics colliders.
    #[default]
    Physics3d,
    /// Nothing occludes.
    None,
}

/// Scene-wide perception settings (at most one entity carries them).
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PerceptionSettings {
    /// Relation of two different non-zero teams without an entry.
    pub default_affiliation: Affiliation,
    /// Relation of any pair involving team 0 without an entry.
    pub unaffiliated: Affiliation,
    pub relations: Vec<TeamRelation>,
    /// Sight rays per tick, shared by all perceivers (round-robin beyond that).
    pub max_rays_per_tick: u32,
    pub occlusion: Occlusion,
    /// Names of the physics layers that occlude; empty means every layer.
    pub occlusion_layers: Vec<String>,
}

impl Default for PerceptionSettings {
    fn default() -> Self {
        Self {
            default_affiliation: Affiliation::Hostile,
            unaffiliated: Affiliation::Neutral,
            relations: Vec::new(),
            max_rays_per_tick: 64,
            occlusion: Occlusion::Physics3d,
            occlusion_layers: Vec::new(),
        }
    }
}

impl PerceptionSettings {
    pub(crate) fn check(&self) -> Result<()> {
        if self.relations.len() > MAX_RELATIONS {
            return Err(invalid(
                "perception.value.invalid",
                format!("more than {MAX_RELATIONS} team relations"),
            ));
        }
        if self.max_rays_per_tick > MAX_RAYS_PER_TICK {
            return Err(invalid(
                "perception.value.invalid",
                format!("max_rays_per_tick is at most {MAX_RAYS_PER_TICK}"),
            ));
        }
        if self.occlusion_layers.len() > 32 {
            return Err(invalid(
                "perception.value.invalid",
                "at most 32 occlusion layers",
            ));
        }
        Ok(())
    }

    pub fn team_table(&self) -> TeamTable {
        let mut table = TeamTable::with_defaults(self.default_affiliation, self.unaffiliated);
        for relation in &self.relations {
            table.set(relation.a, relation.b, relation.affiliation);
        }
        table
    }
}

fn senses_schema() -> Value {
    let affiliations = json!({"type":"array","items":{"type":"string","enum":["hostile","neutral","friendly"]},
        "description":"Which affiliations the sense reacts to."});
    json!({
        "sight": {"type":"object", "additionalProperties": false, "properties": {
            "range": {"type":"number","minimum":0},
            "lose_range": {"type":"number","minimum":0,
                "description":"Range at which a sensed target is lost; at least range."},
            "half_angle": {"type":"number","minimum":0,"maximum":180},
            "peripheral_half_angle": {"type":"number","minimum":0,"maximum":180,
                "description":"Cone kept while a target is sensed; at least half_angle."},
            "height_offset": {"type":"number"},
            "max_age": {"type":"number","minimum":0,"maximum":3600},
            "affiliations": affiliations
        }},
        "hearing": {"type":"object", "additionalProperties": false, "properties": {
            "range": {"type":"number","minimum":0},
            "threshold": {"type":"number","minimum":0,"maximum":1},
            "max_age": {"type":"number","minimum":0,"maximum":3600},
            "affiliations": affiliations
        }},
        "damage": {"type":"object", "additionalProperties": false, "properties": {
            "max_age": {"type":"number","minimum":0,"maximum":3600},
            "affiliations": affiliations
        }}
    })
}

/// Catalog descriptors of the perception components.
pub fn descriptors() -> Vec<ComponentDescriptor> {
    let mut sensor_props = senses_schema();
    let props = sensor_props.as_object_mut().expect("object");
    props.insert("enabled".into(), json!({"type":"boolean"}));
    props.insert("team".into(), json!({"type":"integer","minimum":0}));
    props.insert(
        "forward".into(),
        json!({"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3,
            "description":"Local forward axis (default -Z)."}),
    );
    props.insert(
        "blackboard".into(),
        json!({"type":"object", "additionalProperties": false, "properties": {
            "target": {"type":"string","maxLength":64,"description":"entity key for the best target"},
            "position": {"type":"string","maxLength":64,"description":"vec3 key for its last known position"},
            "sensed": {"type":"string","maxLength":64,"description":"bool key: sensed right now"},
            "affiliations": {"type":"array","items":{"type":"string","enum":["hostile","neutral","friendly"]}}
        }}),
    );
    vec![
        ComponentDescriptor {
            name: PERCEPTION_SENSOR.into(),
            version: 1,
            schema: json!({"type":"object","additionalProperties":false,"properties":sensor_props}),
            defaults: serde_json::to_value(PerceptionSensor::default()).expect("finite defaults"),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
        ComponentDescriptor {
            name: PERCEPTION_STIMULUS.into(),
            version: 1,
            schema: json!({"type":"object","additionalProperties":false,"properties":{
                "enabled": {"type":"boolean"},
                "team": {"type":"integer","minimum":0},
                "visible": {"type":"boolean","description":"Whether sight can see the entity."},
                "height_offset": {"type":"number","description":"Height of the point sight aims at."}
            }}),
            defaults: serde_json::to_value(PerceptionStimulus::default()).expect("finite defaults"),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
        ComponentDescriptor {
            name: PERCEPTION_NOISE.into(),
            version: 1,
            schema: json!({"type":"object","additionalProperties":false,"properties":{
                "enabled": {"type":"boolean"},
                "loudness": {"type":"number","minimum":0,"maximum":1},
                "range": {"type":"number","minimum":0},
                "interval": {"type":"number","minimum":0,"maximum":3600,
                    "description":"Seconds between noises; 0 makes one every tick."}
            }}),
            defaults: serde_json::to_value(PerceptionNoise::default()).expect("finite defaults"),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
        ComponentDescriptor {
            name: PERCEPTION_SETTINGS.into(),
            version: 1,
            schema: json!({"type":"object","additionalProperties":false,"properties":{
                "default_affiliation": {"type":"string","enum":["hostile","neutral","friendly"]},
                "unaffiliated": {"type":"string","enum":["hostile","neutral","friendly"]},
                "relations": {"type":"array","maxItems":MAX_RELATIONS,"items":{
                    "type":"object","additionalProperties":false,"required":["a","b","affiliation"],
                    "properties":{"a":{"type":"integer","minimum":0},"b":{"type":"integer","minimum":0},
                        "affiliation":{"type":"string","enum":["hostile","neutral","friendly"]}}}},
                "max_rays_per_tick": {"type":"integer","minimum":0,"maximum":MAX_RAYS_PER_TICK},
                "occlusion": {"type":"string","enum":["physics3d","none"]},
                "occlusion_layers": {"type":"array","maxItems":32,"items":{"type":"string"}}
            }}),
            defaults: serde_json::to_value(PerceptionSettings::default()).expect("finite defaults"),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
    ]
}
