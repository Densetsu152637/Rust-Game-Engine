use super::*;
use crate::{
    content::{self, FontAtlas, Mesh, Texture, Vertex},
    runtime::{Draw, DrawSpace, DrawState},
};
use std::{collections::BTreeMap, sync::Arc};

impl UiRoot {
    /// Extract native render draws. Text requires a font asset; an empty font is
    /// valid only on widgets without text. Missing assets are explicit errors.
    pub fn extract_draws(
        &self,
        id: &str,
        viewport: [u32; 2],
        focus: Option<&str>,
        fonts: &BTreeMap<String, FontAtlas>,
        textures: &BTreeMap<String, Arc<Texture>>,
    ) -> Result<Vec<Draw>> {
        self.validate()?;
        if !self.visible {
            return Ok(vec![]);
        }
        // Hex plus a low-sorting terminator preserves root lexical order even
        // for IDs that are prefixes of one another or contain punctuation.
        let root_key: String = id.bytes().map(|byte| format!("{byte:02x}")).collect();
        let mut draws = vec![];
        for (order, bounds) in self.tree.layout(viewport)?.iter().enumerate() {
            let node = &self.tree.nodes[bounds.node];
            let clip = intersection(bounds.rect, bounds.clip);
            if clip.width <= 0.0 || clip.height <= 0.0 {
                continue;
            }
            let key = format!("game-ui:{root_key}!{order:04}");
            let mut geometry = Mesh::default();
            quad(&mut geometry, bounds.rect, node.style.background);
            let inner = layout::inset(bounds.rect, node.style.padding);
            match &node.widget {
                UiWidget::Toggle { checked: true, .. } => {
                    let size = inner.height.min(12.0).min(inner.width);
                    quad(
                        &mut geometry,
                        Rect {
                            width: size,
                            height: size,
                            ..inner
                        },
                        node.style.accent,
                    );
                }
                UiWidget::Slider {
                    value, min, max, ..
                }
                | UiWidget::Progress { value, min, max } => {
                    let ratio = (*value - *min) / (*max - *min);
                    quad(
                        &mut geometry,
                        Rect {
                            width: inner.width * ratio,
                            ..inner
                        },
                        node.style.accent,
                    );
                    if matches!(node.widget, UiWidget::Slider { .. }) {
                        let knob = 4.0_f32.min(inner.width);
                        quad(
                            &mut geometry,
                            Rect {
                                x: inner.x + (inner.width - knob) * ratio,
                                width: knob,
                                ..inner
                            },
                            node.style.foreground,
                        );
                    }
                }
                _ => {}
            }
            if self.enabled && bounds.enabled && focus == Some(node.id.as_str()) {
                outline(&mut geometry, bounds.rect, node.style.accent);
            }
            if !self.enabled || !bounds.enabled {
                for v in &mut geometry.vertices {
                    v.color[3] *= 0.5;
                }
            }
            if !geometry.indices.is_empty() {
                draws.push(draw(format!("{key}:0"), geometry, None, self.layer, clip));
            }
            if let Some(text) = node.widget.text().filter(|s| !s.is_empty()) {
                let font = fonts
                    .get(&node.style.font)
                    .ok_or_else(|| Error::new("render.font.unknown", &node.style.font))?;
                let texture = textures
                    .get(&font.texture)
                    .ok_or_else(|| Error::new("render.texture.unknown", &font.texture))?
                    .clone();
                let mut origin = [inner.x, inner.y];
                let mut width = inner.width;
                if matches!(node.widget, UiWidget::Toggle { .. }) {
                    let indent = 16.0_f32.min(width);
                    origin[0] += indent;
                    width -= indent;
                }
                if width > 0.0 && inner.height > 0.0 {
                    let mut color = node.style.foreground;
                    if !self.enabled || !bounds.enabled {
                        color[3] *= 0.5;
                    }
                    let mesh = content::text_mesh(
                        font,
                        &texture,
                        text,
                        origin,
                        width,
                        node.style.font_scale,
                        color,
                    )?;
                    draws.push(draw(
                        format!("{key}:1"),
                        mesh,
                        Some(texture),
                        self.layer,
                        intersection(inner, clip),
                    ));
                }
            }
        }
        Ok(draws)
    }
}

fn draw(id: String, mesh: Mesh, texture: Option<Arc<Texture>>, layer: i32, clip: Rect) -> Draw {
    Draw {
        id,
        mesh: Arc::new(mesh),
        texture,
        layer,
        space: DrawSpace::Ui,
        clip: Some([clip.x, clip.y, clip.width, clip.height]),
        state: DrawState::default(),
        motion: None,
        offset: [0.0; 3],
    }
}

fn quad(mesh: &mut Mesh, r: Rect, color: [f32; 4]) {
    if r.width <= 0.0 || r.height <= 0.0 || color[3] <= 0.0 {
        return;
    }
    let base = mesh.vertices.len() as u32;
    for [x, y] in [
        [r.x, r.y],
        [r.x + r.width, r.y],
        [r.x + r.width, r.y + r.height],
        [r.x, r.y + r.height],
    ] {
        mesh.vertices.push(Vertex {
            position: [x, y, 0.0],
            uv: [0.0; 2],
            color,
        });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| i + base));
}

fn outline(mesh: &mut Mesh, r: Rect, color: [f32; 4]) {
    let thickness = 2.0_f32.min(r.width / 2.0).min(r.height / 2.0);
    quad(
        mesh,
        Rect {
            height: thickness,
            ..r
        },
        color,
    );
    quad(
        mesh,
        Rect {
            y: r.y + r.height - thickness,
            height: thickness,
            ..r
        },
        color,
    );
    quad(
        mesh,
        Rect {
            width: thickness,
            ..r
        },
        color,
    );
    quad(
        mesh,
        Rect {
            x: r.x + r.width - thickness,
            width: thickness,
            ..r
        },
        color,
    );
}
