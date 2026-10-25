use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawSpace {
    World,
    Ui,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Opaque,
    StraightAlpha,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawState {
    pub blend: BlendMode,
    pub depth_test: bool,
    pub depth_write: bool,
}
impl Default for DrawState {
    fn default() -> Self {
        Self {
            blend: BlendMode::StraightAlpha,
            depth_test: false,
            depth_write: false,
        }
    }
}
/// Round the X and Y of `position` to the pixel grid of `pixels_per_unit`; Z is
/// untouched. This is the one definition of pixel snapping: extraction snaps a
/// sprite's baked quad with it and a presenter snaps an interpolated position
/// with it, so both land on the same grid.
pub fn snap_to_pixels(mut position: [f32; 3], pixels_per_unit: f32) -> [f32; 3] {
    for v in &mut position[..2] {
        *v = (*v * pixels_per_unit).round() / pixels_per_unit;
    }
    position
}

/// Where a draw's entity stood at this tick, kept so a presenter can blend the
/// last two ticks. Present only on draws whose geometry is the entity's own
/// placement (sprites and meshes); UI draws never move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawMotion {
    /// Entity position this tick, before any pixel snapping.
    pub position: [f32; 3],
    /// The mesh was baked at the pixel-snapped X/Y of `position` (a sprite
    /// under a pixel-snapping camera) rather than at `position` itself.
    pub pixel_snap: bool,
}
impl DrawMotion {
    /// The position the draw's mesh is baked at.
    pub fn baked_position(&self, pixels_per_unit: f32) -> [f32; 3] {
        if self.pixel_snap {
            snap_to_pixels(self.position, pixels_per_unit)
        } else {
            self.position
        }
    }
}

#[derive(Clone, Debug)]
pub struct Draw {
    pub id: String,
    pub mesh: Arc<Mesh>,
    pub texture: Option<Arc<Texture>>,
    pub layer: i32,
    pub space: DrawSpace,
    pub clip: Option<[f32; 4]>,
    pub state: DrawState,
    /// Entity placement for presentation-time interpolation, if the draw moves
    /// with an entity. Extraction fills it; renderers ignore it.
    pub motion: Option<DrawMotion>,
    /// Translation, in world units, applied to the mesh when it is drawn.
    /// Extraction leaves it zero (the mesh is already in world space); a
    /// presenter sets it to shift a draw without rebuilding or re-uploading its
    /// mesh. World-space draws only.
    pub offset: [f32; 3],
}
#[derive(Clone, Debug)]
pub struct RenderFrame {
    pub camera: Camera2D,
    pub draws: Vec<Draw>,
}
#[derive(Default)]
pub(super) struct Content {
    pub(super) textures: BTreeMap<String, Arc<Texture>>,
    pub(super) meshes: BTreeMap<String, Arc<Mesh>>,
    pub(super) fonts: BTreeMap<String, FontAtlas>,
    /// Shaping backend over `font_face` assets; `None` when the project has none.
    pub(super) text: Option<Arc<crate::ui_tree::EngineTextBackend>>,
    /// The project's `font_config` fallback chains, before per-locale fonts.
    pub(super) base_fallback: engine_text::FallbackConfig,
    /// Localization built from the project's `locale_config`, if it has one.
    pub(super) localization: Option<crate::localization::Localization>,
}
impl Content {
    pub(super) fn ui_text(&self) -> crate::ui_tree::SelectingTextBackend<'_> {
        crate::ui_tree::SelectingTextBackend::new(self.text.as_deref(), &self.fonts, &self.textures)
    }
    pub(super) fn load(project: &Project) -> Result<Self> {
        let mut content = Self::default();
        let mut faces = BTreeMap::new();
        let mut config = None;
        for asset in project.assets.values() {
            let path = project.resolve_asset(&asset.id)?;
            match asset.kind.as_str() {
                "texture" => {
                    content
                        .textures
                        .insert(asset.id.clone(), Arc::new(content::load_texture(&path)?));
                }
                "mesh" => {
                    content
                        .meshes
                        .insert(asset.id.clone(), Arc::new(content::load_mesh(&path)?));
                }
                "font" => {
                    content
                        .fonts
                        .insert(asset.id.clone(), content::load_font(&path)?);
                }
                "font_face" => {
                    faces.insert(asset.id.clone(), Arc::new(content::load_font_face(&path)?));
                }
                "font_config" => {
                    let loaded = content::load_font_config(&path)?;
                    if config.replace(loaded).is_some() {
                        return Err(Error::new(
                            "asset.font_config.duplicate",
                            "A project may declare at most one font_config asset",
                        ));
                    }
                }
                _ => {}
            }
        }
        if !faces.is_empty() || config.is_some() {
            let config = config.unwrap_or_default();
            config.validate_against(&faces.keys().cloned().collect())?;
            content.base_fallback = config.fallback_config();
            if !faces.is_empty() {
                content.text = Some(Arc::new(crate::ui_tree::EngineTextBackend::from_content(
                    &faces, &config,
                )?));
            }
        }
        content.localization = crate::localization::Localization::from_project(project)?;
        Ok(content)
    }
}

