//! Shadow views and the shadow atlas: cascade splits and camera-fitted,
//! texel-snapped cascades for directional lights, perspective views for
//! spot lights, six cube faces for point and area lights, and packing of
//! power-of-two views into atlas pages (layers of one depth texture array).

use crate::lights::{LightKind, ShadowView};
use anyhow::{Result, ensure};
use glam::{Mat4, Vec3, Vec4Swizzles};

/// One allocated atlas tile that a [`crate::PipelineFeature`] may add casters to.
///
/// Available only during [`crate::Stage::Shadow`]. Indices and atlas placement
/// belong to this frame and view; do not cache them across frames. The pipeline
/// owns allocation and clears each used layer before any shadow feature runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowRenderView {
    /// Index into `engine_shadows`, identical to this entry's slice index.
    pub index: u32,
    /// Index of the light in the submitted [`crate::LightList`].
    pub light_index: usize,
    /// Index of the light in the uploaded `engine_lights` list (directional first).
    pub light_slot: u32,
    pub light_kind: LightKind,
    /// Cascade index for directional lights; cube face (+X, -X, +Y, -Y,
    /// +Z, -Z) for point/area lights; zero for a spot light.
    pub view_index: u32,
    /// Depth-array layer and square tile in atlas texels (top-left origin).
    pub layer: u32,
    pub x: u32,
    pub y: u32,
    pub size: u32,
    /// The exact projection used by receivers, including its world-to-clip
    /// matrix, near/far range, cascade split and texel scale.
    pub view: ShadowView,
}

/// Shadow atlas pages of a view: `page_size`^2 `Depth32Float` layers,
/// allocated on demand up to `max_pages`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowAtlasConfig {
    /// Power of two in 512..=8192 (default 4096).
    pub page_size: u32,
    /// 1..=8 (default 4).
    pub max_pages: u32,
}

impl Default for ShadowAtlasConfig {
    fn default() -> Self {
        Self {
            page_size: 4096,
            max_pages: 4,
        }
    }
}

impl ShadowAtlasConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (512..=8192).contains(&self.page_size) && self.page_size.is_power_of_two(),
            "shadow atlas pages are a power of two in 512..=8192 texels"
        );
        ensure!(
            (1..=8).contains(&self.max_pages),
            "the shadow atlas has 1..=8 pages"
        );
        Ok(())
    }

    /// Texels of all pages.
    pub fn capacity(&self) -> u64 {
        u64::from(self.page_size).pow(2) * u64::from(self.max_pages)
    }
}

/// Camera view depths `[near, split_1, .., far]` of `count` cascades with the
/// practical split scheme: `lambda * log + (1 - lambda) * uniform`.
pub fn cascade_splits(near: f32, far: f32, count: u32, lambda: f32) -> Vec<f32> {
    let count = count.max(1);
    (0..=count)
        .map(|i| {
            if i == 0 {
                return near;
            }
            if i == count {
                return far;
            }
            let f = i as f32 / count as f32;
            let log = near * (far / near).powf(f);
            let uniform = near + (far - near) * f;
            lambda * log + (1.0 - lambda) * uniform
        })
        .collect()
}

/// View-space depth range (positive distances) of a projection's near and
/// far planes.
pub fn projection_depth_range(clip_from_view: Mat4) -> (f32, f32) {
    let view_from_clip = clip_from_view.inverse();
    let depth = |z: f32| {
        let p = view_from_clip * glam::Vec4::new(0.0, 0.0, z, 1.0);
        -p.z / p.w
    };
    (depth(0.0), depth(1.0))
}

/// View-space corners of the frustum slice between view depths `d0` and
/// `d1`: four at `d0`, then four at `d1`.
fn slice_corners(view_from_clip: Mat4, d0: f32, d1: f32) -> [Vec3; 8] {
    let mut corners = [Vec3::ZERO; 8];
    for (i, (x, y)) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
        .into_iter()
        .enumerate()
    {
        let near = view_from_clip * glam::Vec4::new(x, y, 0.0, 1.0);
        let far = view_from_clip * glam::Vec4::new(x, y, 1.0, 1.0);
        let (near, far) = (near.xyz() / near.w, far.xyz() / far.w);
        let at = |depth: f32| {
            let t = (depth + near.z) / (near.z - far.z);
            near.lerp(far, t)
        };
        corners[i] = at(d0);
        corners[i + 4] = at(d1);
    }
    corners
}

