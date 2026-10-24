use super::*;
use crate::rendering3d::*;

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
                        draw.texture = Some(
                            self.content
                                .textures
                                .get(texture)
                                .ok_or_else(|| Error::new("render.texture.unknown", texture))?
                                .clone(),
                        );
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
        let Some((entity, camera)) = active_camera3d(world) else {
            return Ok(frame);
        };
        camera.validate()?;
        let mut camera = camera.clone();
        camera.upscaler = effective_upscaler(world, &camera);
        frame.camera3d = Some(CameraInstance3D {
            camera,
            world: crate::world_transform(world, entity)?.matrix,
        });
        let mut mesh_ids = BTreeSet::new();
        for entity in world.iter_entities() {
            let Some(renderer) = entity.get::<MeshRenderer>() else {
                continue;
            };
            if entity.get::<Transform>().is_none() {
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
                .map(|id| {
                    self.content
                        .textures
                        .get(id)
                        .cloned()
                        .ok_or_else(|| Error::new("render.texture.unknown", id))
                })
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
        Ok(frame)
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
                if asset.kind != "shader" {
                    return Err(Error::new("render.shader.kind", id));
                }
                let bytes = crate::schema::read_bounded(
                    &self.project.resolve_asset(id)?,
                    16 * 1024 * 1024,
                )?;
                String::from_utf8(bytes)
                    .map(Arc::<str>::from)
                    .map_err(|e| Error::new("render.shader.invalid", e.to_string()))
            })
            .transpose()
    }
}

/// The first active camera in stable authored-ID order.
fn active_camera3d(world: &World) -> Option<(Entity, &Camera3D)> {
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