impl Runtime {
    /// The UI text backend: shaped `font_face` families first, bitmap fonts
    /// for every other font key.
    pub fn ui_text(&self) -> crate::ui_tree::SelectingTextBackend<'_> {
        self.content.ui_text()
    }
    fn push_world_text(&self, draws: &mut Vec<Draw>, text: crate::world_text::WorldTextDraw) {
        draws.push(Draw {
            mesh: self.cached_mesh(&text.id, text.mesh),
            id: text.id,
            texture: Some(text.texture),
            layer: text.layer,
            space: DrawSpace::World,
            clip: None,
            state: DrawState::default(),
            motion: Some(DrawMotion {
                position: text.position,
                pixel_snap: false,
            }),
            offset: [0.0; 3],
        });
    }
    /// The shaping text engine, when the project has `font_face` assets (to read
    /// its status, set the language or replace fallback chains).
    pub fn text_engine(&self) -> Option<&Arc<crate::ui_tree::EngineTextBackend>> {
        self.content.text.as_ref()
    }
    fn cached_mesh(&self, id: &str, mesh: Mesh) -> Arc<Mesh> {
        let hash = blake3::hash(&serde_json::to_vec(&mesh).expect("finite geometry"));
        let mut cache = self.geometry.borrow_mut();
        if let Some((previous, cached)) = cache.get(id)
            && *previous == hash
        {
            return cached.clone();
        }
        let mesh = Arc::new(mesh);
        cache.insert(id.into(), (hash, mesh.clone()));
        mesh
    }
    /// Number of scene transitions so far. A presenter blending two ticks must
    /// not blend across a change of this value.
    pub fn scene_revision(&self) -> u64 {
        self.scene_revision
    }
    /// The camera [`Self::extract_frame`] would use, without building draws.
    pub fn extract_camera(&self) -> Camera2D {
        let mut camera = self
            .app
            .world()
            .iter_entities()
            .find_map(|e| e.get::<Camera2D>().cloned())
            .unwrap_or_default();
        if let Some(position) = camera.follow.as_ref().and_then(|id| self.position(id)) {
            camera.center = [position[0], position[1]];
        }
        camera
    }
    pub fn extract_frame(&self) -> Result<RenderFrame> {
        let world = self.app.world();
        let camera = self.extract_camera();
        let mut draws = Vec::new();
        for entity in world.iter_entities() {
            let Some(_) = entity.get::<Transform>() else {
                continue;
            };
            let transform = crate::world_transform(world, entity.id())?;
            let id = entity
                .get::<AuthoredId>()
                .map(|a| a.id.clone())
                .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
            if let Some(renderer) = entity.get::<SpriteRenderer>().filter(|s| s.visible) {
                let texture = self
                    .content
                    .textures
                    .get(&renderer.texture)
                    .ok_or_else(|| Error::new("render.texture.unknown", &renderer.texture))?
                    .clone();
                let frame = entity.get::<SpriteFrame>().map(|f| f.0).unwrap_or_else(|| {
                    content::animation_frame(
                        self.elapsed,
                        renderer.frames_per_second,
                        renderer.frames.len(),
                        renderer.looping,
                    )
                });
                let source = if renderer.frames.is_empty() {
                    renderer
                        .source
                        .unwrap_or([0, 0, texture.width, texture.height])
                } else {
                    renderer.frames[frame.min(renderer.frames.len() - 1)]
                };
                let motion = DrawMotion {
                    position: transform.position(),
                    pixel_snap: camera.pixel_snap,
                };
                let mut mesh = content::sprite_quad(
                    [0.0; 3],
                    renderer.size,
                    renderer.pivot,
                    source,
                    &texture,
                    renderer.tint,
                )?;
                let baked = motion.baked_position(camera.pixels_per_unit);
                for vertex in &mut mesh.vertices {
                    vertex.position = transform.transform_point(vertex.position);
                    for (axis, value) in baked.iter().enumerate() {
                        vertex.position[axis] += value - motion.position[axis];
                    }
                }
                let draw_id = format!("sprite:{id}");
                draws.push(Draw {
                    id: draw_id.clone(),
                    mesh: self.cached_mesh(&draw_id, mesh),
                    texture: Some(texture),
                    layer: renderer.layer,
                    space: DrawSpace::World,
                    clip: None,
                    state: DrawState::default(),
                    motion: Some(motion),
                    offset: [0.0; 3],
                });
            }
            if let Some(renderer) = entity.get::<MeshRenderer>() {
                let original = entity
                    .get::<crate::GeometryOverride>()
                    .map(|geometry| &geometry.0)
                    .or_else(|| self.content.meshes.get(&renderer.mesh))
                    .ok_or_else(|| Error::new("render.mesh.unknown", &renderer.mesh))?;
                let mut mesh = (**original).clone();
                for vertex in &mut mesh.vertices {
                    vertex.position = transform.transform_point(vertex.position);
                }
                let texture = renderer
                    .texture
                    .as_ref()
                    .map(|id| {
                        self.content
                            .textures
                            .get(id)
                            .cloned()
                            .ok_or_else(|| Error::new("render.texture.unknown", id))
                    })
                    .transpose()?;
                let draw_id = format!("mesh:{id}");
                draws.push(Draw {
                    id: draw_id.clone(),
                    mesh: self.cached_mesh(&draw_id, mesh),
                    texture,
                    layer: 0,
                    space: DrawSpace::World,
                    clip: None,
                    state: DrawState {
                        blend: BlendMode::Opaque,
                        depth_test: true,
                        depth_write: true,
                    },
                    motion: Some(DrawMotion {
                        position: transform.position(),
                        pixel_snap: false,
                    }),
                    offset: [0.0; 3],
                });
            }
        }
        for panel in world.resource::<UiModel>().panels() {
            let o = &panel.options;
            let font = self
                .content
                .fonts
                .get(&o.font_asset)
                .ok_or_else(|| Error::new("render.font.unknown", &o.font_asset))?;
            let texture = self
                .content
                .textures
                .get(&font.texture)
                .ok_or_else(|| Error::new("render.texture.unknown", &font.texture))?
                .clone();
            let mut text = panel.lines.join("\n");
            for (index, choice) in o.choices.iter().enumerate() {
                text.push('\n');
                if index == panel.selected {
                    text.push_str("> ");
                } else {
                    text.push_str("  ");
                }
                text.push_str(choice);
            }
            let mesh = content::text_mesh(
                font,
                &texture,
                &text,
                [o.rect.x, o.rect.y - panel.scroll_offset],
                0.0,
                o.scale,
                o.color,
            )?;
            let id = format!("ui:{}", o.id);
            draws.push(Draw {
                id: id.clone(),
                mesh: self.cached_mesh(&id, mesh),
                texture: Some(texture),
                layer: o.layer,
                space: DrawSpace::Ui,
                clip: Some([
                    panel.clip.x,
                    panel.clip.y,
                    panel.clip.width,
                    panel.clip.height,
                ]),
                state: DrawState::default(),
                motion: None,
                offset: [0.0; 3],
            });
        }
        for text in crate::world_text::extract(world, self.content.text.as_deref())? {
            self.push_world_text(&mut draws, text);
        }
        draws.extend(crate::ui_tree::extract_world_with(
            world,
            camera.logical_size,
            &self.ui_text(),
            &self.content.textures,
        )?);
        draws.sort_by(|a, b| {
            (a.space == DrawSpace::Ui, a.layer, &a.id).cmp(&(
                b.space == DrawSpace::Ui,
                b.layer,
                &b.id,
            ))
        });
        let active: BTreeSet<_> = draws.iter().map(|d| d.id.clone()).collect();
        self.geometry
            .borrow_mut()
            .retain(|id, _| active.contains(id));
        Ok(RenderFrame { camera, draws })
    }
}
