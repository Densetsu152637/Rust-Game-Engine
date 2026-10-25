//! CPU reference geometry shared by the features and their tests: the 2D
//! camera fit and scissor of screen UI, the billboard basis and the
//! back-to-front order of world UI.
use engine_project::ui_tree::{WorldBillboard, WorldUiItem};
use engine_render_pipeline::ViewData;
use glam::{Mat4, Vec3};

/// Where the 2D camera's logical rectangle lands in a render target, with the
/// same integer-above-1x, fractional-below fit as the sprite renderer
/// (`engine-desktop` `CameraView`), so overlay text lines up with sprites/UI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiFit {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Physical pixels per logical pixel.
    pub scale: f64,
}

impl UiFit {
    /// `None` for a zero logical or target size.
    pub fn new(ui_size: [u32; 2], target: [u32; 2]) -> Option<Self> {
        let [lw, lh] = ui_size;
        let [width, height] = target;
        if lw == 0 || lh == 0 || width == 0 || height == 0 {
            return None;
        }
        let fit = (width as f64 / lw as f64).min(height as f64 / lh as f64);
        let scale = if fit >= 1.0 { fit.floor() } else { fit };
        let extent = |logical: u32| (logical as f64 * scale + 1e-9).floor() as u32;
        let vw = extent(lw).max(1).min(width);
        let vh = extent(lh).max(1).min(height);
        Some(Self {
            x: (width - vw) / 2,
            y: (height - vh) / 2,
            width: vw,
            height: vh,
            scale,
        })
    }

    /// A logical clip rectangle `[x, y, w, h]` as a physical scissor
    /// `(x, y, w, h)` inside the fit; `None` when the intersection is empty
    /// (the draw is skipped) and the whole fit for no clip.
    pub fn scissor(&self, clip: Option<[f32; 4]>) -> Option<(u32, u32, u32, u32)> {
        let Some([x, y, w, h]) = clip else {
            return Some((self.x, self.y, self.width, self.height));
        };
        if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite())
            || w <= 0.0
            || h <= 0.0
        {
            return None;
        }
        let bound = |v: f64, max: u32| v.clamp(0.0, max as f64) as u32;
        let left = bound((x as f64 * self.scale).floor(), self.width);
        let top = bound((y as f64 * self.scale).floor(), self.height);
        let right = bound(((x as f64 + w as f64) * self.scale).ceil(), self.width);
        let bottom = bound(((y as f64 + h as f64) * self.scale).ceil(), self.height);
        (right > left && bottom > top).then_some((
            self.x + left,
            self.y + top,
            right - left,
            bottom - top,
        ))
    }
}

/// Column-major matrix mapping UI logical pixels (origin top-left, y down) to
/// clip space, as the sprite renderer's UI camera does.
pub fn ui_clip_from_pixels(ui_size: [u32; 2]) -> [[f32; 4]; 4] {
    let (w, h) = (ui_size[0] as f32, ui_size[1] as f32);
    [
        [2.0 / w, 0.0, 0.0, 0.0],
        [0.0, -2.0 / h, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0, 1.0],
    ]
}

/// The camera axes billboards align with, from a view's `world_from_view`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraBasis {
    pub position: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    /// Pointing from the scene toward the camera (the view looks down -Z).
    pub back: Vec3,
}

impl CameraBasis {
    pub fn of(view: &ViewData) -> Self {
        let m = view.world_from_view;
        Self {
            position: m.w_axis.truncate(),
            right: m.x_axis.truncate().normalize_or_zero(),
            up: m.y_axis.truncate().normalize_or_zero(),
            back: m.z_axis.truncate().normalize_or_zero(),
        }
    }

    /// Unit direction the camera looks along.
    pub fn forward(&self) -> Vec3 {
        -self.back
    }
}

