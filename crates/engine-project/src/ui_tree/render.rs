//! Baseline draw extraction through the native `runtime::Draw` path. Visual
//! styling beyond these defaults belongs to the styling layer.
use super::{
    chrome::{self, Popup},
    layout::{DROPDOWN_ARROW, TOGGLE_INDENT, display_text, text_request},
    text_edit::UiPreedit,
    *,
};
use crate::{
    content::{FontAtlas, Mesh, Texture, Vertex},
    runtime::{Draw, DrawSpace, DrawState},
};
use std::{collections::BTreeMap, sync::Arc};

/// Draw layer of open modal dialogs (and their backdrop).
pub const UI_DIALOG_LAYER: i32 = i32::MAX - 1;
/// Draw layer of dropdown popups and tooltips.
pub const UI_POPUP_LAYER: i32 = i32::MAX;

/// Interaction state one root needs for drawing (see `GameUi::render_state`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiRenderState {
    pub focus: Option<String>,
    pub pressed: Option<String>,
    /// Open dropdown: node, highlighted option, first visible option.
    pub popup: Option<(String, u32, u32)>,
    pub preedit: Option<(String, UiPreedit)>,
    /// Tooltip owner, text and anchor point.
    pub tooltip: Option<(String, String, [f32; 2])>,
    /// Active dialog and its highlighted button.
    pub dialog_choice: Option<(String, u32)>,
    /// The root is drawn from a theme; state looks come from its rules, so the
    /// built-in pressed darkening is skipped.
    pub themed: bool,
    /// Per-item text of virtual-list template nodes (`itemText` bindings).
    pub item_texts: UiItemTexts,
}

impl UiRoot {
    /// Extract native render draws with only a focus highlight. Text requires a
    /// font asset; an empty font is valid only on widgets without text.
    /// Missing assets are explicit errors.
    pub fn extract_draws(
        &self,
        id: &str,
        viewport: [u32; 2],
        focus: Option<&str>,
        fonts: &BTreeMap<String, FontAtlas>,
        textures: &BTreeMap<String, Arc<Texture>>,
    ) -> Result<Vec<Draw>> {
        let state = UiRenderState {
            focus: focus.map(str::to_owned),
            ..Default::default()
        };
        self.extract_draws_with(
            id,
            viewport,
            &state,
            &BitmapTextBackend::new(fonts, textures),
            textures,
        )
    }

    /// Extract draws using interaction `state`, laying text out with `text`.
    /// `textures` resolves image and nine-slice widgets.
    pub fn extract_draws_with(
        &self,
        id: &str,
        viewport: [u32; 2],
        state: &UiRenderState,
        text: &dyn UiTextBackend,
        textures: &BTreeMap<String, Arc<Texture>>,
    ) -> Result<Vec<Draw>> {
        self.validate()?;
        if !self.visible {
            return Ok(vec![]);
        }
        let laid = self.tree.laid(viewport, text)?;
        // Hex plus a low-sorting terminator preserves root lexical order even
        // for IDs that are prefixes of one another or contain punctuation.
        let root_key: String = id.bytes().map(|byte| format!("{byte:02x}")).collect();
        let layer_key = (self.layer as i64 - i32::MIN as i64) as u32;
        let mut painter = Painter {
            root: self,
            laid: &laid,
            state,
            text,
            textures,
            viewport,
            draws: vec![],
            latest: vec![None; self.tree.nodes.len()],
            xf: PaintTransform::IDENTITY,
        };
        let mut backdrop = false;
        for (order, bounds) in laid.boxes.iter().enumerate() {
            let (prefix, layer) = if bounds.overlay {
                (
                    format!("game-ui:~{layer_key:08x}{root_key}!"),
                    UI_DIALOG_LAYER,
                )
            } else {
                (format!("game-ui:{root_key}!"), self.layer)
            };
            if bounds.overlay && !backdrop {
                backdrop = true;
                let mut mesh = Mesh::default();
                quad(&mut mesh, viewport_rect(viewport), [0.0, 0.0, 0.0, 0.5]);
                painter.push(
                    format!("{prefix}00000:"),
                    mesh,
                    None,
                    layer,
                    viewport_rect(viewport),
                );
            }
            painter.node(&format!("{prefix}{order:05}"), layer, bounds)?;
        }
        if let Some((node, highlight, first)) = &state.popup {
            painter.popup(&root_key, layer_key, node, *highlight, *first)?;
        }
        if let Some((node, tip, at)) = &state.tooltip {
            painter.tooltip(&root_key, layer_key, node, tip, *at)?;
        }
        Ok(painter.draws)
    }
}

