use super::*;
use crate::content::Texture;
use crate::rendering3d::*;
use std::sync::Arc;

impl Runtime {
    /// Extract immutable scene snapshots. A 3D camera redirects mesh renderers to
    /// the depth-tested world pass; sprites and UI retain their 2D behavior.
    pub fn extract_scene_frame(&self) -> Result<SceneRenderFrame> {
        let mut frame = self.extract_scene_2d()?;
        let world = self.app.world();
        let Some((entity, camera)) = active_camera3d(world) else {
            return Ok(frame);
        };
        camera.validate()?;
        let mut camera = camera.clone();
        camera.upscaler = effective_upscaler(world, &camera);
        camera.frame_generation = effective_frame_generation(world, &camera);
        frame.indirect = camera
            .lighting
            .indirect
            .as_ref()
            .map(|id| {
                self.content.indirect.get(id).cloned().ok_or_else(|| {
                    crate::Error::new(
                        "render.indirect.unknown",
                        format!("Unknown indirect asset {id}"),
                    )
                })
            })
            .transpose()?;
        frame.applied_origin = crate::world::coordinates::render_origin(world);
        frame.scene_time = self.elapsed;
        frame.camera3d = Some(CameraInstance3D {
            camera,
            world: crate::world_transform(world, entity)?.matrix,
        });
        for entity in drawn_meshes(world, frame.lighting2d.is_some()) {
            let renderer = entity
                .get::<MeshRenderer>()
                .expect("drawn meshes have a MeshRenderer");
            let id = entity
                .get::<AuthoredId>()
                .map(|id| id.id.clone())
                .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
            let material = entity.get::<MeshMaterial>().cloned().unwrap_or_default();
            material.validate()?;
            let mesh = entity
                .get::<crate::GeometryOverride>()
                .map(|geometry| &geometry.0)
                .or_else(|| self.content.meshes.get(&renderer.mesh))
                .ok_or_else(|| Error::new("render.mesh.unknown", &renderer.mesh))?
                .clone();
            let texture = material
                .texture
                .as_ref()
                .or(renderer.texture.as_ref())
                .map(|id| self.resolve_texture(id, &mut frame.streams))
                .transpose()?;
            let shader = self.material_shader(&material)?;
            let lightmap = entity
                .get::<MeshLightmap>()
                .map(|binding| {
                    check_lightmap(
                        &id,
                        binding,
                        &mesh,
                        shader.is_some(),
                        frame.indirect.as_deref(),
                    )
                })
                .transpose()?
                .cloned();
            frame.meshes.push(MeshDraw3D {
                id: id.clone(),
                mesh,
                texture,
                metallic_roughness_texture: material
                    .metallic_roughness_texture
                    .as_ref()
                    .map(|id| {
                        self.content
                            .textures
                            .get(id)
                            .cloned()
                            .ok_or_else(|| Error::new("render.texture.unknown", id))
                    })
                    .transpose()?,
                normal_texture: material
                    .normal_texture
                    .as_ref()
                    .map(|id| {
                        self.content
                            .textures
                            .get(id)
                            .cloned()
                            .ok_or_else(|| Error::new("render.texture.unknown", id))
                    })
                    .transpose()?,
                model: crate::world_transform(world, entity.id())?.matrix,
                material,
                shader,
                lightmap: lightmap.clone(),
            });
            // A runtime geometry override (skinning, World-generated meshes)
            // replaces level zero, so authored static levels and the
            // occluder marker do not apply to it.
            let overridden = entity.get::<crate::GeometryOverride>().is_some();
            let authored_lods = entity.get::<MeshLods>().filter(|_| !overridden);
            let marked = entity
                .get::<HiZOccluder>()
                .is_some_and(|marker| marker.enabled)
                && !overridden;
            if authored_lods.is_some() || marked {
                let levels = authored_lods
                    .map(|lods| {
                        lods.meshes
                            .iter()
                            .map(|id| {
                                self.content
                                    .meshes
                                    .get(id)
                                    .cloned()
                                    .ok_or_else(|| Error::new("render.mesh.unknown", id))
                            })
                            .collect::<Result<Vec<_>>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                if lightmap.is_some()
                    && let Some(level) = levels.iter().position(|level| level.uv2.is_empty())
                {
                    return Err(Error::new(
                        "render.lightmap.uv2",
                        format!(
                            "{id}: MeshLods level {} has no second UV set; a lightmapped mesh needs UV2 on every level",
                            level + 1
                        ),
                    ));
                }
                frame.mesh_options.insert(
                    id.clone(),
                    MeshDrawOptions3D {
                        levels,
                        policy: authored_lods.map_or_else(
                            || engine_mesh_lod::ScreenSizeLods {
                                thresholds: vec![1.0],
                                cull_below: None,
                                hysteresis: 0.0,
                            },
                            MeshLods::policy,
                        ),
                        occluder: marked,
                    },
                );
            }
        }
        frame.lights = extract_lights(world)?;
        frame.post = self.extract_post(entity, true)?;
        frame.ray_tracing = self
            .app
            .world()
            .get::<crate::raytracing::RayTracing>(entity)
            .cloned();
        if let Some(rt) = &frame.ray_tracing {
            rt.validate()?;
        }
        Ok(frame)
    }
    /// The sprite layer of [`Self::extract_scene_frame`] and nothing else: the 2D draws with
    /// their material and lit textures, without the 3D meshes, lights and post-processing.
    /// A presenter that keeps a history of past frames' sprites calls this for every tick
    /// and extracts the full scene frame once per presented frame.
    pub fn extract_scene_sprites(&self) -> Result<RenderFrame> {
        Ok(self.extract_scene_2d()?.sprites)
    }
    /// The 2D part of a scene frame: sprites and UI draws, the 2D materials and the 2D lighting.
    /// Under an active 3D camera the meshes the 3D pass draws are not part of it (they are
    /// never cloned, transformed or hashed for the 2D layer); meshes that stay 2D are.
    fn extract_scene_2d(&self) -> Result<SceneRenderFrame> {
        let world = self.app.world();
        let mut frame = SceneRenderFrame::from(if active_camera3d(world).is_some() {
            let lit_2d = lighting2d_settings(world).is_some();
            let fragments_drawn =
                world.contains_resource::<crate::simulation::physics::FragmentRenderer>();
            self.extract_frame_without(|entity| {
                is_drawn_mesh(world, entity, lit_2d, fragments_drawn)
            })?
        } else {
            self.extract_frame()?
        });
        let draws = draw_positions(&frame);
        for entity in crate::walk::with::<MeshMaterial>(world) {
            let material = entity
                .get::<MeshMaterial>()
                .expect("filtered to MeshMaterial");
            material.validate()?;
            let id = entity_key(&entity);
            let shader = self.material_shader(material)?;
            for prefix in ["sprite", "mesh"] {
                let draw_id = format!("{prefix}:{id}");
                if let Some(&position) = draws.get(draw_id.as_str()) {
                    if let Some(texture) = &material.texture {
                        frame.sprites.draws[position].texture =
                            Some(self.resolve_texture(texture, &mut frame.streams)?);
                    }
                    let mut material = material.clone();
                    material.unlit = true;
                    frame.materials2d.insert(
                        draw_id,
                        MaterialSnapshot {
                            material,
                            shader: shader.clone(),
                        },
                    );
                }
            }
        }
        frame.lighting2d = self.extract_lighting2d(&mut frame)?;
        Ok(frame)
    }
    fn extract_lighting2d(&self, frame: &mut SceneRenderFrame) -> Result<Option<LightingFrame2D>> {
        use crate::lighting2d::{Light2D, Light2DKind, SpriteLighting2D};
        let world = self.app.world();
        let Some(settings) = lighting2d_settings(world) else {
            return Ok(None);
        };
        settings.validate()?;
        let mut lights = Vec::new();
        for entity in crate::walk::with::<Light2D>(world) {
            let Some(light) = entity.get::<Light2D>().filter(|light| light.enabled) else {
                continue;
            };
            light.validate()?;
            let id = entity
                .get::<AuthoredId>()
                .map(|authored| authored.id.clone())
                .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
            let position = if entity.get::<Transform>().is_some() {
                crate::world_transform(world, entity.id())?.position()
            } else if light.kind == Light2DKind::Directional {
                [0.0; 3]
            } else {
                return Err(Error::new(
                    "render.light2d.transform",
                    format!("{id}: point light requires Transform"),
                ));
            };
            lights.push(crate::lighting2d::LightSnapshot2D {
                id,
                light: light.clone(),
                position,
            });
        }
        if lights.len() > 64 {
            return Err(Error::new(
                "render.light2d.limit",
                "at most 64 enabled 2D lights",
            ));
        }
        lights.sort_by(|a, b| a.id.cmp(&b.id));
        let mut materials = BTreeMap::new();
        let draws = draw_positions(frame);
        // A lit entity is a sprite carrying the marker or a tile chunk whose owner carries it.
        for entity in crate::walk::with_any!(
            world,
            SpriteLighting2D,
            crate::world::tilemap::TileChunkRender
        ) {
            let owner = entity
                .get::<crate::world::tilemap::TileChunkRender>()
                .map(|chunk| chunk.owner)
                .unwrap_or(entity.id());
            if !world
                .get::<SpriteLighting2D>(owner)
                .is_some_and(|marker| marker.enabled)
            {
                continue;
            }
            let id = entity
                .get::<AuthoredId>()
                .map(|authored| authored.id.clone())
                .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
            let prefix = if entity.get::<SpriteRenderer>().is_some() {
                "sprite"
            } else if entity
                .get::<crate::world::tilemap::TileChunkRender>()
                .is_some()
            {
                "mesh"
            } else {
                continue;
            };
            let draw_id = format!("{prefix}:{id}");
            let Some(&position) = draws.get(draw_id.as_str()) else {
                continue;
            };
            let draw = &mut frame.sprites.draws[position];
            let material = world
                .get::<MeshMaterial>(owner)
                .cloned()
                .unwrap_or_default();
            material.validate()?;
            if let Some(id) = &material.texture {
                draw.texture = Some(self.resolve_texture(id, &mut frame.streams)?);
            }
            let resolve_data = |id: &Option<String>| -> Result<Option<Arc<Texture>>> {
                id.as_ref()
                    .map(|id| {
                        self.content
                            .textures
                            .get(id)
                            .cloned()
                            .ok_or_else(|| Error::new("render.texture.unknown", id))
                    })
                    .transpose()
            };
            let mr = resolve_data(&material.metallic_roughness_texture)?;
            let normal = resolve_data(&material.normal_texture)?;
            if let Some(base) = &draw.texture {
                for atlas in [&mr, &normal].into_iter().flatten() {
                    if (base.width, base.height) != (atlas.width, atlas.height) {
                        return Err(Error::new(
                            "render.light2d.atlas",
                            format!("{draw_id}: data atlas dimensions must match base atlas"),
                        ));
                    }
                }
            }
            materials.insert(
                draw_id,
                LitMaterialSnapshot2D {
                    shader: self.material_shader_lit2d(&material)?,
                    material,
                    metallic_roughness_texture: mr,
                    normal_texture: normal,
                },
            );
        }
        for id in materials.keys() {
            frame.materials2d.remove(id);
        }
        Ok(Some(LightingFrame2D {
            lighting: crate::lighting2d::LightingSnapshot2D {
                ambient: settings.ambient,
                exposure: settings.exposure,
                lights,
            },
            materials,
        }))
    }
    fn material_shader_lit2d(&self, material: &MeshMaterial) -> Result<Option<Arc<str>>> {
        let Some(id) = &material.shader else {
            return Ok(None);
        };
        if let Some(asset) = self
            .project
            .assets
            .get(id)
            .filter(|asset| asset.kind == "graph")
        {
            return self
                .cached_shader(id, true, || {
                    let document = crate::graphs::load_graph(&self.project, id)?;
                    if document.kind != engine_material_graph::KIND {
                        return Err(Error::new(
                            "render.shader.kind",
                            format!("{id}: graph kind must be material"),
                        ));
                    }
                    engine_material_graph::compile_material_sprite_lit(&document)
                        .map(|compiled| Arc::<str>::from(compiled.source))
                        .map_err(|diagnostics| crate::graphs::into_error(diagnostics, &asset.path))
                })
                .map(Some);
        }
        self.material_shader(material)
    }
    /// The source of shader asset `id` (`lit_2d` selects the lit 2D compilation of a material
    /// graph), read or compiled by `load` the first time and kept for the runtime's life: the
    /// project's assets do not change under a running runtime, and the same source is needed
    /// for every extracted frame. A failed load is not kept. [`Self::reload_shader_asset`]
    /// drops a source so the next extraction loads it again.
    fn cached_shader(
        &self,
        id: &str,
        lit_2d: bool,
        load: impl FnOnce() -> Result<Arc<str>>,
    ) -> Result<Arc<str>> {
        let key = (id.to_owned(), lit_2d);
        if let Some(source) = self.shaders.borrow().get(&key) {
            return Ok(source.clone());
        }
        let source = load()?;
        self.shaders.borrow_mut().insert(key, source.clone());
        Ok(source)
    }
    /// Forget the cached source of `shader` or `graph` asset `id`, after the host changed its
    /// file: the next extraction reads or compiles it again.
    pub fn reload_shader_asset(&self, id: &str) {
        self.shaders
            .borrow_mut()
            .retain(|(cached, _), _| cached != id);
    }
    /// The active camera's `PostProcess`, the enabled volumes and the LUT
    /// files they reference (read bounded; the renderer validates them).
    fn extract_post(&self, camera: Entity, load_luts: bool) -> Result<crate::post::PostFrame> {
        use crate::post::{
            MAX_FRAME_LUTS, MAX_LUT_BYTES, PostFrame, PostProcess, PostProcessVolume,
        };
        let world = self.app.world();
        let mut frame = PostFrame {
            camera: world.get::<PostProcess>(camera).cloned(),
            ..Default::default()
        };
        if let Some(post) = &frame.camera {
            post.validate()?;
        }
        for entity in crate::walk::with::<PostProcessVolume>(world) {
            let Some(volume) = entity.get::<PostProcessVolume>().filter(|v| v.enabled) else {
                continue;
            };
            volume.validate()?;
            let id = entity
                .get::<AuthoredId>()
                .map(|id| id.id.clone())
                .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
            let matrix = if entity.get::<Transform>().is_some() {
                crate::world_transform(world, entity.id())?.matrix
            } else if volume.is_local() {
                return Err(Error::new(
                    "render.post.transform",
                    format!("{id}: a non-global PostProcessVolume requires Transform"),
                ));
            } else {
                [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                    [0.0, 0.0, 0.0, 1.0],
                ]
            };
            frame.volumes.push(crate::post::VolumeInstance3D {
                id,
                volume: volume.clone(),
                world: matrix,
            });
        }
        frame.volumes.sort_by(|a, b| a.id.cmp(&b.id));
        let mut lut_ids: BTreeSet<&str> = BTreeSet::new();
        if let Some(post) = &frame.camera {
            lut_ids.extend(post.asset_refs());
        }
        for instance in &frame.volumes {
            lut_ids.extend(instance.volume.asset_refs());
        }
        if lut_ids.len() > MAX_FRAME_LUTS {
            return Err(Error::new(
                "render.post.lut.limit",
                format!("a frame references at most {MAX_FRAME_LUTS} grading LUTs"),
            ));
        }
        let mut luts = BTreeMap::new();
        for id in lut_ids.into_iter().filter(|_| load_luts) {
            let asset = self
                .project
                .require_asset(id, "lut")
                .map_err(|_| Error::new("render.lut.unknown", id))?;
            let bytes = crate::schema::read_bounded(
                &self.project.resolve_asset(&asset.id)?,
                MAX_LUT_BYTES,
            )?;
            let text = String::from_utf8(bytes)
                .map_err(|e| Error::new("render.lut.invalid", e.to_string()))?;
            luts.insert(id.to_owned(), Arc::<str>::from(text));
        }
        frame.luts = luts;
        Ok(frame)
    }
    /// The renderer-reported post-processing state of the active camera
    /// view. Before any 3D frame the state is `not_rendered` and the summary
    /// is what the authored data resolves to at the camera; `None` without
    /// an active 3D camera.
    pub fn post_status(&self) -> Option<crate::post::PostStatus> {
        let world = self.app.world();
        let (entity, _) = active_camera3d(world)?;
        let mut status = world
            .get_resource::<crate::post::PostControl>()
            .map(|control| control.status.clone())
            .unwrap_or_default();
        if status.state == RenderPipelineState::NotRendered {
            let frame = self.extract_post(entity, false).ok()?;
            let position = crate::world_transform(world, entity)
                .map(|t| [t.matrix[3][0], t.matrix[3][1], t.matrix[3][2]])
                .unwrap_or_default();
            let resolved =
                crate::post::resolve_post(frame.camera.as_ref(), &frame.volumes, position);
            status = crate::post::PostStatus::of(resolved.as_ref(), frame.volumes.len());
        }
        Some(status)
    }
    /// A catalog texture, or the current frame of a `stream:<name>` texture
    /// stream (recorded once in `streams`).
    fn resolve_texture(&self, id: &str, streams: &mut Vec<StreamFrame>) -> Result<Arc<Texture>> {
        if let Some(name) = id.strip_prefix(STREAM_TEXTURE_PREFIX) {
            let stream = self
                .app
                .world()
                .get_resource::<TextureStreams>()
                .and_then(|streams| streams.get(name))
                .ok_or_else(|| Error::new("render.texture_stream.unknown", name))?;
            if !streams.iter().any(|s| s.name == stream.name) {
                streams.push(stream.clone());
            }
            return Ok(stream.texture.clone());
        }
        self.content
            .textures
            .get(id)
            .cloned()
            .ok_or_else(|| Error::new("render.texture.unknown", id))
    }
    /// The renderer-reported render feature state of the active camera.
    /// Before any 3D frame (for example headless runs) the effective
    /// overrides are reported with state `not_rendered` and whatever feature
    /// list the host published; `None` without an active 3D camera.
    pub fn render_pipeline_status(&self) -> Option<RenderPipelineStatus> {
        let world = self.app.world();
        let (_, camera) = active_camera3d(world)?;
        let mut status = world
            .get_resource::<RenderPipelineControl>()
            .map(|control| control.status.clone())
            .unwrap_or_default();
        status.overrides = camera.effective_render_features();
        Some(status)
    }
    /// The renderer-reported ray-traced lighting of the active camera view.
    /// Before any 3D frame the state is `not_rendered` and every effect the
    /// camera's `RayTracing` enables reads `fallback` ("not rendered yet");
    /// `None` without an active 3D camera.
    pub fn ray_tracing_status(&self) -> Option<crate::raytracing::RayTracingReport> {
        let world = self.app.world();
        let (entity, _) = active_camera3d(world)?;
        if let Some(status) = world
            .get_resource::<crate::raytracing::RayTracingControl>()
            .and_then(|control| control.status.clone())
        {
            return Some(status);
        }
        let authored = world.get::<crate::raytracing::RayTracing>(entity);
        let effect = |name: &str, enabled: bool| crate::raytracing::RtEffectReport {
            effect: name.into(),
            mode: if enabled { "fallback" } else { "disabled" }.into(),
            denoised: false,
            fallback: None,
            reason: Some(if enabled {
                "no frame has been rendered".into()
            } else {
                "disabled in settings".into()
            }),
        };
        let rt = authored.cloned().unwrap_or_default();
        Some(crate::raytracing::RayTracingReport {
            tier: "unavailable".into(),
            reason: Some("no frame has been rendered".into()),
            effects: vec![
                effect("shadows", rt.shadows.enabled),
                effect("ambient_occlusion", rt.ambient_occlusion.enabled),
                effect("reflections", rt.reflections.enabled),
                effect("diffuse_gi", rt.diffuse_gi.enabled),
            ],
            ..Default::default()
        })
    }
    /// The renderer-reported lighting state of the active camera view.
    /// Before any 3D frame the state is `not_rendered` with the number of
    /// enabled authored lights; `None` without an active 3D camera.
    pub fn lighting_status(&self) -> Option<crate::lights::LightingStatus> {
        let world = self.app.world();
        active_camera3d(world)?;
        let mut status = world
            .get_resource::<crate::lights::LightingControl>()
            .map(|control| control.status.clone())
            .unwrap_or_default();
        if status.state == RenderPipelineState::NotRendered {
            status.authored = extract_lights(world).map_or(0, |lights| lights.len() as u64);
        }
        Some(status)
    }
    /// The renderer-reported upscaler state. Before any 3D frame was presented
    /// (for example in headless runs) the request of the active camera is
    /// reported with state `not_rendered`; `None` without either.
    pub fn upscaler_status(&self) -> Option<UpscalerStatus> {
        let world = self.app.world();
        let (_, camera) = active_camera3d(world)?;
        let status = world.get_resource::<UpscalerControl>()?.status.clone();
        if status.state != UpscalerState::NotRendered {
            return Some(status);
        }
        Some(UpscalerStatus {
            requested: effective_upscaler(world, camera),
            ..status
        })
    }
    /// The renderer-reported frame generation state, like [`Self::upscaler_status`].
    pub fn frame_generation_status(&self) -> Option<FrameGenerationStatus> {
        let world = self.app.world();
        let (_, camera) = active_camera3d(world)?;
        let status = world
            .get_resource::<FrameGenerationControl>()?
            .status
            .clone();
        if status.state != FrameGenerationState::NotRendered {
            return Some(status);
        }
        Some(FrameGenerationStatus {
            requested: effective_frame_generation(world, camera),
            ..status
        })
    }
    fn material_shader(&self, material: &MeshMaterial) -> Result<Option<Arc<str>>> {
        material
            .shader
            .as_ref()
            .map(|id| {
                let asset = self
                    .project
                    .assets
                    .get(id)
                    .ok_or_else(|| Error::new("render.shader.unknown", id))?;
                match asset.kind.as_str() {
                    "shader" => self.cached_shader(id, false, || {
                        let bytes = crate::schema::read_bounded(
                            &self.project.resolve_asset(id)?,
                            16 * 1024 * 1024,
                        )?;
                        String::from_utf8(bytes)
                            .map(Arc::<str>::from)
                            .map_err(|e| Error::new("render.shader.invalid", e.to_string()))
                    }),
                    "graph" => self.cached_shader(id, false, || {
                        let document = crate::graphs::load_graph(&self.project, id)?;
                        if document.kind != engine_material_graph::KIND {
                            return Err(Error::new(
                                "render.shader.kind",
                                format!("{id}: graph kind must be material"),
                            ));
                        }
                        engine_material_graph::compile_material(&document)
                            .map(|compiled| Arc::<str>::from(compiled.source))
                            .map_err(|diagnostics| {
                                crate::graphs::into_error(diagnostics, &asset.path)
                            })
                    }),
                    _ => Err(Error::new("render.shader.kind", id)),
                }
            })
            .transpose()
    }
}

/// The first active camera in stable authored-ID order.
pub(super) fn active_camera3d(world: &World) -> Option<(Entity, &Camera3D)> {
    crate::walk::with::<Camera3D>(world)
        .filter_map(|entity| {
            entity
                .get::<Camera3D>()
                .filter(|camera| camera.active)
                .map(|camera| {
                    let key = entity
                        .get::<AuthoredId>()
                        .map(|id| id.id.clone())
                        .unwrap_or_else(|| entity.id().to_bits().to_string());
                    (key, entity.id(), camera)
                })
        })
        .min_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, entity, camera)| (entity, camera))
}