/// The matrix a billboard item is drawn with: `world` turned toward the
/// camera, keeping its translation and per-axis scale. The WGSL vertex shader
/// of `ui.world` computes the same basis.
///
/// * `None`: `world` unchanged.
/// * `Full` and `Yaw`: the quad's +Z faces the camera position (`Yaw` ignores
///   the height difference, staying upright); the same construction as
///   `UiWorldPanel::placement`.
/// * `Screen`: the camera's right, up and back axes (the text stays upright
///   and parallel to the screen).
pub fn billboard_matrix(world: Mat4, camera: &CameraBasis, mode: WorldBillboard) -> Mat4 {
    if mode == WorldBillboard::None {
        return world;
    }
    let position = world.w_axis.truncate();
    let scale = Vec3::new(
        world.x_axis.truncate().length(),
        world.y_axis.truncate().length(),
        world.z_axis.truncate().length(),
    );
    let (right, up, back) = match mode {
        WorldBillboard::Screen => (camera.right, camera.up, camera.back),
        _ => {
            let mut forward = camera.position - position;
            if mode == WorldBillboard::Yaw {
                forward.y = 0.0;
            }
            if forward.length_squared() < 1e-12 {
                return world;
            }
            let forward = forward.normalize();
            let reference = if forward.y.abs() > 0.999 {
                Vec3::X
            } else {
                Vec3::Y
            };
            let right = reference.cross(forward).normalize();
            (right, forward.cross(right), forward)
        }
    };
    Mat4::from_cols(
        (right * scale.x).extend(0.0),
        (up * scale.y).extend(0.0),
        (back * scale.z).extend(0.0),
        position.extend(1.0),
    )
}

/// Centre of an item's local x/y bounds (z 0), the point its depth is sorted by.
pub fn local_center(item: &WorldUiItem) -> Vec3 {
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for draw in &item.draws {
        for vertex in &draw.mesh.vertices {
            let p = Vec3::from(vertex.position);
            min = min.min(p);
            max = max.max(p);
        }
    }
    if min.x > max.x {
        return Vec3::ZERO;
    }
    (min + max) * 0.5
}

