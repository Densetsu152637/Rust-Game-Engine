//! World-space text (`engine.WorldText`): shaped text laid out at a fixed
//! raster density and placed through the entity's world transform.
//!
//! Extraction runs the text through `engine-text` (rich markup, fallback chains,
//! bidirectional text), maps the glyph quads through
//! [`engine_text::transform_quads`] and emits one `DrawSpace::World` mesh per
//! atlas page, textured with the coverage atlas. Layout is y-down pixels, so the
//! quad transform is `world * scale(1/ppu, -1/ppu, 1) * translate(-anchor * box)`.
//!
//! `DrawSpace::World` draws use the 2D world camera. A depth-tested 3D path and
//! GPU SDF rendering need the render extension API, which this branch does not
//! have; `atlas: "sdf"` is accepted and reported through `TextRenderStatus`, but
//! rendered through a coverage atlas at the pixel size actually needed.
use crate::{
    ComponentDescriptor, Error, Result, Transform,
    content::{FontAtlasMode, Mesh, Texture, Vertex},
    runtime::AuthoredId,
    ui_tree::{EngineTextBackend, UiTextKind, UiTextRequest, validate_markup},
};
use bevy_ecs::prelude::{Component, World};
use engine_text::{TextAlign, transform_quads};
use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

pub const WORLD_TEXT_COMPONENT: &str = "engine.WorldText";
/// Longest `text` in bytes.
pub const MAX_WORLD_TEXT: usize = 4096;
/// Raster densities and pixel sizes stay within `engine-text`'s size limits.
const MIN_PIXELS: f32 = 1.0;
const MAX_PIXELS: f32 = 512.0;

/// Horizontal alignment inside the text box (`maxWidth`, or the widest line).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldTextAlign {
    /// Left for left-to-right paragraphs, right for right-to-left ones.
    #[default]
    Start,
    End,
    Left,
    Right,
    Center,
}

impl WorldTextAlign {
    fn engine(self) -> TextAlign {
        match self {
            Self::Start => TextAlign::Start,
            Self::End => TextAlign::End,
            Self::Left => TextAlign::Left,
            Self::Right => TextAlign::Right,
            Self::Center => TextAlign::Center,
        }
    }
}

/// Text placed in the world. Requires `Transform`; sized in world units.
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct WorldText {
    pub version: u32,
    pub text: String,
    /// Interpret `text` as rich-text markup (`[b]`, `[color=#f00]`, `[link=id]`, ...).
    pub markup: bool,
    /// Font family key: a `font_face` asset ID or `font_config` family; empty
    /// selects the configured default family.
    pub font: String,
    /// Height of one em in world units.
    pub size: f32,
    /// Straight-alpha RGBA in 0..=1 (markup may override per glyph).
    pub color: [f32; 4],
    pub align: WorldTextAlign,
    /// Wrap width in world units; absent never wraps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_width: Option<f32>,
    /// Ignore the entity's rotation so the text stays upright on screen.
    pub billboard: bool,
    /// Raster density in pixels per world unit; `size * pixelsPerUnit` is the
    /// rasterised pixel size (1..=512).
    pub pixels_per_unit: f32,
    /// The point of the text box placed at the entity origin: `[0, 0]` top-left,
    /// `[0.5, 0.5]` centre, `[1, 1]` bottom-right.
    pub anchor: [f32; 2],
    /// Requested glyph atlas; `sdf` is served by a coverage atlas for now.
    pub atlas: FontAtlasMode,
    pub layer: i32,
    pub visible: bool,
}