/// Bounding sphere (view space) of a frustum slice. It depends only on the
/// projection and the depths, so it is identical every frame for a fixed
/// projection; the radius is rounded up to 1/64 m to keep it bit-stable.
pub fn slice_sphere(clip_from_view: Mat4, d0: f32, d1: f32) -> (Vec3, f32) {
    let corners = slice_corners(clip_from_view.inverse(), d0, d1);
    let a = (corners[0] + corners[1] + corners[2] + corners[3]) * 0.25;
    let b = (corners[4] + corners[5] + corners[6] + corners[7]) * 0.25;
    let rn2 = corners[..4]
        .iter()
        .map(|c| c.distance_squared(a))
        .fold(0.0, f32::max);
    let rf2 = corners[4..]
        .iter()
        .map(|c| c.distance_squared(b))
        .fold(0.0, f32::max);
    let length = a.distance(b);
    let x = if length > 1e-6 {
        ((rf2 - rn2 + length * length) / (2.0 * length)).clamp(0.0, length)
    } else {
        0.0
    };
    let center = if length > 1e-6 {
        a + (b - a) * (x / length)
    } else {
        a
    };
    let radius = corners
        .iter()
        .map(|c| c.distance(center))
        .fold(0.0, f32::max);
    (center, (radius * 64.0).ceil() / 64.0)
}

/// Rotation of a light-space basis looking along `direction`.
fn light_rotation(direction: Vec3) -> Mat4 {
    let up = if direction.y.abs() > 0.99 {
        Vec3::X
    } else {
        Vec3::Y
    };
    glam::camera::rh::view::look_to_mat4(Vec3::ZERO, direction, up)
}

/// One camera-fitted cascade of a directional light travelling along
/// `direction`: an orthographic view around the bounding sphere of the
/// camera frustum slice `split` (view depths), with the light-space centre
/// snapped to whole texels so the shadow texel grid stays fixed in world
/// space while the camera moves. Casters up to `caster_extension` metres
/// beyond the sphere toward the light are included.
pub fn cascade_view(
    world_from_view: Mat4,
    clip_from_view: Mat4,
    split: [f32; 2],
    direction: Vec3,
    resolution: u32,
    caster_extension: f32,
) -> ShadowView {
    let (center_view, radius) = slice_sphere(clip_from_view, split[0], split[1]);
    let center_world = world_from_view.transform_point3(center_view);
    let rotation = light_rotation(direction.normalize());
    let texel = 2.0 * radius / resolution as f32;
    let center_light = rotation.transform_point3(center_world);
    let snapped = (center_light / texel).floor() * texel;
    let depth = 2.0 * radius + caster_extension;
    // Light space with the near plane (toward the light) at distance 0.
    let eye = snapped + Vec3::Z * (radius + caster_extension);
    let view = Mat4::from_translation(-eye) * rotation;
    let projection =
        glam::camera::rh::proj::directx::orthographic(-radius, radius, -radius, radius, 0.0, depth);
    ShadowView {
        clip_from_world: projection * view,
        resolution,
        near: 0.0,
        far: depth,
        perspective: false,
        texel_world: texel,
        split,
    }
}

/// Tangent of the half field of view of a perspective shadow view covering
/// `tan_half` plus `margin` texels on each side (for filter kernels).
fn widened(tan_half: f32, resolution: u32, margin: u32) -> f32 {
    tan_half * (1.0 + 2.0 * margin as f32 / resolution as f32)
}

fn perspective_view(
    position: Vec3,
    direction: Vec3,
    up: Vec3,
    tan_half: f32,
    near: f32,
    far: f32,
    resolution: u32,
) -> ShadowView {
    let view = glam::camera::rh::view::look_to_mat4(position, direction, up);
    let projection =
        glam::camera::rh::proj::directx::perspective(2.0 * tan_half.atan(), 1.0, near, far);
    ShadowView {
        clip_from_world: projection * view,
        resolution,
        near,
        far,
        perspective: true,
        texel_world: 2.0 * tan_half / resolution as f32,
        split: [0.0, 0.0],
    }
}

/// Largest half field of view of a spot shadow (80 degrees); wider cones
/// are unshadowed outside it.
pub const MAX_SPOT_SHADOW_HALF_ANGLE: f32 = 80.0 * std::f32::consts::PI / 180.0;

/// The perspective view of a spot light's cone (`outer_angle` half angle,
/// widened by `margin` texels).
pub fn spot_view(
    position: Vec3,
    direction: Vec3,
    outer_angle: f32,
    near: f32,
    range: f32,
    resolution: u32,
    margin: u32,
) -> ShadowView {
    let direction = direction.normalize();
    let up = if direction.y.abs() > 0.99 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let tan_half = widened(
        outer_angle.min(MAX_SPOT_SHADOW_HALF_ANGLE).tan(),
        resolution,
        margin,
    );
    perspective_view(
        position,
        direction,
        up,
        tan_half,
        near,
        range.max(near * 2.0),
        resolution,
    )
}

