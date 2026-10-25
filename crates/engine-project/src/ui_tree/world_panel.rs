//! World-space UI panels: a `UiRoot` rendered onto a quad placed by the
//! entity's transform, with pointer picking through a camera ray.
//!
//! Without the `ui.world` render feature the panel's UI draws are clipped on
//! the CPU and re-emitted as `DrawSpace::World` meshes for the 2D camera. With
//! it ([`extract_items`], routed by `UiGpuQueue`) the same draws become
//! panel-local meshes that the feature places, billboards, sorts and
//! depth-tests in the 3D frame. See `docs/ui-render.md`.
use super::{
    localize,
    resolve::{any_themed, effective_for_render, states_by_root},
    world::{self, TickParts},
    *,
};
use crate::{
    Camera2D,
    content::{Mesh, Texture, Vertex},
    rendering3d::{Camera3D, Projection3D},
    runtime::{Draw, DrawSpace, DrawState},
};
use bevy_ecs::prelude::{Entity, Resource, World};
use glam::{Mat4, Vec3};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Largest panel surface edge in pixels.
pub const MAX_PANEL_PIXELS: u32 = 4096;
/// Panel-local depth offset between consecutive draws, so stacked layers do
/// not share a plane (world units).
pub const PANEL_Z_STEP: f32 = 1.0e-4;
const PANEL_Z_MAX: f32 = 0.05;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiBillboard {
    /// Use the entity's rotation.
    #[default]
    None,
    /// Face the active 3D camera on every axis.
    Full,
    /// Rotate about the Y axis only, staying upright.
    Yaw,
}

/// Which camera converts the screen pointer into a ray.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiWorldPointer {
    /// The active `Camera3D` when there is one, else the `Camera2D`.
    #[default]
    Auto,
    Camera2d,
    Camera3d,
    /// Not interactive.
    None,
}

/// Renders the `engine.UiRoot` on the same entity onto a world-space quad
/// (`engine.UiWorldPanel`). The root leaves the screen-space UI.
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiWorldPanel {
    pub version: u32,
    /// Quad size in world units (before the entity's scale).
    pub size: [f32; 2],
    /// UI pixels per world unit; `size * pixelsPerUnit` is the UI viewport.
    pub pixels_per_unit: f32,
    /// Quad origin as a fraction of `size`, from the top-left (0.5, 0.5 is centred).
    pub pivot: [f32; 2],
    pub billboard: UiBillboard,
    pub pointer: UiWorldPointer,
    /// Also receive keyboard/gamepad navigation actions (`ui_next`, `ui_confirm`, ...).
    pub keyboard: bool,
}

impl Default for UiWorldPanel {
    fn default() -> Self {
        Self {
            version: 1,
            size: [2.0, 1.0],
            pixels_per_unit: 100.0,
            pivot: [0.5, 0.5],
            billboard: UiBillboard::None,
            pointer: UiWorldPointer::Auto,
            keyboard: false,
        }
    }
}

fn panel_error(message: impl Into<String>) -> Error {
    Error::new("ui.world_panel.invalid", message)
}