impl Default for WorldText {
    fn default() -> Self {
        Self {
            version: 1,
            text: String::new(),
            markup: false,
            font: String::new(),
            size: 1.0,
            color: [1.0; 4],
            align: WorldTextAlign::Start,
            max_width: None,
            billboard: false,
            pixels_per_unit: 32.0,
            anchor: [0.5, 0.5],
            atlas: FontAtlasMode::Coverage,
            layer: 0,
            visible: true,
        }
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new("world_text.invalid", message)
}

impl WorldText {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(invalid("Unsupported WorldText version"));
        }
        if self.text.len() > MAX_WORLD_TEXT || self.font.len() > 128 {
            return Err(invalid("text is at most 4096 bytes and font at most 128"));
        }
        if !(self.size.is_finite() && (0.001..=1000.0).contains(&self.size)) {
            return Err(invalid("size must be within 0.001..=1000 world units"));
        }
        if !(self.pixels_per_unit.is_finite()
            && (MIN_PIXELS..=MAX_PIXELS).contains(&self.pixels_per_unit))
        {
            return Err(invalid("pixelsPerUnit must be within 1..=512"));
        }
        let pixels = self.size * self.pixels_per_unit;
        if !(MIN_PIXELS..=MAX_PIXELS).contains(&pixels) {
            return Err(invalid(
                "size * pixelsPerUnit (the rasterised pixel size) must be within 1..=512",
            ));
        }
        if self
            .color
            .iter()
            .any(|c| !(c.is_finite() && (0.0..=1.0).contains(c)))
            || self
                .anchor
                .iter()
                .any(|a| !(a.is_finite() && (0.0..=1.0).contains(a)))
        {
            return Err(invalid("color and anchor components must be within 0..=1"));
        }
        if self
            .max_width
            .is_some_and(|w| !(w.is_finite() && (0.001..=1.0e6).contains(&w)))
        {
            return Err(invalid("maxWidth must be within 0.001..=1000000"));
        }
        if self.markup {
            validate_markup(&self.text)?;
        }
        Ok(())
    }
}

/// Authoring metadata for MCP `component_set`, scene documents and the editor.
pub fn descriptor() -> ComponentDescriptor {
    let unit = json!({"type":"number","minimum":0,"maximum":1});
    let array = |items: serde_json::Value, count: usize| json!({"type":"array","items":items,"minItems":count,"maxItems":count});
    ComponentDescriptor {
        name: WORLD_TEXT_COMPONENT.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "version":{"type":"integer","enum":[1]},
            "text":{"type":"string","maxLength":MAX_WORLD_TEXT},
            "markup":{"type":"boolean"},
            "font":{"type":"string","maxLength":128},
            "size":{"type":"number","minimum":0.001,"maximum":1000},
            "color":array(unit.clone(), 4),
            "align":{"type":"string","enum":["start","end","left","right","center"]},
            "maxWidth":{"type":"number","minimum":0.001,"maximum":1.0e6},
            "billboard":{"type":"boolean"},
            "pixelsPerUnit":{"type":"number","minimum":1,"maximum":512},
            "anchor":array(unit, 2),
            "atlas":{"type":"string","enum":["coverage","sdf"]},
            "layer":{"type":"integer","minimum":i32::MIN,"maximum":i32::MAX},
            "visible":{"type":"boolean"}
        }}),
        defaults: serde_json::to_value(WorldText::default()).expect("defaults serialize"),
        requires: vec!["Transform".into()],
        entity_references: vec![],
        data_only: false,
    }
}

pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<WorldText>(app, WORLD_TEXT_COMPONENT, 1)
}

/// One extracted world-text draw (a mesh per atlas page).
pub(crate) struct WorldTextDraw {
    pub id: String,
    pub layer: i32,
    pub texture: Arc<Texture>,
    pub mesh: Mesh,
    /// Entity position, for presentation-time interpolation.
    pub position: [f32; 3],
}

fn rotation_free(matrix: Mat4) -> Mat4 {
    let (scale, _, translation) = matrix.to_scale_rotation_translation();
    Mat4::from_scale_rotation_translation(scale, Quat::IDENTITY, translation)
}