/// The stable authored ID of an entity, or `entity:<bits>` for one without.
fn entity_key(entity: &EntityRef) -> String {
    entity
        .get::<AuthoredId>()
        .map(|id| id.id.clone())
        .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()))
}

/// Where each draw of the frame stands in `frame.sprites.draws`, by draw ID.
fn draw_positions(frame: &SceneRenderFrame) -> BTreeMap<String, usize> {
    let mut positions = BTreeMap::new();
    for (position, draw) in frame.sprites.draws.iter().enumerate() {
        positions.entry(draw.id.clone()).or_insert(position);
    }
    positions
}

/// The settings of the 2D lighting of the scene, when the 2D camera enables it.
fn lighting2d_settings(world: &World) -> Option<crate::lighting2d::Lighting2D> {
    crate::walk::with::<Camera2D>(world)
        .find_map(|entity| {
            entity.get::<Camera2D>().map(|_| {
                entity
                    .get::<crate::lighting2d::Lighting2D>()
                    .cloned()
                    .unwrap_or_default()
            })
        })
        .filter(|settings| settings.enabled)
}

/// The mesh renderers the 3D pass draws. `lit_2d` tells whether the frame has 2D lighting:
/// generated tilemap chunks whose owner is lit draw through the 2D lit pass instead.
fn drawn_meshes(world: &World, lit_2d: bool) -> impl Iterator<Item = EntityRef<'_>> {
    // With a fragment renderer installed a fractured destructible draws only through
    // its fragments; hide the intact mesh of this entity (children carry their own
    // `MeshRenderer` and still draw). Without one nothing draws the fragments, so the
    // intact mesh stays visible rather than the object vanishing.
    let fragments_drawn = world.contains_resource::<crate::simulation::physics::FragmentRenderer>();
    crate::walk::with::<MeshRenderer>(world)
        .filter(move |entity| is_drawn_mesh(world, entity, lit_2d, fragments_drawn))
}