impl UiWorldPanel {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(panel_error("Unsupported UiWorldPanel version"));
        }
        let ok = self
            .size
            .iter()
            .all(|v| v.is_finite() && *v > 0.0 && *v <= 10_000.0)
            && self.pixels_per_unit.is_finite()
            && (1.0..=4096.0).contains(&self.pixels_per_unit)
            && self
                .pivot
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v));
        if !ok {
            return Err(panel_error(
                "Panel size, pixelsPerUnit or pivot out of range",
            ));
        }
        let pixels = self.surface_pixels();
        if pixels
            .iter()
            .any(|p| *p < 1.0 || *p > MAX_PANEL_PIXELS as f32)
        {
            return Err(panel_error(
                "Panel surface (size x pixelsPerUnit) must be 1..=4096 pixels per axis",
            ));
        }
        Ok(())
    }

    fn surface_pixels(&self) -> [f32; 2] {
        [
            (self.size[0] * self.pixels_per_unit).round(),
            (self.size[1] * self.pixels_per_unit).round(),
        ]
    }

    /// UI viewport in pixels.
    pub fn pixel_size(&self) -> [u32; 2] {
        let p = self.surface_pixels();
        [
            (p[0] as u32).clamp(1, MAX_PANEL_PIXELS),
            (p[1] as u32).clamp(1, MAX_PANEL_PIXELS),
        ]
    }

    /// Panel-local position (before the entity matrix) of UI pixel `pixel`;
    /// the UI's y axis points down, the panel's y axis up.
    pub fn local_point(&self, pixel: [f32; 2]) -> Vec3 {
        let [w, h] = self.pixel_size();
        Vec3::new(
            pixel[0] / w as f32 * self.size[0] - self.size[0] * self.pivot[0],
            self.size[1] * self.pivot[1] - pixel[1] / h as f32 * self.size[1],
            0.0,
        )
    }

    /// World positions of the top-left, top-right, bottom-right and
    /// bottom-left UI pixel corners under `matrix`.
    pub fn corners(&self, matrix: Mat4) -> [[f32; 3]; 4] {
        let [w, h] = self.pixel_size();
        let (w, h) = (w as f32, h as f32);
        [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]]
            .map(|p| matrix.transform_point3(self.local_point(p)).to_array())
    }

    /// The matrix that places the quad: the entity's world matrix, rotated to
    /// face `camera` when billboarding. Position and scale are kept.
    pub fn placement(&self, world: Mat4, camera: Option<Vec3>) -> Mat4 {
        let Some(camera) = camera else {
            return world;
        };
        if self.billboard == UiBillboard::None {
            return world;
        }
        let position = world.w_axis.truncate();
        let scale = Vec3::new(
            world.x_axis.truncate().length(),
            world.y_axis.truncate().length(),
            world.z_axis.truncate().length(),
        );
        let mut forward = camera - position;
        if self.billboard == UiBillboard::Yaw {
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
        let up = forward.cross(right);
        Mat4::from_cols(
            (right * scale.x).extend(0.0),
            (up * scale.y).extend(0.0),
            (forward * scale.z).extend(0.0),
            position.extend(1.0),
        )
    }

    /// UI pixel under the ray `origin + t * direction` (`t >= 0`) when the
    /// panel is placed by `matrix`; `None` when the ray misses the quad or
    /// runs parallel to it.
    pub fn pick(&self, matrix: Mat4, origin: Vec3, direction: Vec3) -> Option<[f32; 2]> {
        if !matrix.is_finite() || matrix.determinant().abs() < 1e-12 {
            return None;
        }
        let inverse = matrix.inverse();
        let o = inverse.transform_point3(origin);
        let d = inverse.transform_vector3(direction);
        if d.z.abs() < 1e-9 {
            return None;
        }
        let t = -o.z / d.z;
        if t < 0.0 {
            return None;
        }
        let hit = o + d * t;
        let [w, h] = self.pixel_size();
        let pixel = [
            (hit.x + self.size[0] * self.pivot[0]) / self.size[0] * w as f32,
            (self.size[1] * self.pivot[1] - hit.y) / self.size[1] * h as f32,
        ];
        let inside =
            pixel[0] >= 0.0 && pixel[1] >= 0.0 && pixel[0] <= w as f32 && pixel[1] <= h as f32;
        inside.then_some(pixel)
    }
}