/// The text-to-world matrix for a laid-out box of `box_size` layout pixels.
fn placement(text: &WorldText, world: Mat4, box_size: [f32; 2]) -> Mat4 {
    let base = if text.billboard {
        rotation_free(world)
    } else {
        world
    };
    let s = 1.0 / text.pixels_per_unit;
    base * Mat4::from_scale(Vec3::new(s, -s, 1.0))
        * Mat4::from_translation(Vec3::new(
            -text.anchor[0] * box_size[0],
            -text.anchor[1] * box_size[1],
            0.0,
        ))
}

/// Extract every visible `WorldText` as world-space draws. A project with
/// world text but no `font_face` assets fails with `render.font.unknown`.
pub(crate) fn extract(
    world: &World,
    engine: Option<&EngineTextBackend>,
) -> Result<Vec<WorldTextDraw>> {
    let mut draws = vec![];
    let localization = world.get_resource::<crate::localization::Localization>();
    for entity in world.iter_entities() {
        let (Some(text), Some(_)) = (entity.get::<WorldText>(), entity.get::<Transform>()) else {
            continue;
        };
        if !text.visible || text.text.is_empty() {
            continue;
        }
        text.validate()?;
        // `@key` text and per-locale font keys resolve here, not in the component.
        let resolved = localization.and_then(|l| l.resolve_text(&text.text, text.markup));
        let content = resolved.as_deref().unwrap_or(&text.text);
        let font = localization.map_or_else(|| text.font.clone(), |l| l.asset(&text.font));
        let engine = engine.ok_or_else(|| Error::new("render.font.unknown", &font))?;
        let id = entity
            .get::<AuthoredId>()
            .map(|a| a.id.clone())
            .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
        let transform = crate::world_transform(world, entity.id())?;
        if text.atlas == FontAtlasMode::Sdf || engine.wants_sdf(&font) {
            engine.note_sdf_request();
        }
        let pixels = text.size * text.pixels_per_unit;
        let family_size = engine
            .pixel_size(&font, 1.0)
            .ok_or_else(|| Error::new("render.font.unknown", &font))?;
        let request = UiTextRequest {
            text: content,
            kind: if text.markup {
                UiTextKind::Markup
            } else {
                UiTextKind::Plain
            },
            font: &font,
            scale: pixels / family_size,
            color: text.color,
            max_width: text.max_width.map(|w| w * text.pixels_per_unit),
        };
        let placed = engine.place_text(&request, text.align.engine())?;
        let box_width = request
            .max_width
            .unwrap_or(placed.bounds.width)
            .max(placed.bounds.width);
        let matrix = placement(
            text,
            Mat4::from_cols_array_2d(&transform.matrix),
            [box_width, placed.bounds.height],
        );
        let mut meshes: Vec<(u32, Mesh)> = vec![];
        for quad in transform_quads(&placed.quads, matrix) {
            let slot = match meshes.iter().position(|(page, _)| *page == quad.page) {
                Some(slot) => slot,
                None => {
                    meshes.push((quad.page, Mesh::default()));
                    meshes.len() - 1
                }
            };
            let mesh = &mut meshes[slot].1;
            let base = mesh.vertices.len() as u32;
            let color = quad.color.to_f32();
            let uvs = [
                [quad.uv.u0, quad.uv.v0],
                [quad.uv.u1, quad.uv.v0],
                [quad.uv.u1, quad.uv.v1],
                [quad.uv.u0, quad.uv.v1],
            ];
            for (corner, uv) in quad.corners.iter().zip(uvs) {
                mesh.vertices.push(Vertex {
                    position: corner.to_array(),
                    uv,
                    color,
                });
            }
            mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| i + base));
        }
        for (page, mesh) in meshes {
            let Some(texture) = placed.pages.get(page as usize) else {
                continue;
            };
            draws.push(WorldTextDraw {
                id: format!("worldtext:{id}:{page:02}"),
                layer: text.layer,
                texture: texture.clone(),
                mesh,
                position: transform.position(),
            });
        }
    }
    draws.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(draws)
}
