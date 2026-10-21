use crate::{Error, Result, input::ActionFrame};
use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};
pub type GlyphAdvance = Arc<dyn Fn(char) -> f32 + Send + Sync>;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl Default for Rect {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 360.0,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PanelOptions {
    pub id: String,
    pub font_asset: String,
    pub text: String,
    pub rect: Rect,
    pub color: [f32; 4],
    pub scale: f32,
    pub line_height: f32,
    pub glyph_advance: f32,
    pub layer: i32,
    pub modal: bool,
    pub dismissible: bool,
    pub choices: Vec<String>,
    pub confirm_action: String,
    pub cancel_action: String,
    pub up_action: String,
    pub down_action: String,
    pub pointer_action: String,
}
impl Default for PanelOptions {
    fn default() -> Self {
        Self {
            id: String::new(),
            font_asset: String::new(),
            text: String::new(),
            rect: Rect::default(),
            color: [1.0; 4],
            scale: 1.0,
            line_height: 20.0,
            glyph_advance: 10.0,
            layer: 0,
            modal: true,
            dismissible: true,
            choices: vec![],
            confirm_action: "ui_confirm".into(),
            cancel_action: "ui_back".into(),
            up_action: "ui_up".into(),
            down_action: "ui_down".into(),
            pointer_action: "ui_click".into(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PanelSnapshot {
    pub options: PanelOptions,
    pub scroll_offset: f32,
    pub content_height: f32,
    pub focused: bool,
    pub selected: usize,
    pub clip: Rect,
    pub lines: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiEvent {
    pub panel: String,
    pub kind: String,
    pub selection: usize,
}
#[derive(Clone, Default, Resource)]
pub struct UiModel {
    panels: Vec<PanelSnapshot>,
    events: VecDeque<UiEvent>,
    suppressed: BTreeSet<String>,
    release_suppressed: BTreeSet<String>,
    raw: ActionFrame,
    transition: bool,
    font_metrics: BTreeMap<String, GlyphAdvance>,
    panel_metrics: BTreeMap<String, GlyphAdvance>,
}
fn valid_rect(r: Rect) -> bool {
    [r.x, r.y, r.width, r.height].iter().all(|n| n.is_finite())
        && r.width > 0.0
        && r.height > 0.0
        && r.width <= 16384.0
        && r.height <= 16384.0
}
fn clamp_scroll(p: &mut PanelSnapshot) {
    p.scroll_offset = p
        .scroll_offset
        .clamp(0.0, (p.content_height - p.options.rect.height).max(0.0));
}
impl UiModel {
    pub fn panels(&self) -> &[PanelSnapshot] {
        &self.panels
    }
    pub fn modal_active(&self) -> bool {
        self.panels.iter().any(|p| p.options.modal)
    }
    fn capture(&mut self) {
        self.suppressed
            .extend(self.raw.held.iter().chain(&self.raw.pressed).cloned());
        self.transition = true;
    }
    fn layout(
        panel: &mut PanelSnapshot,
        advance: Option<&(dyn Fn(char) -> f32 + Send + Sync)>,
    ) -> Result<()> {
        let o = &panel.options;
        let row = o.line_height * o.scale;
        if !row.is_finite() || row <= 0.0 {
            return Err(Error::new("ui.layout.invalid", "Invalid row height"));
        }
        panel.lines.clear();
        let mut line = String::new();
        let mut width = 0.0;
        for c in o.text.chars() {
            if c == '\r' {
                continue;
            }
            if c == '\n' {
                panel.lines.push(std::mem::take(&mut line));
                width = 0.0;
            } else {
                let a = advance.map(|f| f(c)).unwrap_or(o.glyph_advance) * o.scale;
                if !a.is_finite() || a < 0.0 {
                    return Err(Error::new("ui.layout.invalid", "Invalid glyph advance"));
                }
                if width + a > o.rect.width && !line.is_empty() {
                    panel.lines.push(std::mem::take(&mut line));
                    width = 0.0;
                }
                line.push(c);
                width += a;
            }
            if panel.lines.len() >= 4095 {
                return Err(Error::new("ui.layout.limit", "Text exceeds 4096 lines"));
            }
        }
        panel.lines.push(line);
        panel.content_height = (panel.lines.len() + o.choices.len()) as f32 * row;
        if !panel.content_height.is_finite() {
            return Err(Error::new("ui.layout.invalid", "Invalid content height"));
        }
        panel.clip = o.rect;
        clamp_scroll(panel);
        Ok(())
    }
    pub fn open(&mut self, options: PanelOptions) -> Result<()> {
        let metrics = self.font_metrics.get(&options.font_asset).cloned();
        self.open_with_metrics(options, metrics)
    }
    pub fn set_font_metrics(&mut self, font_asset: impl Into<String>, advance: GlyphAdvance) {
        self.font_metrics.insert(font_asset.into(), advance);
    }
    pub fn open_with_metrics(
        &mut self,
        options: PanelOptions,
        advance: Option<GlyphAdvance>,
    ) -> Result<()> {
        if self.panels.len() >= 64
            || options.id.is_empty()
            || options.id.len() > 128
            || options.font_asset.len() > 128
            || options.text.len() > 65536
            || !valid_rect(options.rect)
            || !options.scale.is_finite()
            || options.scale <= 0.0
            || options.scale > 16.0
            || !options.line_height.is_finite()
            || options.line_height <= 0.0
            || options.line_height > 1024.0
            || !options.glyph_advance.is_finite()
            || options.glyph_advance <= 0.0
            || options.choices.len() > 64
            || options
                .color
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            || options
                .choices
                .iter()
                .any(|c| c.len() > 4096 || c.contains(['\r', '\n']))
            || self.panels.iter().any(|p| p.options.id == options.id)
        {
            return Err(Error::new(
                "ui.panel.invalid",
                "Panel exceeds limits, has duplicate ID or invalid metrics",
            ));
        }
        let mut panel = PanelSnapshot {
            clip: options.rect,
            options,
            scroll_offset: 0.0,
            content_height: 0.0,
            focused: true,
            selected: 0,
            lines: vec![],
        };
        Self::layout(&mut panel, advance.as_deref())?;
        if let Some(advance) = advance {
            self.panel_metrics.insert(panel.options.id.clone(), advance);
        }
        if let Some(last) = self.panels.last_mut() {
            last.focused = false;
        }
        self.panels.push(panel);
        self.capture();
        Ok(())
    }
    pub fn close(&mut self, id: &str) -> bool {
        if let Some(index) = self.panels.iter().position(|p| p.options.id == id) {
            self.capture();
            self.panels.remove(index);
            self.panel_metrics.remove(id);
            if let Some(last) = self.panels.last_mut() {
                last.focused = true;
            }
            true
        } else {
            false
        }
    }
    pub fn set_text(&mut self, id: &str, text: String) -> Result<()> {
        if text.len() > 65536 {
            return Err(Error::new("ui.text.limit", "Text exceeds 64KiB"));
        }
        let advance = self.panel_metrics.get(id).cloned();
        let panel = self
            .panels
            .iter_mut()
            .find(|p| p.options.id == id)
            .ok_or_else(|| Error::new("ui.panel.unknown", "Unknown panel"))?;
        let mut candidate = panel.clone();
        candidate.options.text = text;
        Self::layout(&mut candidate, advance.as_deref())?;
        *panel = candidate;
        Ok(())
    }
    pub fn resize(&mut self, id: &str, rect: Rect) -> Result<()> {
        if !valid_rect(rect) {
            return Err(Error::new("ui.panel.invalid", "Invalid panel rectangle"));
        }
        let advance = self.panel_metrics.get(id).cloned();
        let panel = self
            .panels
            .iter_mut()
            .find(|p| p.options.id == id)
            .ok_or_else(|| Error::new("ui.panel.unknown", "Unknown panel"))?;
        let mut candidate = panel.clone();
        candidate.options.rect = rect;
        Self::layout(&mut candidate, advance.as_deref())?;
        *panel = candidate;
        Ok(())
    }
    pub fn scroll(&mut self, id: &str, delta: f32) -> Result<()> {
        if !delta.is_finite() {
            return Err(Error::new("ui.scroll.invalid", "Invalid scroll delta"));
        }
        let panel = self
            .panels
            .iter_mut()
            .find(|p| p.options.id == id)
            .ok_or_else(|| Error::new("ui.panel.unknown", "Unknown panel"))?;
        panel.scroll_offset += delta;
        clamp_scroll(panel);
        Ok(())
    }
    pub fn clear(&mut self) {
        self.capture();
        self.panels.clear();
        self.events.clear();
        self.panel_metrics.clear();
    }
    pub fn poll_event(&mut self) -> Option<UiEvent> {
        self.events.pop_front()
    }
    pub fn tick(&mut self, input: ActionFrame) {
        self.raw = input;
        self.transition = false;
        self.release_suppressed.clear();
        self.suppressed.retain(|name| {
            let retain = self.raw.held.contains(name) || self.raw.pressed.contains(name);
            if !retain && self.raw.released.contains(name) {
                self.release_suppressed.insert(name.clone());
            }
            retain
        });
        if !self.raw.focused {
            return;
        }
        let Some(p) = self.panels.last_mut() else {
            return;
        };
        let pressed =
            |name: &str| self.raw.pressed.contains(name) && !self.suppressed.contains(name);
        if pressed(&p.options.cancel_action) && p.options.dismissible {
            let id = p.options.id.clone();
            self.events.push_back(UiEvent {
                panel: id.clone(),
                kind: "cancel".into(),
                selection: p.selected,
            });
            self.close(&id);
            return;
        }
        let row = p.options.line_height * p.options.scale;
        let up = pressed(&p.options.up_action);
        let down = pressed(&p.options.down_action);
        if !p.options.choices.is_empty() {
            let before = p.selected;
            if up {
                p.selected = (p.selected + p.options.choices.len() - 1) % p.options.choices.len();
            }
            if down {
                p.selected = (p.selected + 1) % p.options.choices.len();
            }
            if up || down {
                let top = (p.lines.len() + p.selected) as f32 * row;
                let bottom = top + row;
                if row > p.options.rect.height || top < p.scroll_offset {
                    p.scroll_offset = top;
                } else if bottom > p.scroll_offset + p.options.rect.height {
                    p.scroll_offset = bottom - p.options.rect.height;
                }
                clamp_scroll(p);
            }
            if before != p.selected {
                self.events.push_back(UiEvent {
                    panel: p.options.id.clone(),
                    kind: "selection".into(),
                    selection: p.selected,
                });
            }
        } else {
            if up {
                p.scroll_offset -= row;
            }
            if down {
                p.scroll_offset += row;
            }
            clamp_scroll(p);
        }
        let r = p.options.rect;
        let inside = self.raw.pointer[0] >= r.x
            && self.raw.pointer[0] < r.x + r.width
            && self.raw.pointer[1] >= r.y
            && self.raw.pointer[1] < r.y + r.height;
        if inside && self.raw.wheel[1].is_finite() {
            p.scroll_offset -= self.raw.wheel[1] * row * 3.0;
            clamp_scroll(p);
        }
        let mut click = pressed(&p.options.pointer_action) && inside;
        if click && !p.options.choices.is_empty() {
            let index = (self.raw.pointer[1] - r.y + p.scroll_offset) / row - p.lines.len() as f32;
            if !index.is_finite() || index < 0.0 || index >= p.options.choices.len() as f32 {
                click = false;
            } else {
                p.selected = index as usize;
                self.events.push_back(UiEvent {
                    panel: p.options.id.clone(),
                    kind: "selection".into(),
                    selection: p.selected,
                });
            }
        }
        if pressed(&p.options.confirm_action) || click {
            self.events.push_back(UiEvent {
                panel: p.options.id.clone(),
                kind: "confirm".into(),
                selection: p.selected,
            });
        }
        while self.events.len() > 256 {
            self.events.pop_front();
        }
    }
    pub fn gameplay_frame(&self) -> ActionFrame {
        if self.modal_active() || self.transition {
            return ActionFrame {
                focused: self.raw.focused,
                ..Default::default()
            };
        }
        let mut frame = self.raw.clone();
        for name in &self.suppressed {
            frame.pressed.remove(name);
            frame.held.remove(name);
            frame.released.remove(name);
            frame.values.remove(name);
        }
        for name in &self.release_suppressed {
            frame.released.remove(name);
        }
        frame
    }
}
