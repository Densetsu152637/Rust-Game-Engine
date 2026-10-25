use super::*;

pub(super) fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}
fn finite<const N: usize>(values: [f32; N]) -> bool {
    values.iter().all(|v| v.is_finite())
}
fn rectangle(r: [u32; 4]) -> bool {
    r[2] > 0
        && r[3] > 0
        && r[0].checked_add(r[2]).is_some_and(|n| n <= 8192)
        && r[1].checked_add(r[3]).is_some_and(|n| n <= 8192)
}
impl Scene {
    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 {
            return Err(Error::new(
                "project.schema.unsupported",
                "Only scene schema 1 is supported",
            ));
        }
        if !identifier(&self.id) || self.entities.len() > 65536 {
            return Err(Error::new(
                "scene.invalid",
                "Invalid scene ID or entity capacity exceeded",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut cameras = 0;
        for entity in &self.entities {
            let bad = |message: &str| {
                Error::new("scene.component.invalid", message).at(&self.source, &entity.id)
            };
            if !identifier(&entity.id) || !identifier(&entity.name) || !ids.insert(&entity.id) {
                return Err(Error::new(
                    "scene.entity.id.duplicate",
                    "Invalid or duplicate persistent entity ID/name",
                )
                .at(&self.source, &entity.id));
            }
            let c = &entity.components;
            if let Some(camera) = &c.camera3d {
                camera.validate()?;
                if c.transform.is_none() {
                    return Err(bad("Camera3D requires Transform"));
                }
            }
            if let Some(material) = &c.material {
                material.validate()?;
            }
            if let Some(light) = &c.directional_light {
                light.validate()?;
            }
            let local_lights = [
                c.point_light.as_ref().map(|l| l.validate()),
                c.spot_light.as_ref().map(|l| l.validate()),
                c.area_light.as_ref().map(|l| l.validate()),
            ];
            for result in local_lights.into_iter().flatten() {
                result?;
                if c.transform.is_none() {
                    return Err(bad("PointLight, SpotLight and AreaLight require Transform"));
                }
            }
            if let Some(post) = &c.post_process {
                post.validate()?;
            }
            if let Some(volume) = &c.post_process_volume {
                volume.validate()?;
                if volume.is_local() && c.transform.is_none() {
                    return Err(bad("A non-global PostProcessVolume requires Transform"));
                }
            }
            if let Some(t) = &c.transform
                && (t.version != 1 || crate::transforms::local_matrix(t).is_err())
            {
                return Err(bad("Invalid Transform"));
            }
            if let Some(bindings) = &c.event_bindings {
                crate::events::validate_bindings(bindings)?;
            }
            if let Some(s) = &c.script
                && (s.version != 1 || !identifier(&s.asset))
            {
                return Err(bad("Invalid Script"));
            }
            if let Some(m) = &c.mesh_renderer
                && (m.version != 1
                    || !identifier(&m.mesh)
                    || m.texture.as_ref().is_some_and(|s| !identifier(s)))
            {
                return Err(bad("Invalid MeshRenderer"));
            }
            if (c.sprite_renderer.is_some() || c.collider_2d.is_some()) && c.transform.is_none() {
                return Err(bad("SpriteRenderer and Collider2D require Transform"));
            }
            if let Some(s) = &c.sprite_renderer
                && (s.version != 1
                    || !identifier(&s.texture)
                    || !finite(s.size)
                    || s.size.iter().any(|n| *n <= 0.0)
                    || !finite(s.pivot)
                    || s.pivot.iter().any(|n| !(0.0..=1.0).contains(n))
                    || !finite(s.tint)
                    || s.tint.iter().any(|n| !(0.0..=1.0).contains(n))
                    || s.frames.len() > 4096
                    || s.source.is_some_and(|r| !rectangle(r))
                    || s.frames.iter().any(|r| !rectangle(*r))
                    || !s.frames_per_second.is_finite()
                    || !(0.0..=1000.0).contains(&s.frames_per_second))
            {
                return Err(bad("Invalid SpriteRenderer"));
            }
            if let Some(camera) = &c.camera_2d {
                cameras += 1;
                if camera.version != 1
                    || camera.logical_size.iter().any(|n| *n == 0 || *n > 8192)
                    || !camera.pixels_per_unit.is_finite()
                    || camera.pixels_per_unit <= 0.0
                    || !finite(camera.center)
                {
                    return Err(bad("Invalid Camera2D"));
                }
            }
            if let Some(b) = &c.collider_2d
                && (b.version != 1
                    || !finite(b.size)
                    || b.size.iter().any(|n| *n <= 0.0 || *n > 1e6)
                    || !finite(b.offset)
                    || b.offset.iter().any(|n| n.abs() > 1e6)
                    || b.layer == 0
                    || b.mask == 0)
            {
                return Err(bad("Invalid Collider2D"));
            }
        }
        if cameras > 1 {
            return Err(Error::new(
                "scene.camera.multiple",
                "At most one Camera2D is supported",
            ));
        }
        for e in &self.entities {
            let mut current = e;
            let mut visited = BTreeSet::new();
            while let Some(parent) = &current.parent {
                if current.components.transform.is_none()
                    || !visited.insert(&current.id)
                    || visited.len() > 256
                {
                    return Err(Error::new(
                        "scene.parent.invalid",
                        "Parent cycle, missing transform or hierarchy too deep",
                    ));
                }
                current = self
                    .entities
                    .iter()
                    .find(|e| &e.id == parent && e.components.transform.is_some())
                    .ok_or_else(|| {
                        Error::new("scene.parent.invalid", "Parent requires transformed entity")
                    })?;
            }
            if let Some(follow) = e
                .components
                .camera_2d
                .as_ref()
                .and_then(|c| c.follow.as_ref())
                && !self
                    .entities
                    .iter()
                    .any(|t| &t.id == follow && t.components.transform.is_some())
            {
                return Err(Error::new(
                    "scene.camera.follow",
                    "Camera follow requires a transformed entity",
                ));
            }
        }
        Ok(())
    }
    pub fn revision(&self) -> String {
        blake3::hash(&serde_json::to_vec(self).expect("validated scene serialization"))
            .to_hex()
            .to_string()
    }
}
