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
        let mut cameras: Vec<_> = world
            .iter_entities()
            .filter_map(|entity| {
                entity
                    .get::<Camera3D>()
                    .filter(|camera| camera.active)
                    .map(|camera| (entity, camera))
            })
            .collect();
        cameras.sort_by_key(|(entity, _)| {
            entity
                .get::<AuthoredId>()
                .map(|id| id.id.clone())
                .unwrap_or_else(|| entity.id().to_bits().to_string())
        });
        let Some((entity, camera)) = cameras.first() else {
            return Ok(frame);
        };
        camera.validate()?;
        frame.camera3d = Some(CameraInstance3D {
            camera: (*camera).clone(),
            world: crate::world_transform(world, entity.id())?.matrix,
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
            let mesh = self
                .content
                .meshes
                .get(&renderer.mesh)
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