/// World-space ray through viewport pixel `pointer` of a 3D camera placed by
/// `camera_world`. `viewport` is the logical size the pointer is expressed in.
pub fn camera3d_ray(
    camera_world: Mat4,
    projection: &Projection3D,
    viewport: [u32; 2],
    pointer: [f32; 2],
) -> Option<(Vec3, Vec3)> {
    let [w, h] = viewport;
    if w == 0 || h == 0 || !pointer.iter().all(|v| v.is_finite()) || !camera_world.is_finite() {
        return None;
    }
    let aspect = w as f32 / h as f32;
    let ndc = [
        pointer[0] / w as f32 * 2.0 - 1.0,
        1.0 - pointer[1] / h as f32 * 2.0,
    ];
    let (origin, direction) = match projection {
        Projection3D::Perspective { fov_y_radians, .. } => {
            let half = (fov_y_radians / 2.0).tan();
            (
                Vec3::ZERO,
                Vec3::new(ndc[0] * half * aspect, ndc[1] * half, -1.0),
            )
        }
        Projection3D::Orthographic { height, .. } => (
            Vec3::new(ndc[0] * height * aspect / 2.0, ndc[1] * height / 2.0, 0.0),
            Vec3::NEG_Z,
        ),
    };
    let origin = camera_world.transform_point3(origin);
    let direction = camera_world.transform_vector3(direction);
    (direction.length_squared() > 0.0).then(|| (origin, direction.normalize()))
}

/// World-space ray through logical pixel `pointer` of a 2D camera (looking
/// down -Z at world z = 1000). Pixel y points down, world y up.
pub fn camera2d_ray(
    camera: &Camera2D,
    center: [f32; 2],
    pointer: [f32; 2],
) -> Option<(Vec3, Vec3)> {
    let [w, h] = camera.logical_size;
    if w == 0 || h == 0 || !pointer.iter().all(|v| v.is_finite()) {
        return None;
    }
    let ppu = camera.pixels_per_unit;
    let x = center[0] + (pointer[0] - w as f32 / 2.0) / ppu;
    let y = center[1] - (pointer[1] - h as f32 / 2.0) / ppu;
    Some((Vec3::new(x, y, 1000.0), Vec3::NEG_Z))
}

/// First active `Camera3D` in authored-ID order, as the runtime picks it.
fn active_camera3d(world: &World) -> Option<(Entity, &Camera3D)> {
    world
        .iter_entities()
        .filter_map(|e| {
            let camera = e.get::<Camera3D>().filter(|c| c.active)?;
            Some((world::root_key(&e), e.id(), camera))
        })
        .min_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, e, c)| (e, c))
}

fn camera2d(world: &World) -> (Camera2D, [f32; 2]) {
    let camera = world
        .iter_entities()
        .find_map(|e| e.get::<Camera2D>().cloned())
        .unwrap_or_default();
    let mut center = camera.center;
    if let Some(id) = &camera.follow
        && let Some(entity) = crate::services::find_entity(world, id)
        && let Ok(t) = crate::world_transform(world, entity)
    {
        let p = t.position();
        center = [p[0], p[1]];
    }
    (camera, center)
}

fn camera_position(world: &World) -> Option<Vec3> {
    let (entity, _) = active_camera3d(world)?;
    let matrix = crate::world_transform(world, entity).ok()?.matrix;
    Some(Mat4::from_cols_array_2d(&matrix).w_axis.truncate())
}

fn pointer_ray(
    world: &World,
    mode: UiWorldPointer,
    viewport: [u32; 2],
    pointer: [f32; 2],
) -> Option<(Vec3, Vec3)> {
    let use3d = match mode {
        UiWorldPointer::None => return None,
        UiWorldPointer::Camera3d => true,
        UiWorldPointer::Camera2d => false,
        UiWorldPointer::Auto => active_camera3d(world).is_some(),
    };
    if use3d {
        let (entity, camera) = active_camera3d(world)?;
        let matrix = Mat4::from_cols_array_2d(&crate::world_transform(world, entity).ok()?.matrix);
        camera3d_ray(matrix, &camera.projection, viewport, pointer)
    } else {
        let (camera, center) = camera2d(world);
        camera2d_ray(&camera, center, pointer)
    }
}

/// One panel found in the world.
struct Panel {
    id: String,
    entity: Entity,
    root: UiRoot,
    panel: UiWorldPanel,
    /// The entity's world matrix, before billboarding.
    world: Mat4,
    /// `world` turned toward the active 3D camera's entity position.
    matrix: Mat4,
}

