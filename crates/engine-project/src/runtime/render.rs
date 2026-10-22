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
#[derive(Clone, Debug)]
pub struct Draw {
    pub id: String,
    pub mesh: Arc<Mesh>,
    pub texture: Option<Arc<Texture>>,
    pub layer: i32,
    pub space: DrawSpace,
    pub clip: Option<[f32; 4]>,
    pub state: DrawState,
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
}
impl Content {
    pub(super) fn load(project: &Project) -> Result<Self> {
        let mut content = Self::default();
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
                _ => {}
            }
        }
        Ok(content)
    }
}

impl Runtime {
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
    pub fn extract_frame(&self) -> Result<RenderFrame> {
        let world = self.app.world();
        let mut camera = world
            .iter_entities()
            .find_map(|e| e.get::<Camera2D>().cloned())
            .unwrap_or_default();
        if let Some(position) = camera.follow.as_ref().and_then(|id| self.position(id)) {
            camera.center = [position[0], position[1]];
        }
        let mut draws = Vec::new();
        for entity in world.iter_entities() {
            let Some(transform) = entity.get::<Transform>() else {
                continue;
            };
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
                let mut position = transform.position;
                if camera.pixel_snap {
                    for v in &mut position[..2] {
                        *v = (*v * camera.pixels_per_unit).round() / camera.pixels_per_unit;
                    }
                }
                let mesh = content::sprite_quad(
                    position,
                    renderer.size,
                    renderer.pivot,
                    source,
                    &texture,
                    renderer.tint,
                )?;
                let draw_id = format!("sprite:{id}");
                draws.push(Draw {
                    id: draw_id.clone(),
                    mesh: self.cached_mesh(&draw_id, mesh),
                    texture: Some(texture),
                    layer: renderer.layer,
                    space: DrawSpace::World,
                    clip: None,
                    state: DrawState::default(),
                });
            }
            if let Some(renderer) = entity.get::<MeshRenderer>() {
                let original = self
                    .content
                    .meshes
                    .get(&renderer.mesh)
                    .ok_or_else(|| Error::new("render.mesh.unknown", &renderer.mesh))?;
                let mut mesh = (**original).clone();
                for vertex in &mut mesh.vertices {
                    for axis in 0..3 {
                        vertex.position[axis] += transform.position[axis];
                    }
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
            });
        }
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