/// Cube face axes in `EngineShadow` order: +X, -X, +Y, -Y, +Z, -Z (the
/// shader picks the face of the major axis of `world - light`).
pub const CUBE_FACES: [(Vec3, Vec3); 6] = [
    (Vec3::X, Vec3::Y),
    (Vec3::NEG_X, Vec3::Y),
    (Vec3::Y, Vec3::Z),
    (Vec3::NEG_Y, Vec3::Z),
    (Vec3::Z, Vec3::Y),
    (Vec3::NEG_Z, Vec3::Y),
];

/// Six 90-degree faces (widened by `margin` texels) around `position`.
pub fn cube_views(
    position: Vec3,
    near: f32,
    range: f32,
    resolution: u32,
    margin: u32,
) -> [ShadowView; 6] {
    let tan_half = widened(1.0, resolution, margin);
    CUBE_FACES.map(|(direction, up)| {
        perspective_view(
            position,
            direction,
            up,
            tan_half,
            near,
            range.max(near * 2.0),
            resolution,
        )
    })
}

/// Where one shadow view lives in the atlas (texels).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasSlot {
    pub page: u32,
    pub x: u32,
    pub y: u32,
    pub size: u32,
}

/// `index` with its even bits gathered (Morton decode of one axis).
fn compact_bits(mut index: u64) -> u32 {
    index &= 0x5555_5555_5555_5555;
    index = (index | (index >> 1)) & 0x3333_3333_3333_3333;
    index = (index | (index >> 2)) & 0x0f0f_0f0f_0f0f_0f0f;
    index = (index | (index >> 4)) & 0x00ff_00ff_00ff_00ff;
    index = (index | (index >> 8)) & 0x0000_ffff_0000_ffff;
    index = (index | (index >> 16)) & 0x0000_0000_ffff_ffff;
    index as u32
}