struct Painter<'a> {
    root: &'a UiRoot,
    laid: &'a layout::Laid,
    state: &'a UiRenderState,
    text: &'a dyn UiTextBackend,
    textures: &'a BTreeMap<String, Arc<Texture>>,
    viewport: [u32; 2],
    draws: Vec<Draw>,
    /// Border box of the most recently painted instance of each node, for
    /// scale pivots of ancestors.
    latest: Vec<Option<Rect>>,
    /// Paint transform of the node being painted (`UiStyle::scale`).
    xf: PaintTransform,
}

/// `p -> scale * p + translate`: the composed paint scales of a node's
/// ancestors-or-self, each about its own border-box centre.
#[derive(Clone, Copy)]
struct PaintTransform {
    scale: f32,
    translate: [f32; 2],
}

impl PaintTransform {
    const IDENTITY: Self = Self {
        scale: 1.0,
        translate: [0.0; 2],
    };
    fn is_identity(&self) -> bool {
        self.scale == 1.0 && self.translate == [0.0; 2]
    }
}

/// Glyph quads grouped by atlas page.
#[derive(Default)]
struct TextBatch(Vec<(Arc<Texture>, Mesh)>);

impl TextBatch {
    fn add(&mut self, layout: &UiTextLayout, origin: [f32; 2], alpha: f32) {
        for glyph in &layout.glyphs {
            let Some(page) = layout.pages.get(glyph.page as usize) else {
                continue;
            };
            let slot = match self.0.iter().position(|(p, _)| Arc::ptr_eq(p, page)) {
                Some(slot) => slot,
                None => {
                    self.0.push((page.clone(), Mesh::default()));
                    self.0.len() - 1
                }
            };
            let mut color = glyph.color;
            color[3] *= alpha;
            let rect = Rect {
                x: glyph.rect.x + origin[0],
                y: glyph.rect.y + origin[1],
                ..glyph.rect
            };
            textured(&mut self.0[slot].1, rect, glyph.uv, color);
        }
    }
}

