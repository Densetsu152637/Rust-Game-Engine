//! Authored components of large-world coordinates: `engine.LargeWorld` (rebasing settings) and
//! `engine.AbsolutePosition` (f64 authoring of a root entity's position).
use super::WorldOrigin;
use crate::{ComponentDescriptor, Error, Parent, Result, Scene, Transform};
use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::prelude::*;
use bevy_ecs::world::DeferredWorld;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const LARGE_WORLD_NAME: &str = "engine.LargeWorld";
pub const ABSOLUTE_POSITION_NAME: &str = "engine.AbsolutePosition";
/// Largest absolute coordinate, in metres, accepted for positions and origin offsets. Every
/// multiple of a snap grid below it is exact in f64 (2^53 > 1e12 * 2^13).
pub const MAX_ABSOLUTE: f64 = 1.0e12;
/// Rebase thresholds accepted by `engine.LargeWorld`, in metres.
pub const THRESHOLD_RANGE: (f64, f64) = (1.0, 1.0e6);
/// Allowed snap grids: powers of two from 1 m to 65 536 m.
pub const SNAP_VALUES: [u32; 17] = [
    1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16_384, 32_768, 65_536,
];
const MAX_ANCHOR: usize = 128;

/// Floating-origin rebasing settings. At most one per scene.
///
/// Every tick, before the simulation, the coordinates service measures the anchor's position
/// relative to the current origin. When any axis exceeds `threshold`, the origin moves once to
/// the anchor's absolute position rounded to a multiple of `snap` (a power of two, so every
/// offset and every delta is exact in f64), and every root `Transform` moves by the opposite
/// delta.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LargeWorld {
    pub enabled: bool,
    /// Metres from the origin, on any axis, at which the anchor triggers a rebase.
    pub threshold: f64,
    /// Grid in metres the new origin snaps to; one of [`SNAP_VALUES`], at most `threshold`.
    pub snap: u32,
    /// Authored id of the anchor entity; empty uses the active `Camera3D` (lowest id).
    pub anchor: String,
}

impl Default for LargeWorld {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 2048.0,
            snap: 512,
            anchor: String::new(),
        }
    }
}

impl LargeWorld {
    /// Checks what the JSON schema cannot: `snap <= threshold` and finiteness.
    pub fn check(&self) -> Result<()> {
        let bad = |m: &str| Error::new("world.large_world.invalid", m.to_owned());
        let (low, high) = THRESHOLD_RANGE;
        if !(self.threshold.is_finite() && (low..=high).contains(&self.threshold)) {
            return Err(bad("threshold must be in [1, 1e6] metres"));
        }
        if !SNAP_VALUES.contains(&self.snap) {
            return Err(bad("snap must be a power of two from 1 to 65536 metres"));
        }
        if f64::from(self.snap) > self.threshold {
            return Err(bad("snap must not exceed threshold"));
        }
        if self.anchor.chars().count() > MAX_ANCHOR || self.anchor.chars().any(char::is_control) {
            return Err(bad(
                "anchor must be at most 128 characters, no control characters",
            ));
        }
        Ok(())
    }
}

/// The absolute position of a root entity in f64 metres.
///
/// When the component is inserted (scene start, streamed cells, PCG prefabs, native inserts)
/// the entity's local `Transform` position becomes `position - WorldOrigin.offset`; on every
/// rebase it is recomputed from the f64 value, so the entity never accumulates f32 rounding.
/// While the entity moves (physics, scripts) the value follows `offset + local`. Only root
/// entities (no `parent`) are converted; scene validation rejects it on children.
#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[component(on_insert = absolute_inserted)]
#[serde(default, deny_unknown_fields)]
pub struct AbsolutePosition {
    pub position: [f64; 3],
    /// The local `Transform` position the service last derived from `position`; a different
    /// `Transform` means the entity moved and `position` is stale.
    #[serde(skip)]
    pub(super) applied: Option<[f32; 3]>,
}

impl AbsolutePosition {
    pub fn new(position: [f64; 3]) -> Self {
        Self {
            position,
            applied: None,
        }
    }
    pub fn check(&self) -> Result<()> {
        if self
            .position
            .iter()
            .all(|v| v.is_finite() && v.abs() <= MAX_ABSOLUTE)
        {
            Ok(())
        } else {
            Err(Error::new(
                "world.absolute_position.invalid",
                "position must be finite with |x|, |y|, |z| <= 1e12 metres",
            ))
        }
    }
    /// Whether the entity's local position is still the one derived from `position`.
    pub(super) fn exact_for(&self, local: [f32; 3]) -> bool {
        self.applied == Some(local)
    }
}