/// Whether the 3D pass draws the mesh of `entity` (see [`drawn_meshes`]); the meshes it does
/// not draw, when it runs at all, stay in the 2D layer or do not draw.
fn is_drawn_mesh(world: &World, entity: &EntityRef, lit_2d: bool, fragments_drawn: bool) -> bool {
    if !entity.contains::<MeshRenderer>() {
        return false;
    }
    if fragments_drawn && entity.contains::<crate::simulation::physics::Fractured>() {
        return false;
    }
    if !entity.contains::<Transform>() {
        return false;
    }
    let lit_chunk = entity
        .get::<crate::world::tilemap::TileChunkRender>()
        .is_some_and(|chunk| {
            world
                .get::<crate::lighting2d::SpriteLighting2D>(chunk.owner)
                .is_some_and(|lighting| lighting.enabled)
        });
    !(lit_chunk && lit_2d)
}

/// A native override wins over the camera's authored request.
fn effective_upscaler(world: &World, camera: &Camera3D) -> UpscalerSettings {
    world
        .get_resource::<UpscalerControl>()
        .and_then(|control| control.override_settings)
        .unwrap_or(camera.upscaler)
}

/// A valid native override wins over the camera's authored request.
fn effective_frame_generation(world: &World, camera: &Camera3D) -> FrameGenerationSettings {
    world
        .get_resource::<FrameGenerationControl>()
        .and_then(|control| control.override_settings)
        .filter(|settings| settings.validate().is_ok())
        .unwrap_or(camera.frame_generation)
}