impl Painter<'_> {
    fn push(
        &mut self,
        id: String,
        mesh: Mesh,
        texture: Option<Arc<Texture>>,
        layer: i32,
        clip: Rect,
    ) {
        if mesh.indices.is_empty() {
            return;
        }
        let (mut mesh, mut clip) = (mesh, clip);
        if !self.xf.is_identity() {
            let PaintTransform { scale, translate } = self.xf;
            for v in &mut mesh.vertices {
                v.position[0] = v.position[0] * scale + translate[0];
                v.position[1] = v.position[1] * scale + translate[1];
            }
            clip = Rect {
                x: clip.x * scale + translate[0],
                y: clip.y * scale + translate[1],
                width: clip.width * scale,
                height: clip.height * scale,
            };
        }
        self.draws.push(Draw {
            id,
            mesh: Arc::new(mesh),
            texture,
            layer,
            space: DrawSpace::Ui,
            clip: Some([clip.x, clip.y, clip.width, clip.height]),
            state: DrawState::default(),
            motion: None,
            offset: [0.0; 3],
        });
    }

    fn push_text(&mut self, key: &str, batch: TextBatch, layer: i32, clip: Rect) {
        for (page, (texture, mesh)) in batch.0.into_iter().enumerate() {
            self.push(
                format!("{key}:2{page:02}"),
                mesh,
                Some(texture),
                layer,
                clip,
            );
        }
    }

    fn texture(&self, key: &str) -> Result<Arc<Texture>> {
        self.textures
            .get(key)
            .cloned()
            .ok_or_else(|| Error::new("render.texture.unknown", key))
    }

    /// Inline images of a laid-out rich label, grouped by texture.
    fn inline_images(
        &self,
        layout: &UiTextLayout,
        origin: [f32; 2],
        alpha: f32,
    ) -> Result<Vec<(Arc<Texture>, Mesh)>> {
        let mut groups: Vec<(Arc<Texture>, Mesh)> = vec![];
        for image in &layout.images {
            let page = self.texture(&image.key)?;
            let slot = match groups.iter().position(|(p, _)| Arc::ptr_eq(p, &page)) {
                Some(slot) => slot,
                None => {
                    groups.push((page, Mesh::default()));
                    groups.len() - 1
                }
            };
            let rect = Rect {
                x: image.rect.x + origin[0],
                y: image.rect.y + origin[1],
                ..image.rect
            };
            textured(
                &mut groups[slot].1,
                rect,
                [0.0, 0.0, 1.0, 1.0],
                [1.0, 1.0, 1.0, alpha],
            );
        }
        Ok(groups)
    }

    fn layout_text(
        &self,
        node: &UiNode,
        text: &str,
        kind: UiTextKind,
        width: Option<f32>,
    ) -> Result<UiTextLayout> {
        self.text.layout(&text_request(node, text, kind, width))
    }

    /// Opacity product and paint transform inherited by `node` from itself and
    /// its ancestors (`UiStyle::opacity` and `UiStyle::scale`).
    fn paint_inheritance(&self, node: usize) -> (f32, PaintTransform) {
        let nodes = &self.root.tree.nodes;
        let mut opacity = 1.0;
        let mut xf = PaintTransform::IDENTITY;
        let mut cursor = Some(node);
        while let Some(n) = cursor {
            let style = &nodes[n].style;
            opacity *= style.opacity;
            if style.scale != 1.0
                && let Some(rect) = self.latest[n]
            {
                let k = style.scale;
                let c = [rect.x + rect.width / 2.0, rect.y + rect.height / 2.0];
                xf.scale *= k;
                xf.translate = [
                    xf.translate[0] * k + c[0] * (1.0 - k),
                    xf.translate[1] * k + c[1] * (1.0 - k),
                ];
            }
            cursor = self.laid.hierarchy.parents[n];
        }
        (opacity, xf)
    }

    fn node(&mut self, key: &str, layer: i32, bounds: &UiBox) -> Result<()> {
        let node = &self.root.tree.nodes[bounds.node];
        self.latest[bounds.node] = Some(bounds.rect);
        // Virtual-list replicas show their bound per-item text.
        let patched;
        let node = match bounds
            .item
            .and_then(|item| self.state.item_texts.text(&node.id, item))
        {
            Some(text) => {
                patched = node.with_bound_text(text);
                &patched
            }
            None => node,
        };
        let clip = bounds.visible();
        if clip.width <= 0.0 || clip.height <= 0.0 {
            return Ok(());
        }
        let (opacity, xf) = self.paint_inheritance(bounds.node);
        if opacity <= 0.0 {
            return Ok(());
        }
        self.xf = xf;
        let style = &node.style;
        let enabled = self.root.enabled && bounds.enabled;
        let alpha = (if enabled { 1.0 } else { 0.5 }) * opacity;
        let focused = enabled
            && bounds.item.is_none()
            && self.state.focus.as_deref() == Some(node.id.as_str());
        let pressed =
            bounds.item.is_none() && self.state.pressed.as_deref() == Some(node.id.as_str());
        let inner = bounds.content;
        let mut chrome = Mesh::default();
        let mut top = Mesh::default();
        let mut batch = TextBatch::default();
        let mut background = style.background;
        if pressed && !self.state.themed {
            for c in &mut background[..3] {
                *c *= 0.8;
            }
        }
        if !matches!(node.widget, UiWidget::NineSlice { .. }) {
            quad(&mut chrome, bounds.rect, background);
        }
        let mut image = None;
        let mut inline = vec![];
        match &node.widget {
            UiWidget::Toggle { checked: true, .. } => {
                let size = inner.height.min(12.0).min(inner.width);
                quad(
                    &mut chrome,
                    Rect {
                        width: size,
                        height: size,
                        ..inner
                    },
                    style.accent,
                );
            }
            UiWidget::Slider {
                value, min, max, ..
            }
            | UiWidget::Progress { value, min, max } => {
                let ratio = (*value - *min) / (*max - *min);
                quad(
                    &mut chrome,
                    Rect {
                        width: inner.width * ratio,
                        ..inner
                    },
                    style.accent,
                );
                if matches!(node.widget, UiWidget::Slider { .. }) {
                    let knob = 4.0_f32.min(inner.width);
                    quad(
                        &mut chrome,
                        Rect {
                            x: inner.x + (inner.width - knob) * ratio,
                            width: knob,
                            ..inner
                        },
                        style.foreground,
                    );
                }
            }
            UiWidget::Image {
                texture,
                uv,
                tint,
                fit,
            } => {
                let page = self.texture(texture)?;
                let (rect, uv) = fit_image(inner, &page, *uv, *fit);
                let mut mesh = Mesh::default();
                textured(&mut mesh, rect, uv, *tint);
                image = Some((mesh, page));
            }
            UiWidget::NineSlice {
                texture,
                slices,
                border,
                center,
                tint,
            } => {
                let page = self.texture(texture)?;
                let mut mesh = Mesh::default();
                nine_slice(
                    &mut mesh,
                    bounds.rect,
                    &page,
                    *slices,
                    border.unwrap_or(*slices),
                    *center,
                    *tint,
                );
                image = Some((mesh, page));
            }
            UiWidget::Dropdown { .. } => {
                let size = 8.0_f32.min(inner.width).min(inner.height);
                quad(
                    &mut chrome,
                    Rect {
                        x: inner.x + inner.width - DROPDOWN_ARROW / 2.0 - size / 2.0,
                        y: inner.y + (inner.height - size) / 2.0,
                        width: size,
                        height: size,
                    },
                    style.accent,
                );
            }
            UiWidget::ScrollView { .. } => {
                for (track, thumb) in chrome::scrollbars(bounds, node).into_iter().flatten() {
                    let mut faint = style.foreground;
                    faint[3] *= 0.15;
                    quad(&mut chrome, track, faint);
                    quad(&mut chrome, thumb, style.accent);
                }
            }
            UiWidget::Tabs {
                labels,
                selected,
                header_height,
                ..
            } => {
                for (i, cell) in chrome::tab_cells(bounds, labels.len(), *header_height)
                    .into_iter()
                    .enumerate()
                {
                    if i == *selected as usize {
                        quad(&mut chrome, cell, style.accent);
                    }
                    if !labels[i].is_empty() && cell.width > 8.0 {
                        let layout = self.layout_text(
                            node,
                            &labels[i],
                            UiTextKind::Plain,
                            Some(cell.width - 8.0),
                        )?;
                        batch.add(&layout, [cell.x + 4.0, cell.y + 4.0], alpha);
                    }
                }
            }
            UiWidget::ModalDialog {
                title,
                buttons,
                title_height,
                button_height,
                ..
            } => {
                let bar = chrome::dialog_title(bounds, *title_height);
                quad(&mut chrome, bar, style.accent);
                if !title.is_empty() && bar.width > 8.0 {
                    let layout =
                        self.layout_text(node, title, UiTextKind::Plain, Some(bar.width - 8.0))?;
                    batch.add(&layout, [bar.x + 4.0, bar.y + 4.0], alpha);
                }
                let choice = self
                    .state
                    .dialog_choice
                    .as_ref()
                    .filter(|(n, _)| *n == node.id)
                    .map(|(_, c)| *c as usize);
                for (i, cell) in chrome::dialog_buttons(bounds, buttons.len(), *button_height)
                    .into_iter()
                    .enumerate()
                {
                    let mut fill = style.accent;
                    fill[3] *= 0.6;
                    quad(&mut chrome, cell, fill);
                    if choice == Some(i) {
                        outline(&mut top, cell, style.foreground);
                    }
                    if !buttons[i].is_empty() && cell.width > 8.0 {
                        let layout = self.layout_text(
                            node,
                            &buttons[i],
                            UiTextKind::Plain,
                            Some(cell.width - 8.0),
                        )?;
                        batch.add(&layout, [cell.x + 4.0, cell.y + 4.0], alpha);
                    }
                }
            }
            _ => {}
        }
        // Selection highlight of list items and virtual-list replicas.
        if let Some(parent) = self.laid.hierarchy.parents[bounds.node] {
            let selected = match &self.root.tree.nodes[parent].widget {
                UiWidget::List {
                    selected: Some(s), ..
                } => self.laid.hierarchy.children[parent].get(*s as usize) == Some(&bounds.node),
                UiWidget::VirtualList { selected, .. } => {
                    selected.is_some() && bounds.item == *selected
                }
                _ => false,
            };
            if selected {
                outline(
                    &mut top,
                    bounds.rect,
                    self.root.tree.nodes[parent].style.accent,
                );
            }
        }
        // Text.
        let text_clip = intersection(inner, clip);
        if let UiWidget::TextInput { .. } = &node.widget {
            let preedit = self
                .state
                .preedit
                .as_ref()
                .filter(|(n, _)| *n == node.id)
                .map(|(_, p)| p);
            if let Some(view) = chrome::input_view(node, bounds, self.text, preedit)? {
                let mut selection = style.accent;
                selection[3] *= 0.4;
                for rect in &view.selection {
                    quad(&mut chrome, *rect, selection);
                }
                for rect in &view.preedit {
                    quad(&mut top, *rect, style.foreground);
                }
                if focused {
                    quad(&mut top, view.caret, style.foreground);
                }
                let fade = if view.placeholder { 0.5 } else { 1.0 };
                batch.add(&view.layout, view.origin, alpha * fade);
            }
        } else if let Some((content, kind, indent, _)) =
            display_text(node).filter(|t| !t.0.is_empty())
        {
            let indent = if matches!(node.widget, UiWidget::Toggle { .. }) {
                TOGGLE_INDENT.min(inner.width)
            } else {
                indent.min(inner.width)
            };
            let width = inner.width - indent;
            if width > 0.0 && inner.height > 0.0 {
                let wraps = !matches!(node.widget, UiWidget::Dropdown { .. });
                let layout = self.layout_text(node, &content, kind, wraps.then_some(width))?;
                let origin_x = if matches!(node.widget, UiWidget::Toggle { .. }) {
                    inner.x + indent
                } else {
                    inner.x
                };
                batch.add(&layout, [origin_x, inner.y], alpha);
                inline = self.inline_images(&layout, [origin_x, inner.y], alpha)?;
            }
        }
        if focused {
            outline(&mut chrome, bounds.rect, style.accent);
        }
        for mesh in [&mut chrome, &mut top] {
            for v in &mut mesh.vertices {
                v.color[3] *= alpha;
            }
        }
        self.push(format!("{key}:0"), chrome, None, layer, clip);
        if let Some((mut mesh, page)) = image {
            for v in &mut mesh.vertices {
                v.color[3] *= alpha;
            }
            self.push(format!("{key}:1"), mesh, Some(page), layer, clip);
        }
        self.push_text(key, batch, layer, text_clip);
        for (index, (page, mesh)) in inline.into_iter().enumerate() {
            self.push(
                format!("{key}:2~{index:03}"),
                mesh,
                Some(page),
                layer,
                text_clip,
            );
        }
        self.push(format!("{key}:3"), top, None, layer, clip);
        self.xf = PaintTransform::IDENTITY;
        Ok(())
    }

    fn popup(
        &mut self,
        root_key: &str,
        layer_key: u32,
        id: &str,
        highlight: u32,
        first: u32,
    ) -> Result<()> {
        let Some(bounds) = self
            .laid
            .boxes
            .iter()
            .find(|b| b.item.is_none() && self.root.tree.nodes[b.node].id == id)
        else {
            return Ok(());
        };
        let node = &self.root.tree.nodes[bounds.node];
        let UiWidget::Dropdown {
            options,
            max_visible,
            ..
        } = &node.widget
        else {
            return Ok(());
        };
        if options.is_empty() {
            return Ok(());
        }
        let popup = Popup::new(
            bounds.rect,
            options.len(),
            *max_visible,
            first,
            self.viewport,
        );
        let key = format!("game-ui:~p{layer_key:08x}{root_key}!");
        let mut chrome = Mesh::default();
        quad(&mut chrome, popup.rect, node.style.background);
        let mut batch = TextBatch::default();
        for index in popup.first..popup.first + popup.visible {
            let Some(row) = popup.option_rect(index) else {
                continue;
            };
            if index == highlight {
                quad(&mut chrome, row, node.style.accent);
            }
            let option = &options[index as usize];
            if !option.is_empty() && row.width > 8.0 {
                let layout = self.layout_text(node, option, UiTextKind::Plain, None)?;
                batch.add(&layout, [row.x + 4.0, row.y + 4.0], 1.0);
            }
        }
        outline(&mut chrome, popup.rect, node.style.accent);
        let clip = intersection(popup.rect, viewport_rect(self.viewport));
        self.push(format!("{key}0"), chrome, None, UI_POPUP_LAYER, clip);
        self.push_text(&format!("{key}1"), batch, UI_POPUP_LAYER, clip);
        Ok(())
    }

    fn tooltip(
        &mut self,
        root_key: &str,
        layer_key: u32,
        id: &str,
        tip: &str,
        at: [f32; 2],
    ) -> Result<()> {
        let Some(node) = self.root.tree.nodes.iter().find(|n| n.id == id) else {
            return Ok(());
        };
        let layout = self.layout_text(node, tip, UiTextKind::Plain, Some(240.0))?;
        let size = [layout.metrics.width + 8.0, layout.metrics.height + 8.0];
        let screen = viewport_rect(self.viewport);
        let rect = Rect {
            x: (at[0] + 12.0).min(screen.width - size[0]).max(0.0),
            y: (at[1] + 16.0).min(screen.height - size[1]).max(0.0),
            width: size[0],
            height: size[1],
        };
        let key = format!("game-ui:~t{layer_key:08x}{root_key}!");
        let mut chrome = Mesh::default();
        quad(&mut chrome, rect, [0.05, 0.05, 0.05, 0.95]);
        let mut batch = TextBatch::default();
        batch.add(&layout, [rect.x + 4.0, rect.y + 4.0], 1.0);
        let clip = intersection(rect, screen);
        self.push(format!("{key}0"), chrome, None, UI_POPUP_LAYER, clip);
        self.push_text(&format!("{key}1"), batch, UI_POPUP_LAYER, clip);
        Ok(())
    }
}

