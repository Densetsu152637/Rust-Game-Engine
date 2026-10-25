//! Headless UI previews: project assets, the effective (themed, localized)
//! root and a rasterized image (`docs/ui-authoring.md`).
use super::{
    raster::{RasterImage, RasterOptions, UiSampling, rasterize},
    *,
};
use crate::{
    Project,
    content::{self, FontAtlas, Texture},
    localization::Localization,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// The fonts and textures a preview draws with.
#[derive(Default)]
pub struct UiPreviewAssets {
    pub textures: BTreeMap<String, Arc<Texture>>,
    /// Bitmap fonts by asset ID.
    pub fonts: BTreeMap<String, FontAtlas>,
    /// Shaping backend over the project's `font_face` assets.
    pub engine: Option<Arc<EngineTextBackend>>,
    /// Built from the project's `locale_config`, when it has one.
    pub localization: Option<Localization>,
}

/// Texture keys `root` refers to: image and nine-slice textures and the
/// `[img=key ...]` inline images of rich labels.
pub fn referenced_textures(root: &UiRoot) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for node in &root.tree.nodes {
        match &node.widget {
            UiWidget::Image { texture, .. } | UiWidget::NineSlice { texture, .. } => {
                keys.insert(texture.clone());
            }
            UiWidget::RichLabel { markup } => {
                for part in markup.split("[img=").skip(1) {
                    let key: String = part
                        .chars()
                        .take_while(|c| !c.is_whitespace() && *c != ']')
                        .collect();
                    if !key.is_empty() {
                        keys.insert(key);
                    }
                }
            }
            _ => {}
        }
    }
    keys
}

impl UiPreviewAssets {
    /// Load what previews of `roots` need from `project`: every bitmap font
    /// with its atlas texture, every `font_face` (and the `font_config`), the
    /// textures the roots refer to, and the localization. Other assets stay
    /// on disk.
    pub fn from_project(project: &Project, wanted: &BTreeSet<String>) -> Result<Self> {
        let mut assets = Self::default();
        let mut faces = BTreeMap::new();
        let mut config = None;
        let mut textures: BTreeSet<String> = wanted.clone();
        for asset in project.assets.values() {
            match asset.kind.as_str() {
                "font" => {
                    let font = content::load_font(&project.resolve_asset(&asset.id)?)?;
                    textures.insert(font.texture.clone());
                    assets.fonts.insert(asset.id.clone(), font);
                }
                "font_face" => {
                    faces.insert(
                        asset.id.clone(),
                        Arc::new(content::load_font_face(&project.resolve_asset(&asset.id)?)?),
                    );
                }
                "font_config" => {
                    config = Some(content::load_font_config(
                        &project.resolve_asset(&asset.id)?,
                    )?);
                }
                _ => {}
            }
        }
        for id in &textures {
            if project.assets.get(id).is_some_and(|a| a.kind == "texture") {
                let path = project.resolve_asset(id)?;
                assets
                    .textures
                    .insert(id.clone(), Arc::new(content::load_texture(&path)?));
            }
        }
        if !faces.is_empty() {
            let config = config.unwrap_or_default();
            config.validate_against(&faces.keys().cloned().collect())?;
            assets.engine = Some(Arc::new(EngineTextBackend::from_content(&faces, &config)?));
        }
        assets.localization = Localization::from_project(project)?;
        Ok(assets)
    }

    /// The backend previews lay text out with: shaped families first, bitmap
    /// fonts for every other key.
    pub fn text(&self) -> SelectingTextBackend<'_> {
        SelectingTextBackend::new(self.engine.as_deref(), &self.fonts, &self.textures)
    }

    /// Whether any font is available.
    pub fn has_fonts(&self) -> bool {
        !self.fonts.is_empty() || self.engine.is_some()
    }

    /// The font key to use when a node names none: the only font of the
    /// project, `None` when there are none or several.
    pub fn sole_font(&self) -> Option<String> {
        let mut keys: Vec<String> = self.fonts.keys().cloned().collect();
        if let Some(engine) = &self.engine {
            keys.extend(engine.families());
        }
        (keys.len() == 1).then(|| keys.remove(0))
    }
}

/// How a root is styled for inspection and preview.
#[derive(Clone, Default)]
pub struct UiViewOptions<'a> {
    /// Cascaded over the root (`:hover`, `:focus`, ... follow `states`).
    pub theme: Option<&'a UiComposedTheme>,
    /// Interaction flags by node ID.
    pub states: BTreeMap<String, UiStateFlags>,
    /// Resolves `@message.key` texts.
    pub localization: Option<&'a Localization>,
    /// Font key for nodes whose `style.font` is empty.
    pub default_font: Option<&'a str>,
}