fn panels(world: &World) -> Result<Vec<Panel>> {
    let camera = camera_position(world);
    let mut out = vec![];
    for e in world.iter_entities() {
        let (Some(root), Some(panel)) = (e.get::<UiRoot>(), e.get::<UiWorldPanel>()) else {
            continue;
        };
        let matrix = Mat4::from_cols_array_2d(&crate::world_transform(world, e.id())?.matrix);
        out.push(Panel {
            id: world::root_key(&e),
            entity: e.id(),
            root: root.clone(),
            panel: panel.clone(),
            world: matrix,
            matrix: panel.placement(matrix, camera),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    if out.len() > MAX_UI_ROOTS {
        return Err(invalid("UI exceeds 64 world panels"));
    }
    Ok(out)
}

/// Per-panel interaction state (resource).
#[derive(Default, Resource)]
pub struct UiWorldPanels {
    states: BTreeMap<String, GameUi>,
}

impl UiWorldPanels {
    /// Interaction state of the panel whose root has this ID.
    pub fn get(&self, root: &str) -> Option<&GameUi> {
        self.states.get(root)
    }

    pub fn clear(&mut self) {
        self.states.clear();
    }
}

/// The input a panel sees: the pointer mapped into panel pixels (a missing hit
/// disables pointer actions) and, unless `keyboard`, no navigation actions.
fn panel_input(
    input: &crate::ActionFrame,
    hit: Option<[f32; 2]>,
    keyboard: bool,
) -> crate::ActionFrame {
    let mut frame = input.clone();
    match hit {
        Some(pixel) => frame.pointer = pixel,
        None => {
            frame.pointer = [f32::NAN; 2];
            frame.pressed.remove("ui_click");
            frame.wheel = [0.0; 2];
        }
    }
    if !keyboard {
        let keep = |a: &String| a == "ui_click" || !a.starts_with("ui_");
        frame.pressed.retain(keep);
        frame.held.retain(keep);
        frame.released.retain(keep);
    }
    frame
}

/// Tick every world panel with its own pointer and `GameUi`.
pub(super) fn tick(
    world: &mut World,
    parts: &mut TickParts,
    ctx: &UiTickContext<'_>,
    input: &crate::ActionFrame,
    live: &mut BTreeSet<String>,
) -> Result<()> {
    for panel in panels(world)? {
        panel.panel.validate()?;
        let hit = pointer_ray(world, panel.panel.pointer, ctx.viewport, input.pointer)
            .and_then(|(o, d)| panel.panel.pick(panel.matrix, o, d));
        let frame = panel_input(input, hit, panel.panel.keyboard);
        let panel_ctx = UiTickContext {
            viewport: panel.panel.pixel_size(),
            dt: ctx.dt,
            text: ctx.text,
        };
        let mut ui = parts.panels.states.remove(&panel.id).unwrap_or_default();
        live.insert(panel.id.clone());
        let result = world::process(
            world,
            parts,
            &mut ui,
            vec![(panel.id.clone(), panel.entity, panel.root)],
            &panel_ctx,
            &frame,
        );
        parts.panels.states.insert(panel.id, ui);
        result?;
    }
    let ids: BTreeSet<String> = parts.panels.states.keys().cloned().collect();
    for gone in ids.difference(live) {
        parts.panels.states.remove(gone);
    }
    Ok(())
}

/// The UI draws of one panel in panel pixels (screen-root pipeline: theme,
/// localisation, text scale, bindings), in the order the sprite pass sorts them.
fn panel_draws(
    world: &World,
    panel: &Panel,
    text: &dyn UiTextBackend,
    textures: &BTreeMap<String, Arc<Texture>>,
) -> Result<Vec<Draw>> {
    let states = world.get_resource::<UiWorldPanels>();
    let data = world.get_resource::<UiDataContext>();
    let bindings = world.get_resource::<UiBindingState>();
    let localization = world.get_resource::<crate::localization::Localization>();
    panel.panel.validate()?;
    let ui = states.and_then(|s| s.states.get(&panel.id));
    let by_root = if any_themed(world, std::iter::once(&panel.root)) {
        ui.map(states_by_root).unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    let effective = effective_for_render(world, &panel.id, &panel.root, &by_root)?;
    let root = effective.as_ref().unwrap_or(&panel.root);
    let localized = localization.and_then(|l| localize::localize_root(root, l));
    let root = localized.as_ref().unwrap_or(root);
    let scaled_text = crate::accessibility::UiScaling::of(world).text_scaled(root, text);
    let root = scaled_text.as_ref().unwrap_or(root);
    let mut state = ui.map(|u| u.render_state(&panel.id)).unwrap_or_default();
    state.themed = effective.is_some();
    if let (Some(data), Some(bindings)) = (data, bindings) {
        state.item_texts = bindings.item_texts(root, data);
    }
    root.extract_draws_with(&panel.id, panel.panel.pixel_size(), &state, text, textures)
}

/// Draw the world panels as `DrawSpace::World` meshes.
pub(super) fn extract(
    world: &World,
    text: &dyn UiTextBackend,
    textures: &BTreeMap<String, Arc<Texture>>,
) -> Result<Vec<Draw>> {
    let mut out = vec![];
    for panel in panels(world)? {
        let draws = panel_draws(world, &panel, text, textures)?;
        for (n, draw) in draws.into_iter().enumerate() {
            let z = (n as f32 * PANEL_Z_STEP).min(PANEL_Z_MAX);
            let mesh = world_mesh(&panel.panel, panel.matrix, &draw.mesh, draw.clip, z);
            if mesh.indices.is_empty() {
                continue;
            }
            out.push(Draw {
                id: format!("world:{}", draw.id),
                mesh: Arc::new(mesh),
                texture: draw.texture,
                layer: draw.layer,
                space: DrawSpace::World,
                clip: None,
                state: DrawState::default(),
                motion: None,
                offset: [0.0; 3],
            });
        }
    }
    Ok(out)
}

/// The world panels as items for the `ui.world` render feature: each panel's
/// draws clipped and mapped to panel-local units (x right, y up, z 0), with the
/// entity matrix and billboard mode left to the feature, which applies them
/// per view with that view's camera.
pub(super) fn extract_items(
    world: &World,
    text: &dyn UiTextBackend,
    textures: &BTreeMap<String, Arc<Texture>>,
) -> Result<Vec<WorldUiItem>> {
    let mut out = vec![];
    for panel in panels(world)? {
        let mut draws = panel_draws(world, &panel, text, textures)?;
        // The sprite pass orders equal-depth draws by layer, then ID.
        draws.sort_by(|a, b| a.layer.cmp(&b.layer).then_with(|| a.id.cmp(&b.id)));
        let layer = draws.iter().map(|d| d.layer).min().unwrap_or(0);
        let draws: Vec<WorldUiDraw> = draws
            .into_iter()
            .filter_map(|draw| {
                let mesh = world_mesh(&panel.panel, Mat4::IDENTITY, &draw.mesh, draw.clip, 0.0);
                let sdf = is_sdf_draw(&draw);
                (!mesh.indices.is_empty()).then(|| WorldUiDraw {
                    mesh: Arc::new(mesh),
                    texture: draw.texture,
                    sdf,
                })
            })
            .collect();
        if draws.is_empty() {
            continue;
        }
        out.push(WorldUiItem {
            id: format!("world:{}", panel.id),
            kind: WorldUiKind::Panel,
            layer,
            world_from_local: panel.world.to_cols_array_2d(),
            billboard: match panel.panel.billboard {
                UiBillboard::None => WorldBillboard::None,
                UiBillboard::Full => WorldBillboard::Full,
                UiBillboard::Yaw => WorldBillboard::Yaw,
            },
            draws,
        });
    }
    Ok(out)
}

/// `mesh` (UI pixels) clipped to `clip` and placed on the panel plane.
pub fn world_mesh(
    panel: &UiWorldPanel,
    matrix: Mat4,
    mesh: &Mesh,
    clip: Option<[f32; 4]>,
    z: f32,
) -> Mesh {
    let mut clipped = match clip {
        Some(clip) => clip_quads(mesh, clip),
        None => mesh.clone(),
    };
    for vertex in &mut clipped.vertices {
        let mut local = panel.local_point([vertex.position[0], vertex.position[1]]);
        local.z = z;
        vertex.position = matrix.transform_point3(local).to_array();
    }
    clipped
}

/// Geometry clipping for UI meshes, which are made of axis-aligned quads
/// (four vertices, indices `0,1,2,0,2,3`): each quad is cut to `clip`
/// (`[x, y, width, height]`) with its UVs and colours adjusted; anything else
/// is passed through unclipped.
pub fn clip_quads(mesh: &Mesh, clip: [f32; 4]) -> Mesh {
    let (cx0, cy0, cx1, cy1) = (clip[0], clip[1], clip[0] + clip[2], clip[1] + clip[3]);
    let mut out = Mesh::default();
    let quads = mesh.vertices.len() / 4;
    let regular = mesh.vertices.len().is_multiple_of(4)
        && mesh.indices.len() == quads * 6
        && mesh.indices.chunks(6).enumerate().all(|(q, c)| {
            let b = (q * 4) as u32;
            c == [b, b + 1, b + 2, b, b + 2, b + 3]
        });
    if !regular {
        return mesh.clone();
    }
    for quad in mesh.vertices.chunks(4) {
        let xs = quad.iter().map(|v| v.position[0]);
        let ys = quad.iter().map(|v| v.position[1]);
        let (x0, x1) = (
            xs.clone().fold(f32::MAX, f32::min),
            xs.fold(f32::MIN, f32::max),
        );
        let (y0, y1) = (
            ys.clone().fold(f32::MAX, f32::min),
            ys.fold(f32::MIN, f32::max),
        );
        let axis_aligned = quad.iter().all(|v| {
            (v.position[0] == x0 || v.position[0] == x1)
                && (v.position[1] == y0 || v.position[1] == y1)
        });
        if !axis_aligned || x1 <= x0 || y1 <= y0 {
            continue;
        }
        let (nx0, nx1) = (x0.max(cx0), x1.min(cx1));
        let (ny0, ny1) = (y0.max(cy0), y1.min(cy1));
        if nx1 <= nx0 || ny1 <= ny0 {
            continue;
        }
        let base = out.vertices.len() as u32;
        for v in quad {
            // Each corner moves inward by the clipped amount; UVs are affine
            // in position for an axis-aligned quad.
            let nx = if v.position[0] == x0 { nx0 } else { nx1 };
            let ny = if v.position[1] == y0 { ny0 } else { ny1 };
            let mut uv = v.uv;
            if let Some(o) = quad
                .iter()
                .find(|o| o.position[1] == v.position[1] && o.position[0] != v.position[0])
            {
                let k = (nx - v.position[0]) / (o.position[0] - v.position[0]);
                uv[0] += (o.uv[0] - v.uv[0]) * k;
                uv[1] += (o.uv[1] - v.uv[1]) * k;
            }
            if let Some(o) = quad
                .iter()
                .find(|o| o.position[0] == v.position[0] && o.position[1] != v.position[1])
            {
                let k = (ny - v.position[1]) / (o.position[1] - v.position[1]);
                uv[0] += (o.uv[0] - v.uv[0]) * k;
                uv[1] += (o.uv[1] - v.uv[1]) * k;
            }
            out.vertices.push(Vertex {
                position: [nx, ny, v.position[2]],
                uv,
                color: v.color,
            });
        }
        out.indices.extend([0, 1, 2, 0, 2, 3].map(|i| i + base));
    }
    out
}
