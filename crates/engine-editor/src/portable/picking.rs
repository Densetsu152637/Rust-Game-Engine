//! Fixed-resolution CPU ID buffer built only when the preview changes.
//! Geometry and texture alpha determine hits; selection never uses origin proximity.
use super::*;
use engine_project::{
    content::{Mesh, Texture},
    runtime::DrawSpace,
};
use glam::{Vec2, Vec4};
const W: usize = viewport::PREVIEW_WIDTH as usize;
const H: usize = viewport::PREVIEW_HEIGHT as usize;
const MAX_TRIANGLES: usize = 200_000;
const MAX_SAMPLES: usize = 32_000_000;
#[derive(Default)]
pub(super) struct Picker {
    ids: Vec<String>,
    pixels: Vec<u32>,
    depth: Vec<f32>,
    samples: usize,
    triangles: usize,
    scissor: Option<engine_desktop::PixelRect>,
}
#[derive(Clone, Copy)]
struct Vertex {
    clip: Vec4,
    uv: Vec2,
    alpha: f32,
}
impl Vertex {
    fn lerp(self, b: Self, t: f32) -> Self {
        Self {
            clip: self.clip.lerp(b.clip, t),
            uv: self.uv.lerp(b.uv, t),
            alpha: self.alpha + (b.alpha - self.alpha) * t,
        }
    }
}
impl Picker {
    pub fn build(frame: &SceneRenderFrame, navigation: &ViewNavigation) -> Result<Self> {
        let Some(vp) = viewport::view_projection(navigation) else {
            return Err(anyhow!("Invalid picking camera"));
        };
        let mut picker = Self {
            pixels: vec![0; W * H],
            depth: vec![f32::INFINITY; W * H],
            ..Default::default()
        };
        if navigation.three_dimensional {
            let eye = navigation.camera_world().w_axis.truncate();
            let mut meshes: Vec<_> = frame.meshes.iter().collect();
            meshes.sort_by(|a, b| {
                a.material
                    .transparent
                    .cmp(&b.material.transparent)
                    .then_with(|| {
                        if a.material.transparent {
                            let distance = |draw: &MeshDraw3D| {
                                Mat4::from_cols_array_2d(&draw.model)
                                    .w_axis
                                    .truncate()
                                    .distance_squared(eye)
                            };
                            distance(b).total_cmp(&distance(a))
                        } else {
                            a.id.cmp(&b.id)
                        }
                    })
                    .then_with(|| a.id.cmp(&b.id))
            });
            for draw in meshes {
                picker.mesh(
                    &draw.id,
                    &draw.mesh,
                    vp * Mat4::from_cols_array_2d(&draw.model),
                    draw.texture.as_deref(),
                    draw.material.tint[3],
                    true,
                    !draw.material.transparent,
                )?;
            }
        }
        let mut draws: Vec<_> = frame
            .sprites
            .draws
            .iter()
            .filter(|d| d.space == DrawSpace::World)
            .collect();
        draws.sort_by(|a, b| a.layer.cmp(&b.layer).then_with(|| a.id.cmp(&b.id)));
        // Sprite preview uses the orthographic 2D camera even when meshes use 3D.
        let sprite_camera =
            engine_desktop::CameraView::new(&frame.sprites.camera, W as u32, H as u32)?;
        for draw in draws {
            let Some(scissor) = sprite_camera.scissor(draw.clip)? else {
                continue;
            };
            picker.scissor = Some(scissor);
            picker.mesh(
                draw.id
                    .strip_prefix("sprite:")
                    .or_else(|| draw.id.strip_prefix("mesh:"))
                    .unwrap_or(&draw.id),
                &draw.mesh,
                Mat4::from_cols_array(&sprite_camera.world_with_offset(draw.offset)),
                draw.texture.as_deref(),
                frame
                    .materials2d
                    .get(&draw.id)
                    .map(|m| m.material.tint[3])
                    .unwrap_or(1.0),
                draw.state.depth_test,
                draw.state.depth_write,
            )?;
        }
        Ok(picker)
    }
    #[allow(clippy::too_many_arguments)]
    fn mesh(
        &mut self,
        id: &str,
        mesh: &Mesh,
        matrix: Mat4,
        texture: Option<&Texture>,
        alpha: f32,
        depth: bool,
        depth_write: bool,
    ) -> Result<()> {
        self.triangles += mesh.indices.len() / 3;
        if self.triangles > MAX_TRIANGLES {
            return Err(anyhow!(
                "Picking geometry exceeds {MAX_TRIANGLES} triangles"
            ));
        }
        self.ids.push(id.to_owned());
        let code = self.ids.len() as u32;
        for triangle in mesh.indices.as_chunks::<3>().0 {
            let mut polygon = Vec::with_capacity(9);
            for index in triangle {
                let Some(v) = mesh.vertices.get(*index as usize) else {
                    return Err(anyhow!("Invalid picking mesh index"));
                };
                polygon.push(Vertex {
                    clip: matrix * Vec3::from_array(v.position).extend(1.0),
                    uv: Vec2::from_array(v.uv),
                    alpha: v.color[3] * alpha,
                });
            }
            if polygon.iter().any(|v| !v.clip.is_finite()) {
                continue;
            }
            // Homogeneous clipping preserves UVs at near/far and viewport edges.
            for plane in 0..6 {
                let distance = |v: Vertex| match plane {
                    0 => v.clip.x + v.clip.w,
                    1 => v.clip.w - v.clip.x,
                    2 => v.clip.y + v.clip.w,
                    3 => v.clip.w - v.clip.y,
                    4 => v.clip.z,
                    _ => v.clip.w - v.clip.z,
                };
                let input = std::mem::take(&mut polygon);
                let Some(mut previous) = input.last().copied() else {
                    break;
                };
                for current in input {
                    let (a, b) = (distance(previous), distance(current));
                    if (a >= 0.0) != (b >= 0.0) {
                        polygon.push(previous.lerp(current, a / (a - b)));
                    }
                    if b >= 0.0 {
                        polygon.push(current);
                    }
                    previous = current;
                }
            }
            if let Some((first, rest)) = polygon.split_first() {
                for edge in rest.windows(2) {
                    self.triangle(
                        [*first, edge[0], edge[1]],
                        code,
                        texture,
                        depth,
                        depth_write,
                    )?;
                }
            }
        }
        Ok(())
    }
    fn triangle(
        &mut self,
        v: [Vertex; 3],
        code: u32,
        texture: Option<&Texture>,
        depth_test: bool,
        depth_write: bool,
    ) -> Result<()> {
        if v.iter().any(|v| v.clip.w <= 1e-6) {
            return Ok(());
        }
        let p = v.map(|v| {
            Vec2::new(
                (v.clip.x / v.clip.w + 1.0) * W as f32 * 0.5,
                (1.0 - v.clip.y / v.clip.w) * H as f32 * 0.5,
            )
        });
        let edge = |a: Vec2, b: Vec2, c: Vec2| (b - a).perp_dot(c - a);
        let area = edge(p[0], p[1], p[2]);
        if area.abs() < 1e-6 {
            return Ok(());
        }
        let min = p[0].min(p[1]).min(p[2]).floor().max(Vec2::ZERO);
        let max = p[0]
            .max(p[1])
            .max(p[2])
            .ceil()
            .min(Vec2::new(W as f32 - 1.0, H as f32 - 1.0));
        let (mut x0, mut y0, mut x1, mut y1) = (
            min.x as usize,
            min.y as usize,
            max.x as usize,
            max.y as usize,
        );
        if let Some(scissor) = self.scissor {
            x0 = x0.max(scissor.x as usize);
            y0 = y0.max(scissor.y as usize);
            x1 = x1.min((scissor.x + scissor.width).saturating_sub(1) as usize);
            y1 = y1.min((scissor.y + scissor.height).saturating_sub(1) as usize);
        }
        if x1 < x0 || y1 < y0 {
            return Ok(());
        }
        self.samples += (x1 - x0 + 1) * (y1 - y0 + 1);
        if self.samples > MAX_SAMPLES {
            return Err(anyhow!("Picking raster work exceeds {MAX_SAMPLES} samples"));
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                let point = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let bary = [
                    edge(p[1], p[2], point) / area,
                    edge(p[2], p[0], point) / area,
                    edge(p[0], p[1], point) / area,
                ];
                if bary.iter().any(|v| *v < -1e-6) {
                    continue;
                }
                let z: f32 = (0..3).map(|i| bary[i] * v[i].clip.z / v[i].clip.w).sum();
                let index = y * W + x;
                if depth_test && z > self.depth[index] {
                    continue;
                }
                let weights = [
                    bary[0] / v[0].clip.w,
                    bary[1] / v[1].clip.w,
                    bary[2] / v[2].clip.w,
                ];
                let sum: f32 = weights.iter().sum();
                let uv: Vec2 = (0..3).map(|i| v[i].uv * weights[i] / sum).sum();
                let mut alpha: f32 = (0..3).map(|i| v[i].alpha * weights[i] / sum).sum();
                if let Some(t) = texture {
                    let tx = (uv.x.clamp(0.0, 1.0) * t.width as f32)
                        .min(t.width.saturating_sub(1) as f32)
                        as usize;
                    let ty = (uv.y.clamp(0.0, 1.0) * t.height as f32)
                        .min(t.height.saturating_sub(1) as f32)
                        as usize;
                    alpha *= t
                        .pixels
                        .get((ty * t.width as usize + tx) * 4 + 3)
                        .copied()
                        .unwrap_or(0) as f32
                        / 255.0;
                }
                if alpha <= 1.0 / 255.0 {
                    continue;
                }
                self.pixels[index] = code;
                if depth_write {
                    self.depth[index] = z;
                }
            }
        }
        Ok(())
    }
    pub fn at(&self, x: u32, y: u32) -> Option<&str> {
        if x >= W as u32 || y >= H as u32 {
            return None;
        }
        let code = *self.pixels.get(y as usize * W + x as usize)?;
        code.checked_sub(1)
            .and_then(|i| self.ids.get(i as usize))
            .map(String::as_str)
    }
    pub fn members(&self, area: egui::Rect, rect: egui::Rect) -> Vec<String> {
        let area = area.intersect(rect);
        if !area.is_positive() {
            return vec![];
        }
        let low = (area.min - rect.min) / rect.size() * egui::vec2(W as f32, H as f32);
        let high = (area.max - rect.min) / rect.size() * egui::vec2(W as f32, H as f32);
        let mut ids = std::collections::BTreeSet::new();
        for y in low.y.max(0.0) as u32..=(high.y as u32).min(H as u32 - 1) {
            for x in low.x.max(0.0) as u32..=(high.x as u32).min(W as u32 - 1) {
                if let Some(id) = self.at(x, y) {
                    ids.insert(id.to_owned());
                }
            }
        }
        ids.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::content::Vertex as MeshVertex;
    fn picker() -> Picker {
        Picker {
            pixels: vec![0; W * H],
            depth: vec![f32::INFINITY; W * H],
            ..Default::default()
        }
    }
    fn triangle(z: f32) -> Mesh {
        Mesh {
            normals: vec![],
            indices: vec![0, 1, 2],
            vertices: [[-0.8, -0.8, z], [0.8, -0.8, z], [0.0, 0.8, z]]
                .map(|position| MeshVertex {
                    position,
                    uv: [0.5, 0.5],
                    color: [1.0; 4],
                })
                .to_vec(),
        }
    }
    #[test]
    fn geometry_depth_and_texture_holes_determine_pick() {
        let mut p = picker();
        p.mesh("far", &triangle(0.8), Mat4::IDENTITY, None, 1.0, true, true)
            .unwrap();
        p.mesh(
            "near",
            &triangle(0.2),
            Mat4::IDENTITY,
            None,
            1.0,
            true,
            true,
        )
        .unwrap();
        p.mesh(
            "behind",
            &triangle(0.6),
            Mat4::IDENTITY,
            None,
            1.0,
            true,
            true,
        )
        .unwrap();
        let transparent = Texture {
            width: 1,
            height: 1,
            pixels: vec![255, 255, 255, 0],
        };
        p.mesh(
            "hole",
            &triangle(0.1),
            Mat4::IDENTITY,
            Some(&transparent),
            1.0,
            true,
            true,
        )
        .unwrap();
        assert_eq!(p.at(320, 180), Some("near"));
        assert_eq!(
            p.at(1, 1),
            None,
            "origins must not make empty pixels selectable"
        );
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(W as f32, H as f32));
        assert_eq!(p.members(rect, rect), vec!["near"]);
    }
    #[test]
    fn clipped_and_behind_camera_triangles_are_bounded() {
        let mut p = picker();
        let mut mesh = triangle(-1.0);
        p.mesh("behind", &mesh, Mat4::IDENTITY, None, 1.0, true, true)
            .unwrap();
        assert!(p.pixels.iter().all(|id| *id == 0));
        mesh.vertices[0].position = [-100.0, -0.8, -1.0];
        mesh.vertices[1].position = [0.8, -0.8, 0.5];
        mesh.vertices[2].position = [0.0, 0.8, 0.5];
        p.mesh("clipped", &mesh, Mat4::IDENTITY, None, 1.0, true, true)
            .unwrap();
        assert!(p.pixels.iter().any(|id| *id != 0));
        assert!(p.samples <= W * H * 8);
        let perspective = glam::camera::rh::proj::directx::perspective(1.0, 16.0 / 9.0, 0.1, 100.0);
        let mut p = picker();
        p.mesh(
            "behind eye",
            &triangle(1.0),
            perspective,
            None,
            1.0,
            true,
            true,
        )
        .unwrap();
        assert!(p.pixels.iter().all(|id| *id == 0));
    }
    #[test]
    fn excessive_geometry_and_raster_work_return_errors() {
        let mut p = picker();
        p.triangles = MAX_TRIANGLES;
        assert!(
            p.mesh(
                "too many",
                &triangle(0.5),
                Mat4::IDENTITY,
                None,
                1.0,
                true,
                true
            )
            .is_err()
        );
        let mut p = picker();
        p.samples = MAX_SAMPLES;
        assert!(
            p.mesh(
                "too much",
                &triangle(0.5),
                Mat4::IDENTITY,
                None,
                1.0,
                true,
                true
            )
            .is_err()
        );
    }
}