/// Destination rectangle and UV rectangle `[u0, v0, u1, v1]` for an image.
fn fit_image(area: Rect, texture: &Texture, uv: [f32; 4], fit: UiImageFit) -> (Rect, [f32; 4]) {
    let full = [uv[0], uv[1], uv[0] + uv[2], uv[1] + uv[3]];
    let source = [uv[2] * texture.width as f32, uv[3] * texture.height as f32];
    if fit == UiImageFit::Stretch
        || source[0] <= 0.0
        || source[1] <= 0.0
        || area.width <= 0.0
        || area.height <= 0.0
    {
        return (area, full);
    }
    let sx = area.width / source[0];
    let sy = area.height / source[1];
    match fit {
        UiImageFit::Contain => {
            let scale = sx.min(sy);
            let size = [source[0] * scale, source[1] * scale];
            (
                Rect {
                    x: area.x + (area.width - size[0]) / 2.0,
                    y: area.y + (area.height - size[1]) / 2.0,
                    width: size[0],
                    height: size[1],
                },
                full,
            )
        }
        _ => {
            let scale = sx.max(sy);
            let fraction = [
                area.width / scale / source[0],
                area.height / scale / source[1],
            ];
            let u0 = uv[0] + uv[2] * (1.0 - fraction[0]) / 2.0;
            let v0 = uv[1] + uv[3] * (1.0 - fraction[1]) / 2.0;
            (
                area,
                [u0, v0, u0 + uv[2] * fraction[0], v0 + uv[3] * fraction[1]],
            )
        }
    }
}