/// Enabled light components in stable entity ID order, with validated
/// values and world transforms (identity for a directional light without
/// a Transform).
fn extract_lights(world: &World) -> Result<Vec<crate::lights::LightInstance3D>> {
    use crate::lights::{
        AreaLight, DirectionalLight, LightInstance3D, PointLight, SceneLight, SpotLight,
    };
    let mut lights = Vec::new();
    for entity in crate::walk::with_any!(world, DirectionalLight, PointLight, SpotLight, AreaLight)
    {
        let mut found = Vec::new();
        if let Some(light) = entity.get::<DirectionalLight>().filter(|l| l.enabled) {
            light.validate()?;
            found.push(SceneLight::Directional(light.clone()));
        }
        if let Some(light) = entity.get::<PointLight>().filter(|l| l.enabled) {
            light.validate()?;
            found.push(SceneLight::Point(light.clone()));
        }
        if let Some(light) = entity.get::<SpotLight>().filter(|l| l.enabled) {
            light.validate()?;
            found.push(SceneLight::Spot(light.clone()));
        }
        if let Some(light) = entity.get::<AreaLight>().filter(|l| l.enabled) {
            light.validate()?;
            found.push(SceneLight::Area(light.clone()));
        }
        if found.is_empty() {
            continue;
        }
        let id = entity
            .get::<AuthoredId>()
            .map(|id| id.id.clone())
            .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
        let directional_only = found
            .iter()
            .all(|light| matches!(light, SceneLight::Directional(_)));
        let matrix = if entity.get::<Transform>().is_some() {
            crate::world_transform(world, entity.id())?.matrix
        } else if directional_only {
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]
        } else {
            return Err(Error::new(
                "render.light.transform",
                format!("{id}: PointLight, SpotLight and AreaLight require Transform"),
            ));
        };
        for light in found {
            lights.push(LightInstance3D {
                id: id.clone(),
                light,
                world: matrix,
            });
        }
    }
    lights.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(lights)
}