/// Converts the f64 position into the local `Transform` as soon as the component lands.
fn absolute_inserted(mut world: DeferredWorld, context: HookContext) {
    let entity = context.entity;
    if world.get::<Parent>(entity).is_some() {
        return;
    }
    let offset = world
        .get_resource::<WorldOrigin>()
        .map_or([0.0; 3], |o| o.offset);
    let Some(absolute) = world.get::<AbsolutePosition>(entity) else {
        return;
    };
    if absolute.check().is_err() {
        return;
    }
    let local = WorldOrigin { offset }.local(absolute.position);
    let Some(mut transform) = world.get_mut::<Transform>(entity) else {
        return;
    };
    transform.position = local;
    if let Some(mut absolute) = world.get_mut::<AbsolutePosition>(entity) {
        absolute.applied = Some(local);
    }
}

pub(super) fn descriptors() -> Vec<ComponentDescriptor> {
    let absolute = json!({"type": "number", "minimum": -MAX_ABSOLUTE, "maximum": MAX_ABSOLUTE});
    vec![
        ComponentDescriptor {
            name: ABSOLUTE_POSITION_NAME.into(),
            version: 1,
            schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "position": {"type": "array", "minItems": 3, "maxItems": 3, "items": absolute,
                        "description": "Absolute position in f64 metres; the local Transform is position - WorldOrigin.offset."},
                },
            }),
            defaults: serde_json::to_value(AbsolutePosition::default()).expect("plain data"),
            requires: vec!["Transform".into()],
            entity_references: vec![],
            data_only: false,
        },
        ComponentDescriptor {
            name: LARGE_WORLD_NAME.into(),
            version: 1,
            schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "enabled": {"type": "boolean", "description": "Rebase automatically when the anchor passes the threshold."},
                    "threshold": {"type": "number", "minimum": THRESHOLD_RANGE.0, "maximum": THRESHOLD_RANGE.1,
                        "description": "Metres from the origin, on any axis, that trigger a rebase."},
                    "snap": {"type": "integer", "enum": SNAP_VALUES,
                        "description": "Power-of-two grid in metres the new origin snaps to; at most threshold."},
                    "anchor": {"type": "string", "maxLength": MAX_ANCHOR,
                        "description": "Authored id of the anchor entity; empty uses the active Camera3D."},
                },
            }),
            defaults: serde_json::to_value(LargeWorld::default()).expect("plain data"),
            requires: vec![],
            // Prefab instancing remaps the anchor id; an empty string means the active camera.
            entity_references: vec!["/anchor".into()],
            data_only: false,
        },
    ]
}

/// Scene validation: well-formed values, one `engine.LargeWorld`, `engine.AbsolutePosition`
/// only on root entities.
pub fn validate_scene(scene: &Scene) -> Result<()> {
    let mut settings = 0;
    for entity in &scene.entities {
        let custom = &entity.components.custom;
        if let Some(value) = custom.get(LARGE_WORLD_NAME) {
            let large: LargeWorld = serde_json::from_value(value.data.clone()).map_err(|e| {
                Error::new(
                    "world.large_world.invalid",
                    format!("entity {}: {e}", entity.id),
                )
            })?;
            large.check()?;
            settings += 1;
            if settings > 1 {
                return Err(Error::new(
                    "world.large_world.invalid",
                    "a scene holds at most one engine.LargeWorld",
                ));
            }
        }
        if let Some(value) = custom.get(ABSOLUTE_POSITION_NAME) {
            let absolute: AbsolutePosition =
                serde_json::from_value(value.data.clone()).map_err(|e| {
                    Error::new(
                        "world.absolute_position.invalid",
                        format!("entity {}: {e}", entity.id),
                    )
                })?;
            absolute.check()?;
            if entity.parent.is_some() {
                return Err(Error::new(
                    "world.absolute_position.invalid",
                    format!(
                        "entity {}: engine.AbsolutePosition is only valid on root entities",
                        entity.id
                    ),
                ));
            }
        }
    }
    Ok(())
}