/// Place power-of-two squares (`sizes`, each at most the page size) into
/// pages. Squares are placed largest first along a Morton curve, so a page
/// fills without gaps; `None` when they need more than `max_pages`.
/// Slots are returned in the order of `sizes`.
pub fn pack_atlas(sizes: &[u32], config: &ShadowAtlasConfig) -> Option<Vec<AtlasSlot>> {
    let page_area = u64::from(config.page_size).pow(2);
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by(|&a, &b| sizes[b].cmp(&sizes[a]));
    let mut slots = vec![
        AtlasSlot {
            page: 0,
            x: 0,
            y: 0,
            size: 0
        };
        sizes.len()
    ];
    let (mut page, mut cursor) = (0u32, 0u64);
    for index in order {
        let size = sizes[index];
        if !size.is_power_of_two() || size > config.page_size {
            return None;
        }
        let area = u64::from(size).pow(2);
        if cursor + area > page_area {
            page += 1;
            cursor = 0;
        }
        if page >= config.max_pages {
            return None;
        }
        let cell = cursor / area;
        slots[index] = AtlasSlot {
            page,
            x: compact_bits(cell) * size,
            y: compact_bits(cell >> 1) * size,
            size,
        };
        cursor += area;
    }
    Some(slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3Swizzles;

    fn perspective() -> Mat4 {
        glam::camera::rh::proj::directx::perspective(1.0, 16.0 / 9.0, 0.1, 200.0)
    }

    #[test]
    fn practical_splits_interpolate_uniform_and_logarithmic() {
        let uniform = cascade_splits(1.0, 101.0, 4, 0.0);
        assert_eq!(uniform, vec![1.0, 26.0, 51.0, 76.0, 101.0]);
        let log = cascade_splits(1.0, 10_000.0, 4, 1.0);
        for (value, expected) in log.iter().zip([1.0, 10.0, 100.0, 1000.0, 10_000.0]) {
            assert!((value - expected).abs() / expected < 1e-4, "{log:?}");
        }
        let practical = cascade_splits(0.1, 100.0, 3, 0.5);
        assert_eq!(practical.len(), 4);
        assert!(practical.windows(2).all(|w| w[0] < w[1]), "{practical:?}");
        let (u, l) = (
            cascade_splits(0.1, 100.0, 3, 0.0)[1],
            cascade_splits(0.1, 100.0, 3, 1.0)[1],
        );
        assert!((practical[1] - 0.5 * (u + l)).abs() < 1e-4);
        assert_eq!(cascade_splits(0.5, 9.0, 1, 0.7), vec![0.5, 9.0]);
    }

    #[test]
    fn depth_range_and_slice_spheres_bound_the_frustum() {
        let (near, far) = projection_depth_range(perspective());
        assert!((near - 0.1).abs() < 1e-4 && (far - 200.0).abs() < 0.05);
        let (center, radius) = slice_sphere(perspective(), 5.0, 20.0);
        for corner in slice_corners(perspective().inverse(), 5.0, 20.0) {
            assert!(corner.distance(center) <= radius + 1e-4);
        }
        assert!(center.x.abs() < 1e-4 && center.y.abs() < 1e-4 && center.z < -5.0);
        // A tighter bound than the slice's diagonal.
        let corners = slice_corners(perspective().inverse(), 5.0, 20.0);
        assert!(radius < corners[0].distance(corners[6]));
    }

    #[test]
    fn cascades_keep_the_texel_grid_fixed_in_world_space() {
        let direction = Vec3::new(0.3, -1.0, 0.2).normalize();
        let project = |camera_x: f32| {
            let world_from_view = Mat4::from_translation(Vec3::new(camera_x, 2.0, 5.0));
            cascade_view(
                world_from_view,
                perspective(),
                [0.1, 12.0],
                direction,
                512,
                50.0,
            )
        };
        let a = project(0.0);
        let b = project(0.013);
        assert_eq!(a.texel_world, b.texel_world, "fixed radius");
        // The texel coordinates of a world point differ by whole texels.
        for point in [
            Vec3::ZERO,
            Vec3::new(3.0, -1.0, 2.5),
            Vec3::new(-7.0, 0.5, -4.0),
        ] {
            let texel = |view: &ShadowView| {
                let clip = view.clip_from_world * point.extend(1.0);
                (clip.xy() / clip.w * 0.5 + 0.5) * view.resolution as f32
            };
            let delta = texel(&a) - texel(&b);
            assert!(
                (delta - delta.round()).abs().max_element() < 2e-3,
                "fractional texel shift {delta}"
            );
        }
        // The slice is inside the cascade.
        let world_from_view = Mat4::from_translation(Vec3::new(0.0, 2.0, 5.0));
        for corner in slice_corners(perspective().inverse(), 0.1, 12.0) {
            let clip = a.clip_from_world * world_from_view.transform_point3(corner).extend(1.0);
            let ndc = clip.xyz() / clip.w;
            assert!(ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0, "{ndc}");
            assert!((0.0..=1.0).contains(&ndc.z), "{ndc}");
        }
    }

    #[test]
    fn spot_and_cube_views_cover_their_volumes() {
        let spot = spot_view(
            Vec3::new(0.0, 4.0, 0.0),
            Vec3::NEG_Y,
            0.5,
            0.05,
            10.0,
            512,
            2,
        );
        let below = spot.clip_from_world * Vec3::new(0.0, 0.0, 0.0).extend(1.0);
        let ndc = below.xyz() / below.w;
        assert!(ndc.xy().length() < 1e-4 && (0.0..1.0).contains(&ndc.z));
        let edge = Vec3::new(4.0 * 0.5f32.tan() * 0.99, 0.0, 0.0);
        let clip = spot.clip_from_world * edge.extend(1.0);
        assert!((clip.x / clip.w).abs() < 1.0, "cone edge inside the view");
        let faces = cube_views(Vec3::ONE, 0.05, 20.0, 256, 2);
        for (face, (axis, _)) in faces.iter().zip(CUBE_FACES) {
            let clip = face.clip_from_world * (Vec3::ONE + axis * 3.0).extend(1.0);
            let ndc = clip.xyz() / clip.w;
            assert!(ndc.xy().length() < 1e-4 && (0.0..1.0).contains(&ndc.z));
        }
    }

    #[test]
    fn atlas_packing_fills_pages_without_overlap() {
        let config = ShadowAtlasConfig {
            page_size: 1024,
            max_pages: 2,
        };
        let sizes = [256, 1024, 512, 256, 512, 256, 256, 512];
        let slots = pack_atlas(&sizes, &config).unwrap();
        for (slot, size) in slots.iter().zip(sizes) {
            assert_eq!(slot.size, size);
            assert!(slot.x + size <= 1024 && slot.y + size <= 1024 && slot.page < 2);
        }
        for (i, a) in slots.iter().enumerate() {
            for b in &slots[i + 1..] {
                let apart = a.page != b.page
                    || a.x + a.size <= b.x
                    || b.x + b.size <= a.x
                    || a.y + a.size <= b.y
                    || b.y + b.size <= a.y;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
        assert_eq!(slots[1].page, 0, "the largest view is placed first");
        assert!(pack_atlas(&[1024, 1024, 64], &config).is_none(), "full");
        assert!(pack_atlas(&[2048], &config).is_none(), "larger than a page");
        assert!(ShadowAtlasConfig::default().validate().is_ok());
        assert!(
            ShadowAtlasConfig {
                page_size: 3000,
                max_pages: 1
            }
            .validate()
            .is_err()
        );
    }
}