/// Validates one authored lightmap binding against its mesh and the camera's
/// indirect asset, so a bad binding is a named extraction error rather than a
/// renderer failure. `custom_shader` draws are rejected: only the built-in lit
/// shader samples lightmaps.
fn check_lightmap<'a>(
    id: &str,
    binding: &'a MeshLightmap,
    mesh: &crate::content::Mesh,
    custom_shader: bool,
    indirect: Option<&crate::indirect::IndirectAsset>,
) -> Result<&'a MeshLightmap> {
    binding
        .validate()
        .map_err(|e| Error::new("render.lightmap.invalid", format!("{id}: {e}")))?;
    if custom_shader {
        return Err(Error::new(
            "render.lightmap.shader",
            format!(
                "{id}: lightmaps apply only to the built-in lit shader, not MeshMaterial.shader"
            ),
        ));
    }
    let asset = indirect.ok_or_else(|| {
        Error::new(
            "render.lightmap.indirect",
            format!("{id}: MeshLightmap needs Camera3D.lighting.indirect to name an asset with lightmaps"),
        )
    })?;
    if binding.lightmap >= asset.lightmaps.len() {
        return Err(Error::new(
            "render.lightmap.indirect",
            format!(
                "{id}: lightmap {} is missing; the indirect asset holds {}",
                binding.lightmap,
                asset.lightmaps.len()
            ),
        ));
    }
    if mesh.uv2.is_empty() {
        return Err(Error::new(
            "render.lightmap.uv2",
            format!("{id}: the mesh has no second UV set (UV2) to address a lightmap"),
        ));
    }
    Ok(binding)
}