fn nine_slice(
    mesh: &mut Mesh,
    r: Rect,
    texture: &Texture,
    slices: [f32; 4],
    border: [f32; 4],
    center: bool,
    tint: [f32; 4],
) {
    let (tw, th) = (texture.width.max(1) as f32, texture.height.max(1) as f32);
    // Shrink borders proportionally when they exceed the destination.
    let fit = |a: f32, b: f32, total: f32| {
        let sum = a + b;
        if sum > total && sum > 0.0 {
            (a * total / sum, b * total / sum)
        } else {
            (a, b)
        }
    };
    let (l, rr) = fit(border[0], border[2], r.width);
    let (t, b) = fit(border[1], border[3], r.height);
    let xs = [r.x, r.x + l, r.x + r.width - rr, r.x + r.width];
    let ys = [r.y, r.y + t, r.y + r.height - b, r.y + r.height];
    let us = [
        0.0,
        (slices[0] / tw).min(1.0),
        (1.0 - slices[2] / tw).max(0.0),
        1.0,
    ];
    let vs = [
        0.0,
        (slices[1] / th).min(1.0),
        (1.0 - slices[3] / th).max(0.0),
        1.0,
    ];
    for row in 0..3 {
        for col in 0..3 {
            if row == 1 && col == 1 && !center {
                continue;
            }
            textured(
                mesh,
                Rect {
                    x: xs[col],
                    y: ys[row],
                    width: xs[col + 1] - xs[col],
                    height: ys[row + 1] - ys[row],
                },
                [us[col], vs[row], us[col + 1], vs[row + 1]],
                tint,
            );
        }
    }
}

/// Textured quad; vertex order matches `content::text_mesh` glyphs
/// (bottom-left, bottom-right, top-right, top-left).
fn textured(mesh: &mut Mesh, r: Rect, uv: [f32; 4], color: [f32; 4]) {
    if r.width <= 0.0 || r.height <= 0.0 || color[3] <= 0.0 {
        return;
    }
    let base = mesh.vertices.len() as u32;
    for ([x, y], uv) in [
        ([r.x, r.y + r.height], [uv[0], uv[3]]),
        ([r.x + r.width, r.y + r.height], [uv[2], uv[3]]),
        ([r.x + r.width, r.y], [uv[2], uv[1]]),
        ([r.x, r.y], [uv[0], uv[1]]),
    ] {
        mesh.vertices.push(Vertex {
            position: [x, y, 0.0],
            uv,
            color,
        });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| i + base));
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