/// The root as the runtime would draw it: theme cascade, message references
/// and the default font applied. `id` keys the root (used for stable draw IDs).
pub fn effective_root(root: &UiRoot, id: &str, view: &UiViewOptions<'_>) -> Result<UiRoot> {
    let mut out = match view.theme {
        Some(theme) => UiStyleRuntime::default().resolve(id, root, theme, &view.states)?,
        None => root.clone(),
    };
    if let Some(localization) = view.localization
        && let Some(localized) = localize_root(&out, localization)
    {
        out = localized;
    }
    if let Some(font) = view.default_font {
        for node in &mut out.tree.nodes {
            if node.style.font.is_empty() {
                node.style.font = font.to_owned();
            }
        }
    }
    Ok(out)
}

#[derive(Clone)]
pub struct UiPreviewOptions<'a> {
    pub view: UiViewOptions<'a>,
    /// Layout viewport in logical pixels.
    pub viewport: [u32; 2],
    /// Image pixels per logical pixel.
    pub scale: f32,
    /// Straight RGBA behind the UI.
    pub clear: [f32; 4],
    pub sampling: UiSampling,
}

impl UiPreviewOptions<'_> {
    pub fn new(viewport: [u32; 2]) -> Self {
        Self {
            view: UiViewOptions::default(),
            viewport,
            scale: 1.0,
            clear: [0.07, 0.07, 0.09, 1.0],
            sampling: UiSampling::Nearest,
        }
    }
}

/// A rendered preview.
#[derive(Clone, Debug)]
pub struct UiPreview {
    pub image: RasterImage,
    /// Draws and triangles the root produced.
    pub draws: usize,
    pub triangles: usize,
}

/// Lay `root` out, extract its draws and rasterize them.
pub fn render_preview(
    root: &UiRoot,
    id: &str,
    assets: &UiPreviewAssets,
    options: &UiPreviewOptions<'_>,
) -> Result<UiPreview> {
    let scale = options.scale;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(Error::new(
            "ui.preview.size",
            "The scale is positive and finite",
        ));
    }
    let size = options.viewport.map(|side| {
        (f64::from(side) * f64::from(scale))
            .ceil()
            .min(f64::from(u32::MAX)) as u32
    });
    // Reject oversized images before laying anything out.
    if size
        .iter()
        .any(|s| *s == 0 || *s > super::raster::MAX_RASTER_DIMENSION)
    {
        return Err(Error::new(
            "ui.preview.size",
            format!(
                "The image would be {}x{} pixels; sides are 1 to {}",
                size[0],
                size[1],
                super::raster::MAX_RASTER_DIMENSION
            ),
        ));
    }
    let effective = effective_root(root, id, &options.view)?;
    let focus = options
        .view
        .states
        .iter()
        .find(|(_, flags)| flags.focused())
        .map(|(node, _)| node.clone());
    let pressed = options
        .view
        .states
        .iter()
        .find(|(_, flags)| flags.pressed())
        .map(|(node, _)| node.clone());
    let state = UiRenderState {
        focus,
        pressed,
        themed: options.view.theme.is_some(),
        ..Default::default()
    };
    let text = assets.text();
    text.begin_frame();
    let draws =
        effective.extract_draws_with(id, options.viewport, &state, &text, &assets.textures)?;
    let triangles = draws.iter().map(|d| d.mesh.indices.len() / 3).sum();
    let image = rasterize(
        &draws,
        &RasterOptions {
            size,
            scale,
            clear: options.clear,
            sampling: options.sampling,
            ..RasterOptions::new(size)
        },
    )?;
    Ok(UiPreview {
        image,
        draws: draws.len(),
        triangles,
    })
}

impl UiDocument {
    /// The attached theme composed for cascading (themes that `extends`
    /// another need a `UiThemeSet`).
    pub fn composed_theme(&self) -> Result<Option<UiComposedTheme>> {
        self.theme().map(UiComposedTheme::new).transpose()
    }

    /// Render the document; the document theme applies unless `options`
    /// names one.
    pub fn preview(
        &self,
        assets: &UiPreviewAssets,
        options: &UiPreviewOptions<'_>,
    ) -> Result<UiPreview> {
        let composed = self.composed_theme()?;
        let mut options = options.clone();
        if options.view.theme.is_none() {
            options.view.theme = composed.as_ref();
        }
        render_preview(self.root(), "preview", assets, &options)
    }
}
