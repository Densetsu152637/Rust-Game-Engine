//! Authored opt-in lighting for sprites and World tile chunks.
use crate::{ComponentDescriptor, Error, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const LIGHT: &str = "engine.Light2D";
pub const SPRITE: &str = "engine.SpriteLighting2D";
pub const CAMERA: &str = "engine.Lighting2D";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Light2DKind {
    Point,
    Directional,
}

#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Light2D {
    pub kind: Light2DKind,
    pub color: [f32; 3],
    /// Lux for directional light, candela for point light.
    pub intensity: f32,
    pub range: f32,
    /// World-space direction in which directional rays travel.
    pub direction: [f32; 3],
    pub enabled: bool,
}
impl Default for Light2D {
    fn default() -> Self {
        Self {
            kind: Light2DKind::Point,
            color: [1.0; 3],
            intensity: 1.0,
            range: 8.0,
            direction: [0.0, 0.0, -1.0],
            enabled: true,
        }
    }
}
impl Light2D {
    pub fn validate(&self) -> Result<()> {
        if !self
            .color
            .iter()
            .all(|x| x.is_finite() && (0.0..=1.0).contains(x))
            || !self.intensity.is_finite()
            || !(0.0..=1_000_000.0).contains(&self.intensity)
            || !self.range.is_finite()
            || !(0.0..=1_000_000.0).contains(&self.range)
            || !self
                .direction
                .iter()
                .all(|x| x.is_finite() && x.abs() <= 1_000_000.0)
            || (self.kind == Light2DKind::Point && self.range == 0.0)
            || (self.kind == Light2DKind::Directional
                && self.direction.iter().map(|x| x * x).sum::<f32>() <= 1e-12)
        {
            return Err(Error::new(
                "render.light2d.invalid",
                "Light2D requires finite colour, intensity, range and nonzero directional vector",
            ));
        }
        Ok(())
    }
}

#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpriteLighting2D {
    pub enabled: bool,
}
impl Default for SpriteLighting2D {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Lighting2D {
    pub enabled: bool,
    pub ambient: [f32; 3],
    pub exposure: f32,
}
impl Default for Lighting2D {
    fn default() -> Self {
        Self {
            enabled: false,
            ambient: [0.1; 3],
            exposure: 1.0,
        }
    }
}
impl Lighting2D {
    pub fn validate(&self) -> Result<()> {
        if !self
            .ambient
            .iter()
            .all(|x| x.is_finite() && (0.0..=1_000_000.0).contains(x))
            || !self.exposure.is_finite()
            || !(0.0..=1_000.0).contains(&self.exposure)
        {
            return Err(Error::new(
                "render.lighting2d.invalid",
                "Lighting2D ambient and exposure must be finite and bounded",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct LightSnapshot2D {
    pub id: String,
    pub light: Light2D,
    pub position: [f32; 3],
}
#[derive(Clone, Debug)]
pub struct LightingSnapshot2D {
    pub ambient: [f32; 3],
    pub exposure: f32,
    pub lights: Vec<LightSnapshot2D>,
}

pub fn descriptors() -> Vec<ComponentDescriptor> {
    let vector = |n, max| json!({"type":"array","items":{"type":"number","minimum":0,"maximum":max},"minItems":n,"maxItems":n});
    vec![
        ComponentDescriptor {
            name: LIGHT.into(),
            version: 1,
            schema: json!({"type":"object","additionalProperties":false,"properties":{
                "kind":{"type":"string","enum":["point","directional"]},
                "color":vector(3,1),"intensity":{"type":"number","minimum":0,"maximum":1000000},
                "range":{"type":"number","minimum":0,"maximum":1000000},
                "direction":{"type":"array","items":{"type":"number","minimum":-1000000,"maximum":1000000},"minItems":3,"maxItems":3},
                "enabled":{"type":"boolean"}}}),
            defaults: json!(Light2D::default()),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
        ComponentDescriptor {
            name: SPRITE.into(),
            version: 1,
            schema: json!({"type":"object","additionalProperties":false,"properties":{"enabled":{"type":"boolean"}}}),
            defaults: json!({"enabled":true}),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
        ComponentDescriptor {
            name: CAMERA.into(),
            version: 1,
            schema: json!({"type":"object","additionalProperties":false,"properties":{
                "enabled":{"type":"boolean"},"ambient":vector(3,1000000),
                "exposure":{"type":"number","minimum":0,"maximum":1000}}}),
            defaults: json!(Lighting2D::default()),
            requires: vec!["Camera2D".into()],
            entity_references: vec![],
            data_only: false,
        },
    ]
}

pub fn register(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<Light2D>(app, LIGHT, 1)?;
    crate::register_component::<SpriteLighting2D>(app, SPRITE, 1)?;
    crate::register_component::<Lighting2D>(app, CAMERA, 1)
}

pub fn validate_component(name: &str, value: &serde_json::Value) -> Result<()> {
    match name {
        LIGHT => serde_json::from_value::<Light2D>(value.clone())?.validate(),
        CAMERA => serde_json::from_value::<Lighting2D>(value.clone())?.validate(),
        SPRITE => {
            serde_json::from_value::<SpriteLighting2D>(value.clone())?;
            Ok(())
        }
        _ => Ok(()),
    }
}
