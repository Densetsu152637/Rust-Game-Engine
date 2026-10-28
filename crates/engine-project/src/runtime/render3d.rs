use super::*;
use crate::content::Texture;
use crate::rendering3d::*;
use std::sync::Arc;

impl Runtime {
    /// Extract immutable scene snapshots. A 3D camera redirects mesh renderers to
    /// the depth-tested world pass; sprites and UI retain their 2D behavior.
    pub fn extract_scene_frame(&self) -> Result<SceneRenderFrame> {
        let mut frame = SceneRenderFrame::from(self.extract_frame()?);
        let world = self.app.world();
        for entity in world.iter_entities() {
            let Some(material) = entity.get::<MeshMaterial>() else {
                continue;
            };
            material.validate()?;
            let id = entity
                .get::<AuthoredId>()
                .map(|id| id.id.clone())
                .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
            let shader = self.material_shader(material)?;
            for prefix in ["sprite", "mesh"] {
                let draw_id = format!("{prefix}:{id}");
                if let Some(draw) = frame
                    .sprites
                    .draws
                    .iter_mut()
                    .find(|draw| draw.id == draw_id)
                {
                    if let Some(texture) = &material.texture {
                        draw.texture = Some(self.resolve_texture(texture, &mut frame.streams)?);
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
        let lighting2d = self.extract_lighting2d(&mut frame)?;
        frame.lighting2d = lighting2d;
        let Some((entity, camera)) = active_camera3d(world) else {
            return Ok(frame);
        };
        camera.validate()?;
        let mut camera = camera.clone();
        camera.upscaler = effective_upscaler(world, &camera);
        camera.frame_generation = effective_frame_generation(world, &camera);
        frame.camera3d = Some(CameraInstance3D {
            camera,
            world: crate::world_transform(world, entity)?.matrix,
        });
        let mut mesh_ids = BTreeSet::new();
        let fragments_drawn =
            world.contains_resource::<crate::simulation::physics::FragmentRenderer>();
        for entity in world.iter_entities() {
            let Some(renderer) = entity.get::<MeshRenderer>() else {
                continue;
            };
            // With a fragment renderer installed a fractured destructible draws only through
            // its fragments; hide the intact mesh of this entity (children carry their own
            // `MeshRenderer` and still draw). Without one nothing draws the fragments, so the
            // intact mesh stays visible rather than the object vanishing.
            if fragments_drawn && entity.contains::<crate::simulation::physics::Fractured>() {
                continue;
            }
            if entity.get::<Transform>().is_none() {
                continue;
            }
            if let Some(chunk) = entity.get::<crate::world::tilemap::TileChunkRender>()
                && world
                    .get::<crate::lighting2d::SpriteLighting2D>(chunk.owner)
                    .is_some_and(|lighting| lighting.enabled)
                && frame.lighting2d.is_some()
            {
                continue;
            }
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
            });
            mesh_ids.insert(format!("mesh:{id}"));
        }
        frame
            .sprites
            .draws
            .retain(|draw| !mesh_ids.contains(&draw.id));
        frame.materials2d.retain(|id, _| !mesh_ids.contains(id));
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
    fn extract_lighting2d(&self, frame: &mut SceneRenderFrame) -> Result<Option<LightingFrame2D>> {
        use crate::lighting2d::{Light2D, Light2DKind, Lighting2D, SpriteLighting2D};
        let world = self.app.world();
        let Some(settings) = world
            .iter_entities()
            .find_map(|entity| {
                entity
                    .get::<Camera2D>()
                    .map(|_| entity.get::<Lighting2D>().cloned().unwrap_or_default())
            })
            .filter(|settings| settings.enabled)
        else {
            return Ok(None);
        };
        settings.validate()?;
        let mut lights = Vec::new();
        for entity in world.iter_entities() {
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
        for entity in world.iter_entities() {
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
            let Some(draw) = frame
                .sprites
                .draws
                .iter_mut()
                .find(|draw| draw.id == draw_id)
            else {
                continue;
            };
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
            let document = crate::graphs::load_graph(&self.project, id)?;
            if document.kind != engine_material_graph::KIND {
                return Err(Error::new(
                    "render.shader.kind",
                    format!("{id}: graph kind must be material"),
                ));
            }
            return engine_material_graph::compile_material_sprite_lit(&document)
                .map(|compiled| Some(Arc::<str>::from(compiled.source)))
                .map_err(|diagnostics| crate::graphs::into_error(diagnostics, &asset.path));
        }
        self.material_shader(material)
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
        for entity in world.iter_entities() {
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
                    "shader" => {
                        let bytes = crate::schema::read_bounded(
                            &self.project.resolve_asset(id)?,
                            16 * 1024 * 1024,
                        )?;
                        String::from_utf8(bytes)
                            .map(Arc::<str>::from)
                            .map_err(|e| Error::new("render.shader.invalid", e.to_string()))
                    }
                    "graph" => {
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
                    }
                    _ => Err(Error::new("render.shader.kind", id)),
                }
            })
            .transpose()
    }
}

/// The first active camera in stable authored-ID order.
pub(super) fn active_camera3d(world: &World) -> Option<(Entity, &Camera3D)> {
    world
        .iter_entities()
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
    for entity in world.iter_entities() {
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