/// Item indices in draw order for `view`: farthest first (distance of the
/// item's bounds centre along the view direction), then lower layer, then ID.
/// Sorting is per item: intersecting or very large items can still need
/// per-fragment ordering, which blended UI does not provide.
pub fn order_items(items: &[WorldUiItem], view: &ViewData) -> Vec<usize> {
    let camera = CameraBasis::of(view);
    let forward = camera.forward();
    let depth: Vec<f32> = items
        .iter()
        .map(|item| {
            let world = Mat4::from_cols_array_2d(&item.world_from_local);
            let placed = billboard_matrix(world, &camera, item.billboard);
            (placed.transform_point3(local_center(item)) - camera.position).dot(forward)
        })
        .collect();
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        depth[b]
            .total_cmp(&depth[a])
            .then_with(|| items[a].layer.cmp(&items[b].layer))
            .then_with(|| items[a].id.cmp(&items[b].id))
    });
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::{
        content::{Mesh, Vertex},
        ui_tree::{UiBillboard, UiWorldPanel, WorldUiDraw, WorldUiKind},
    };
    use engine_render_pipeline::ViewId;
    use std::sync::Arc;

    fn quad(half: f32) -> Arc<Mesh> {
        let v = |x: f32, y: f32| Vertex {
            position: [x, y, 0.0],
            uv: [0.0; 2],
            color: [1.0; 4],
        };
        Arc::new(Mesh {
            normals: vec![],
            vertices: vec![
                v(-half, -half),
                v(half, -half),
                v(half, half),
                v(-half, half),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
        })
    }

    fn item(id: &str, position: Vec3, layer: i32, billboard: WorldBillboard) -> WorldUiItem {
        WorldUiItem {
            id: id.into(),
            kind: WorldUiKind::Panel,
            layer,
            world_from_local: Mat4::from_translation(position).to_cols_array_2d(),
            billboard,
            draws: vec![WorldUiDraw {
                mesh: quad(0.5),
                texture: None,
                sdf: false,
            }],
        }
    }

    fn view(position: Vec3, target: Vec3) -> ViewData {
        let world_from_view =
            glam::camera::rh::view::look_at_mat4(position, target, Vec3::Y).inverse();
        ViewData::new(
            ViewId::MAIN,
            world_from_view,
            glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 100.0),
            [64, 64],
            [64, 64],
        )
        .unwrap()
    }

    #[test]
    fn items_sort_back_to_front_along_the_view_direction() {
        let items = [
            item("near", Vec3::new(0.0, 0.0, 2.0), 0, WorldBillboard::None),
            item("far", Vec3::new(0.0, 0.0, -6.0), 0, WorldBillboard::None),
            item("mid", Vec3::new(1.0, 0.0, -1.0), 0, WorldBillboard::None),
        ];
        let order = order_items(&items, &view(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO));
        let ids: Vec<&str> = order.iter().map(|&i| items[i].id.as_str()).collect();
        assert_eq!(ids, ["far", "mid", "near"]);
        // Seen from the other side the order reverses.
        let order = order_items(&items, &view(Vec3::new(0.0, 0.0, -9.0), Vec3::ZERO));
        let ids: Vec<&str> = order.iter().map(|&i| items[i].id.as_str()).collect();
        assert_eq!(ids, ["near", "mid", "far"]);
    }

    #[test]
    fn sort_uses_depth_along_the_view_axis_not_distance() {
        // `side` is closer in distance but deeper along the view axis.
        let items = [
            item("axis", Vec3::new(0.0, 0.0, 0.0), 0, WorldBillboard::None),
            item("side", Vec3::new(3.0, 0.0, -0.5), 0, WorldBillboard::None),
        ];
        let v = view(Vec3::new(0.0, 0.0, 5.0), Vec3::new(0.0, 0.0, 0.0));
        let order = order_items(&items, &v);
        assert_eq!(items[order[0]].id, "side");
    }

    #[test]
    fn equal_depth_orders_by_layer_then_id() {
        let items = [
            item("b", Vec3::ZERO, 1, WorldBillboard::None),
            item("c", Vec3::ZERO, 0, WorldBillboard::None),
            item("a", Vec3::ZERO, 1, WorldBillboard::None),
        ];
        let order = order_items(&items, &view(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO));
        let ids: Vec<&str> = order.iter().map(|&i| items[i].id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "b"]);
    }

    #[test]
    fn full_and_yaw_billboards_match_the_panel_placement() {
        let world = Mat4::from_scale_rotation_translation(
            Vec3::new(2.0, 3.0, 1.0),
            glam::Quat::from_rotation_y(2.0),
            Vec3::new(1.0, 2.0, -3.0),
        );
        let camera_position = Vec3::new(5.0, 6.0, 4.0);
        let v = view(camera_position, Vec3::ZERO);
        let basis = CameraBasis::of(&v);
        for (mode, ui) in [
            (WorldBillboard::Full, UiBillboard::Full),
            (WorldBillboard::Yaw, UiBillboard::Yaw),
        ] {
            let panel = UiWorldPanel {
                billboard: ui,
                ..Default::default()
            };
            let want = panel.placement(world, Some(camera_position));
            let got = billboard_matrix(world, &basis, mode);
            assert!(want.abs_diff_eq(got, 1e-5), "{mode:?}\n{want:?}\n{got:?}");
        }
    }

    #[test]
    fn facing_billboards_look_at_the_camera_and_yaw_stays_upright() {
        let world = Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0));
        let v = view(Vec3::new(4.0, 5.0, 4.0), Vec3::ZERO);
        let basis = CameraBasis::of(&v);
        let full = billboard_matrix(world, &basis, WorldBillboard::Full);
        let toward = (basis.position - Vec3::new(0.0, 1.0, 0.0)).normalize();
        assert!(full.z_axis.truncate().normalize().dot(toward) > 0.9999);
        let yaw = billboard_matrix(world, &basis, WorldBillboard::Yaw);
        assert!(
            yaw.y_axis.truncate().normalize().dot(Vec3::Y) > 0.9999,
            "yaw keeps the up axis"
        );
        let normal = yaw.z_axis.truncate().normalize();
        assert!(
            normal.y.abs() < 1e-5,
            "yaw normal is horizontal: {normal:?}"
        );
        let screen = billboard_matrix(world, &basis, WorldBillboard::Screen);
        assert!(screen.x_axis.truncate().normalize().dot(basis.right) > 0.9999);
        assert!(screen.y_axis.truncate().normalize().dot(basis.up) > 0.9999);
        assert_eq!(billboard_matrix(world, &basis, WorldBillboard::None), world);
    }

    #[test]
    fn billboards_keep_position_and_scale() {
        let world = Mat4::from_scale_rotation_translation(
            Vec3::new(2.0, 3.0, 4.0),
            glam::Quat::from_rotation_x(0.7),
            Vec3::new(1.0, 2.0, 3.0),
        );
        let basis = CameraBasis::of(&view(Vec3::new(8.0, 1.0, 9.0), Vec3::ZERO));
        for mode in [
            WorldBillboard::Full,
            WorldBillboard::Yaw,
            WorldBillboard::Screen,
        ] {
            let m = billboard_matrix(world, &basis, mode);
            assert!((m.w_axis.truncate() - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-6);
            assert!((m.x_axis.truncate().length() - 2.0).abs() < 1e-5);
            assert!((m.y_axis.truncate().length() - 3.0).abs() < 1e-5);
            assert!((m.z_axis.truncate().length() - 4.0).abs() < 1e-5);
        }
    }

    #[test]
    fn the_ui_fit_matches_integer_and_fractional_scaling() {
        let fit = UiFit::new([320, 180], [1280, 720]).unwrap();
        assert_eq!((fit.x, fit.y, fit.width, fit.height), (0, 0, 1280, 720));
        assert_eq!(fit.scale, 4.0);
        let letterboxed = UiFit::new([320, 180], [1000, 720]).unwrap();
        assert_eq!(letterboxed.scale, 3.0);
        assert_eq!(
            (
                letterboxed.x,
                letterboxed.y,
                letterboxed.width,
                letterboxed.height
            ),
            (20, 90, 960, 540)
        );
        let small = UiFit::new([320, 180], [160, 90]).unwrap();
        assert_eq!((small.scale, small.width, small.height), (0.5, 160, 90));
        assert!(UiFit::new([0, 180], [10, 10]).is_none());
    }

    #[test]
    fn scissors_are_physical_and_clamped_to_the_fit() {
        let fit = UiFit::new([320, 180], [1000, 720]).unwrap();
        assert_eq!(fit.scissor(None), Some((20, 90, 960, 540)));
        assert_eq!(
            fit.scissor(Some([10.0, 10.0, 20.0, 5.0])),
            Some((20 + 30, 90 + 30, 60, 15))
        );
        assert_eq!(fit.scissor(Some([0.0, 0.0, 0.0, 5.0])), None);
        assert_eq!(fit.scissor(Some([400.0, 0.0, 10.0, 5.0])), None);
        assert_eq!(
            fit.scissor(Some([300.0, 170.0, 100.0, 100.0])),
            Some((20 + 900, 90 + 510, 60, 30))
        );
    }

    #[test]
    fn the_ui_matrix_maps_logical_corners_to_clip_space() {
        let m = Mat4::from_cols_array_2d(&ui_clip_from_pixels([320, 180]));
        let top_left = m.project_point3(Vec3::new(0.0, 0.0, 0.0));
        let bottom_right = m.project_point3(Vec3::new(320.0, 180.0, 0.0));
        assert!((top_left - Vec3::new(-1.0, 1.0, 0.0)).length() < 1e-6);
        assert!((bottom_right - Vec3::new(1.0, -1.0, 0.0)).length() < 1e-6);
    }
}