impl Runtime {
    /// The particle input of the last completed scene tick: enabled
    /// `ParticleEmitter` components in stable ID order with their world
    /// transforms. Cheap and independent of [`Self::extract_scene_frame`] so a
    /// host can feed the renderer once per tick (docs/rendering-particles.md).
    /// `tick` is zero and `dt` zero before the first tick.
    pub fn extract_particles(&self) -> Result<crate::particles::ParticleFrame> {
        use crate::particles::{
            MAX_PARTICLE_EMITTERS, MAX_TOTAL_EMITTER_CAPACITY, ParticleEmitter,
            ParticleEmitterInstance, ParticleFrame,
        };
        let world = self.app.world();
        let mut emitters = Vec::new();
        for entity in crate::walk::with::<ParticleEmitter>(world) {
            let Some(emitter) = entity.get::<ParticleEmitter>().filter(|e| e.enabled) else {
                continue;
            };
            emitter.validate()?;
            let id = entity
                .get::<AuthoredId>()
                .map(|id| id.id.clone())
                .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
            if entity.get::<Transform>().is_none() {
                return Err(Error::new(
                    "render.particles.transform",
                    format!("{id}: ParticleEmitter requires Transform"),
                ));
            }
            emitters.push(ParticleEmitterInstance {
                world: crate::world_transform(world, entity.id())?.matrix,
                emitter: emitter.clone(),
                id,
            });
        }
        emitters.sort_by(|a, b| a.id.cmp(&b.id));
        let total: u64 = emitters.iter().map(|e| u64::from(e.emitter.capacity)).sum();
        if emitters.len() > MAX_PARTICLE_EMITTERS || total > u64::from(MAX_TOTAL_EMITTER_CAPACITY) {
            return Err(Error::new(
                "render.particles.limit",
                format!(
                    "at most {MAX_PARTICLE_EMITTERS} enabled ParticleEmitters totalling \
                     {MAX_TOTAL_EMITTER_CAPACITY} particles; found {} totalling {total}",
                    emitters.len()
                ),
            ));
        }
        Ok(ParticleFrame {
            tick: self.ticks,
            dt: self.last_dt,
            emitters,
        })
    }
}
