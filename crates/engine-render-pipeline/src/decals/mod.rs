//! Projected decals: oriented-box volumes projected onto scene depth.
//!
//! This module is the CPU side of `engine.decals` (see
//! `docs/rendering-decals.md`): the [`Decal`] description, input validation,
//! the bounded per-frame plan with its explicit overflow rule, the screen
//! rectangle each decal can touch, and the GPU uniform layout. The pass lives
//! in the private `gpu` module and runs in [`crate::Stage::OpaqueEffects`].

mod gpu;

use crate::resources::GpuTexture;
use anyhow::{Result, bail, ensure};
use glam::{Mat4, Quat, Vec3, Vec4};
use std::sync::Arc;

pub(crate) use gpu::{DecalShared, descriptor, install};

/// Most decals drawn per view and frame. Decals past this many (in list
/// order) are dropped and counted in [`DecalReport::dropped_overflow`].
pub const MAX_DECALS: usize = 64;
/// Most decals one view may submit. A longer list is rejected as invalid
/// input before any other work, bounding CPU cost of validation.
pub const MAX_SUBMITTED_DECALS: usize = 1024;
/// Largest accepted half extent or centre magnitude, in world units.
pub const MAX_DECAL_EXTENT: f32 = 100_000.0;
/// Largest accepted emissive radiance per channel (scene HDR units).
pub const MAX_DECAL_EMISSIVE: f32 = 1000.0;

/// How a decal's albedo combines with the lit scene colour underneath.
///
/// The pass runs after lighting, so these are approximations of changing the
/// surface albedo; they cannot reconstruct the original albedo or re-light
/// the surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DecalBlend {
    /// `lit * lerp(1, albedo, a)`: stains, grime and tint. Exact for a
    /// diffuse surface whose own albedo is multiplied by `lerp(1, albedo, a)`.
    #[default]
    Multiply,
    /// `lerp(lit, albedo * luma(lit), a)`: paint that replaces the hue and
    /// keeps the lit brightness. Exact for a white diffuse surface.
    Paint,
}

impl DecalBlend {
    pub(crate) const fn code(self) -> f32 {
        match self {
            Self::Multiply => 0.0,
            Self::Paint => 1.0,
        }
    }
}

/// The texture of a decal.
#[derive(Clone, Debug, Default)]
pub enum DecalTexture {
    /// A flat decal: albedo and opacity only.
    #[default]
    None,
    /// RGBA8 colour (sRGB), mapped over the local XY face of the box:
    /// u along +X, v along -Y. RGB multiplies the albedo, alpha the opacity.
    Texture(Arc<GpuTexture>),
    /// A texture was requested but could not be resolved. The decal renders
    /// as flat albedo and opacity, and [`DecalReport::missing_textures`]
    /// counts it. It is never silently dropped and never an error.
    Missing,
}

impl PartialEq for DecalTexture {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::None, Self::None) | (Self::Missing, Self::Missing) => true,
            (Self::Texture(a), Self::Texture(b)) => a.id() == b.id(),
            _ => false,
        }
    }
}

/// One oriented-box decal. The box is `center +- rotation * half_extents`;
/// the decal faces local +Z and projects toward local -Z, so a surface is
/// decorated when its normal points back toward +Z (within `max_angle`).
#[derive(Clone, Debug, PartialEq)]
pub struct Decal {
    pub center: Vec3,
    /// Orientation of the box; must be finite and nonzero (normalised on use).
    pub rotation: Quat,
    /// Half size along local X, Y and Z, each in `(0, MAX_DECAL_EXTENT]`.
    pub half_extents: Vec3,
    /// Linear albedo, each channel `0..=1`.
    pub albedo: Vec3,
    /// Opacity, `0..=1`.
    pub opacity: f32,
    pub blend: DecalBlend,
    /// Radiance added where the decal covers the surface: scaled by the
    /// texture alpha, edge fade and angle fade, but not by `opacity`. Each
    /// channel `0..=MAX_DECAL_EMISSIVE`.
    pub emissive: Vec3,
    pub texture: DecalTexture,
    /// Largest angle in radians between the surface normal and the decal
    /// axis (+Z) that still receives the decal, `(0, pi/2]`.
    pub max_angle: f32,
    /// Width in radians of the fade below `max_angle`, `0..=max_angle`
    /// (0 is a hard cut).
    pub angle_fade: f32,
    /// Fade width as a fraction of the half extent at every face of the box,
    /// `0..=1` (0 is a hard edge).
    pub edge_fade: f32,
}

impl Default for Decal {
    fn default() -> Self {
        Self {
            center: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            half_extents: Vec3::splat(0.5),
            albedo: Vec3::ONE,
            opacity: 1.0,
            blend: DecalBlend::Multiply,
            emissive: Vec3::ZERO,
            texture: DecalTexture::None,
            max_angle: 70f32.to_radians(),
            angle_fade: 15f32.to_radians(),
            edge_fade: 0.1,
        }
    }
}

fn in_range(value: f32, low: f32, high: f32) -> bool {
    value.is_finite() && (low..=high).contains(&value)
}

impl Decal {
    /// Reject non-finite transforms, zero or negative extents, and values
    /// outside their documented ranges. Pure; does not touch the GPU.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.center.is_finite() && self.center.abs().max_element() <= MAX_DECAL_EXTENT,
            "decal center must be finite and within {MAX_DECAL_EXTENT} of the origin"
        );
        ensure!(
            self.rotation.is_finite() && self.rotation.length_squared() > 1e-12,
            "decal rotation must be a finite nonzero quaternion"
        );
        ensure!(
            self.half_extents.is_finite()
                && self.half_extents.min_element() > 0.0
                && self.half_extents.max_element() <= MAX_DECAL_EXTENT,
            "decal half extents must be finite, greater than zero and at most {MAX_DECAL_EXTENT}"
        );
        ensure!(
            self.albedo.is_finite()
                && self.albedo.min_element() >= 0.0
                && self.albedo.max_element() <= 1.0,
            "decal albedo channels must be finite in 0..=1"
        );
        ensure!(
            in_range(self.opacity, 0.0, 1.0),
            "decal opacity must be finite in 0..=1"
        );
        ensure!(
            self.emissive.is_finite()
                && self.emissive.min_element() >= 0.0
                && self.emissive.max_element() <= MAX_DECAL_EMISSIVE,
            "decal emissive channels must be finite in 0..={MAX_DECAL_EMISSIVE}"
        );
        ensure!(
            self.max_angle.is_finite()
                && self.max_angle > 0.0
                && self.max_angle <= std::f32::consts::FRAC_PI_2,
            "decal max_angle must be finite in (0, pi/2]"
        );
        ensure!(
            in_range(self.angle_fade, 0.0, self.max_angle),
            "decal angle_fade must be finite in 0..=max_angle"
        );
        ensure!(
            in_range(self.edge_fade, 0.0, 1.0),
            "decal edge_fade must be finite in 0..=1"
        );
        Ok(())
    }

    /// World-from-unit-box: maps the cube `[-1, 1]^3` onto the decal volume.
    pub fn world_from_box(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(
            self.half_extents,
            self.rotation.normalize(),
            self.center,
        )
    }

    /// World-space unit projection axis: the decal's local +Z, the direction
    /// a receiving surface's normal should face.
    pub fn axis(&self) -> Vec3 {
        self.rotation.normalize() * Vec3::Z
    }
}

/// What the decal pass did for one view in its last frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecalReport {
    /// Decals the view submitted.
    pub submitted: usize,
    /// Decals encoded (inside the budget and on screen).
    pub drawn: usize,
    /// Decals whose volume lies entirely outside the view, or that change
    /// nothing (zero opacity and zero emissive).
    pub culled: usize,
    /// Decals past [`MAX_DECALS`] that were dropped.
    pub dropped_overflow: usize,
    /// Decals (within the budget) whose requested texture was missing.
    pub missing_textures: usize,
}

/// A pixel rectangle `x0..x1`, `y0..y1` (top-left origin) in render pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl PixelRect {
    pub fn width(&self) -> u32 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> u32 {
        self.y1 - self.y0
    }
}

/// The render-pixel rectangle a decal volume can cover, or `None` when the
/// volume is entirely off screen or behind the camera. When the box straddles
/// the camera plane the whole target is returned (conservative).
pub fn screen_rect(
    world_from_box: Mat4,
    clip_from_world: Mat4,
    render_size: [u32; 2],
) -> Option<PixelRect> {
    const EPS: f32 = 1e-5;
    let clip_from_box = clip_from_world * world_from_box;
    let mut behind = 0;
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for corner in 0..8u32 {
        let local = Vec3::new(
            if corner & 1 == 0 { -1.0 } else { 1.0 },
            if corner & 2 == 0 { -1.0 } else { 1.0 },
            if corner & 4 == 0 { -1.0 } else { 1.0 },
        );
        let clip: Vec4 = clip_from_box * local.extend(1.0);
        if clip.w <= EPS {
            behind += 1;
            continue;
        }
        let ndc = [clip.x / clip.w, clip.y / clip.w];
        for axis in 0..2 {
            min[axis] = min[axis].min(ndc[axis]);
            max[axis] = max[axis].max(ndc[axis]);
        }
    }
    let full = PixelRect {
        x0: 0,
        y0: 0,
        x1: render_size[0],
        y1: render_size[1],
    };
    if behind == 8 {
        return None;
    }
    if behind > 0 || !min.iter().chain(&max).all(|v| v.is_finite()) {
        return Some(full);
    }
    let (w, h) = (render_size[0] as f32, render_size[1] as f32);
    // One pixel of padding absorbs rounding and sub-pixel jitter.
    let x0 = ((min[0] * 0.5 + 0.5) * w - 1.0).floor().max(0.0);
    let x1 = ((max[0] * 0.5 + 0.5) * w + 1.0).ceil().min(w);
    let y0 = ((0.5 - max[1] * 0.5) * h - 1.0).floor().max(0.0);
    let y1 = ((0.5 - min[1] * 0.5) * h + 1.0).ceil().min(h);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(PixelRect {
        x0: x0 as u32,
        y0: y0 as u32,
        x1: x1 as u32,
        y1: y1 as u32,
    })
}

/// The per-decal GPU uniform (`DecalParams` in `decals.wgsl`, 208 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DecalUniform {
    /// Inverse of the jittered view-projection.
    pub world_from_clip: [[f32; 4]; 4],
    /// Box-local coordinates: the decal volume is `[-1, 1]^3` there.
    pub box_from_world: [[f32; 4]; 4],
    /// xyz world projection axis.
    pub axis: [f32; 4],
    /// rgb albedo, a opacity.
    pub albedo_opacity: [f32; 4],
    /// rgb emissive, w blend code.
    pub emissive_blend: [f32; 4],
    /// x edge fade, y cos(max_angle), z cos(max_angle - angle_fade),
    /// w 1 when a texture is bound.
    pub params: [f32; 4],
    /// xy render size, zw reciprocals.
    pub size: [f32; 4],
}

/// Byte size of [`DecalUniform`].
pub const DECAL_UNIFORM_BYTES: u64 = std::mem::size_of::<DecalUniform>() as u64;

impl DecalUniform {
    pub fn new(decal: &Decal, world_from_clip: Mat4, render_size: [u32; 2]) -> Self {
        let size = [render_size[0] as f32, render_size[1] as f32];
        let textured = matches!(decal.texture, DecalTexture::Texture(_));
        Self {
            world_from_clip: world_from_clip.to_cols_array_2d(),
            box_from_world: decal.world_from_box().inverse().to_cols_array_2d(),
            axis: decal.axis().extend(0.0).to_array(),
            albedo_opacity: decal.albedo.extend(decal.opacity).to_array(),
            emissive_blend: decal.emissive.extend(decal.blend.code()).to_array(),
            params: [
                decal.edge_fade,
                decal.max_angle.cos().max(0.0),
                (decal.max_angle - decal.angle_fade).cos().max(0.0),
                if textured { 1.0 } else { 0.0 },
            ],
            size: [size[0], size[1], 1.0 / size[0], 1.0 / size[1]],
        }
    }
}

/// One decal chosen for drawing.
#[derive(Clone, Debug)]
pub(crate) struct PlannedDecal {
    /// Index into the submitted list.
    pub index: usize,
    pub rect: PixelRect,
    pub uniform: DecalUniform,
}

/// The validated, bounded draw list of one view and frame.
#[derive(Clone, Debug)]
pub(crate) struct DecalPlan {
    pub items: Vec<PlannedDecal>,
    pub report: DecalReport,
}

/// Validate `decals` and choose what to draw, in list order (later decals
/// draw on top). Invalid input is an error naming the decal; a list longer
/// than [`MAX_SUBMITTED_DECALS`] is an error; a valid list longer than
/// [`MAX_DECALS`] keeps its first [`MAX_DECALS`] entries and counts the rest.
pub(crate) fn plan(
    decals: &[Decal],
    clip_from_world: Mat4,
    world_from_clip: Mat4,
    render_size: [u32; 2],
) -> Result<DecalPlan> {
    if decals.len() > MAX_SUBMITTED_DECALS {
        bail!(
            "{} decals submitted; a view accepts at most {MAX_SUBMITTED_DECALS}",
            decals.len()
        );
    }
    for (index, decal) in decals.iter().enumerate() {
        decal
            .validate()
            .map_err(|error| anyhow::anyhow!("decal {index}: {error}"))?;
    }
    let kept = decals.len().min(MAX_DECALS);
    let mut report = DecalReport {
        submitted: decals.len(),
        dropped_overflow: decals.len() - kept,
        ..Default::default()
    };
    let mut items = Vec::with_capacity(kept);
    for (index, decal) in decals.iter().take(kept).enumerate() {
        if decal.texture == DecalTexture::Missing {
            report.missing_textures += 1;
        }
        // A fully transparent decal changes nothing.
        let visible = decal.opacity > 0.0 || decal.emissive.max_element() > 0.0;
        let rect = visible
            .then(|| screen_rect(decal.world_from_box(), clip_from_world, render_size))
            .flatten();
        let Some(rect) = rect else {
            report.culled += 1;
            continue;
        };
        items.push(PlannedDecal {
            index,
            rect,
            uniform: DecalUniform::new(decal, world_from_clip, render_size),
        });
    }
    report.drawn = items.len();
    Ok(DecalPlan { items, report })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> (Mat4, Mat4) {
        let projection = glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.1, 100.0);
        (projection, projection.inverse())
    }

    fn at(z: f32) -> Decal {
        Decal {
            center: Vec3::new(0.0, 0.0, z),
            half_extents: Vec3::splat(0.5),
            ..Default::default()
        }
    }

    #[test]
    fn defaults_are_valid() {
        Decal::default().validate().unwrap();
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        let base = Decal::default;
        let bad: Vec<(&str, Decal)> = vec![
            (
                "nan center",
                Decal {
                    center: Vec3::new(f32::NAN, 0.0, 0.0),
                    ..base()
                },
            ),
            (
                "inf center",
                Decal {
                    center: Vec3::new(0.0, f32::INFINITY, 0.0),
                    ..base()
                },
            ),
            (
                "huge center",
                Decal {
                    center: Vec3::splat(1e9),
                    ..base()
                },
            ),
            (
                "zero rotation",
                Decal {
                    rotation: Quat::from_xyzw(0.0, 0.0, 0.0, 0.0),
                    ..base()
                },
            ),
            (
                "nan rotation",
                Decal {
                    rotation: Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0),
                    ..base()
                },
            ),
            (
                "zero extent",
                Decal {
                    half_extents: Vec3::new(1.0, 0.0, 1.0),
                    ..base()
                },
            ),
            (
                "negative extent",
                Decal {
                    half_extents: Vec3::new(1.0, 1.0, -1.0),
                    ..base()
                },
            ),
            (
                "nan extent",
                Decal {
                    half_extents: Vec3::new(f32::NAN, 1.0, 1.0),
                    ..base()
                },
            ),
            (
                "huge extent",
                Decal {
                    half_extents: Vec3::splat(1e9),
                    ..base()
                },
            ),
            (
                "albedo above one",
                Decal {
                    albedo: Vec3::new(1.5, 0.0, 0.0),
                    ..base()
                },
            ),
            (
                "albedo negative",
                Decal {
                    albedo: Vec3::new(-0.1, 0.0, 0.0),
                    ..base()
                },
            ),
            (
                "opacity above one",
                Decal {
                    opacity: 1.5,
                    ..base()
                },
            ),
            (
                "opacity negative",
                Decal {
                    opacity: -0.5,
                    ..base()
                },
            ),
            (
                "opacity nan",
                Decal {
                    opacity: f32::NAN,
                    ..base()
                },
            ),
            (
                "emissive negative",
                Decal {
                    emissive: Vec3::new(0.0, -1.0, 0.0),
                    ..base()
                },
            ),
            (
                "emissive huge",
                Decal {
                    emissive: Vec3::splat(1e6),
                    ..base()
                },
            ),
            (
                "zero max angle",
                Decal {
                    max_angle: 0.0,
                    ..base()
                },
            ),
            (
                "max angle above half pi",
                Decal {
                    max_angle: 2.0,
                    ..base()
                },
            ),
            (
                "angle fade above max",
                Decal {
                    max_angle: 0.5,
                    angle_fade: 0.6,
                    ..base()
                },
            ),
            (
                "edge fade above one",
                Decal {
                    edge_fade: 1.5,
                    ..base()
                },
            ),
            (
                "edge fade nan",
                Decal {
                    edge_fade: f32::NAN,
                    ..base()
                },
            ),
        ];
        for (name, decal) in bad {
            assert!(decal.validate().is_err(), "{name} must be rejected");
        }
    }

    #[test]
    fn boundary_values_are_accepted() {
        Decal {
            opacity: 0.0,
            edge_fade: 1.0,
            angle_fade: 0.0,
            max_angle: std::f32::consts::FRAC_PI_2,
            albedo: Vec3::ONE,
            emissive: Vec3::splat(MAX_DECAL_EMISSIVE),
            ..Default::default()
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn box_matrix_and_axis_follow_the_rotation() {
        let decal = Decal {
            center: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            half_extents: Vec3::new(1.0, 2.0, 3.0),
            ..Default::default()
        };
        // Local +Z rotated a quarter turn about Y points along +X.
        assert!(decal.axis().abs_diff_eq(Vec3::X, 1e-6));
        let corner = decal
            .world_from_box()
            .transform_point3(Vec3::new(1.0, 1.0, 1.0));
        // Local (1,1,1) -> scaled (1,2,3) -> rotated (3,2,-1) -> + center.
        assert!(
            corner.abs_diff_eq(Vec3::new(4.0, 4.0, 2.0), 1e-5),
            "{corner}"
        );
        let back = decal.world_from_box().inverse().transform_point3(corner);
        assert!(back.abs_diff_eq(Vec3::ONE, 1e-5));
    }

    #[test]
    fn screen_rect_covers_visible_volumes_and_culls_the_rest() {
        let (clip_from_world, _) = camera();
        let size = [100, 100];
        let centred = screen_rect(at(-5.0).world_from_box(), clip_from_world, size).unwrap();
        // Half extent 0.5 at depth 5 with a 1 radian fov: about 20% of the view.
        assert!(
            centred.width() >= 18 && centred.width() <= 30,
            "{centred:?}"
        );
        assert!(centred.x0 < 50 && centred.x1 > 50 && centred.y0 < 50 && centred.y1 > 50);
        let mut off = at(-5.0);
        off.center.x = 50.0;
        assert_eq!(
            screen_rect(off.world_from_box(), clip_from_world, size),
            None
        );
        assert_eq!(
            screen_rect(at(5.0).world_from_box(), clip_from_world, size),
            None,
            "behind the camera"
        );
        let mut straddle = at(0.0);
        straddle.half_extents = Vec3::splat(2.0);
        assert_eq!(
            screen_rect(straddle.world_from_box(), clip_from_world, size),
            Some(PixelRect {
                x0: 0,
                y0: 0,
                x1: 100,
                y1: 100
            }),
            "a box around the camera covers the target"
        );
    }

    #[test]
    fn plan_keeps_order_counts_culled_and_reports_missing_textures() {
        let (clip_from_world, world_from_clip) = camera();
        let mut off = at(-5.0);
        off.center.x = 50.0;
        let mut missing = at(-6.0);
        missing.texture = DecalTexture::Missing;
        let decals = [at(-5.0), off, missing];
        let plan = plan(&decals, clip_from_world, world_from_clip, [64, 64]).unwrap();
        assert_eq!(plan.report.submitted, 3);
        assert_eq!(plan.report.drawn, 2);
        assert_eq!(plan.report.culled, 1);
        assert_eq!(plan.report.missing_textures, 1);
        assert_eq!(plan.report.dropped_overflow, 0);
        assert_eq!(
            plan.items.iter().map(|i| i.index).collect::<Vec<_>>(),
            [0, 2]
        );
        // A missing texture renders flat.
        assert_eq!(plan.items[1].uniform.params[3], 0.0);
    }

    #[test]
    fn overflow_keeps_the_first_decals_and_counts_the_rest() {
        let (clip_from_world, world_from_clip) = camera();
        let decals = vec![at(-5.0); MAX_DECALS + 7];
        let plan = plan(&decals, clip_from_world, world_from_clip, [64, 64]).unwrap();
        assert_eq!(plan.report.submitted, MAX_DECALS + 7);
        assert_eq!(plan.report.drawn, MAX_DECALS);
        assert_eq!(plan.report.dropped_overflow, 7);
        assert_eq!(plan.items.last().unwrap().index, MAX_DECALS - 1);
    }

    #[test]
    fn plan_rejects_invalid_and_oversized_lists() {
        let (clip_from_world, world_from_clip) = camera();
        let mut bad = at(-5.0);
        bad.opacity = 2.0;
        let error = plan(&[at(-5.0), bad], clip_from_world, world_from_clip, [8, 8]).unwrap_err();
        assert!(error.to_string().contains("decal 1"), "{error}");
        let huge = vec![at(-5.0); MAX_SUBMITTED_DECALS + 1];
        assert!(plan(&huge, clip_from_world, world_from_clip, [8, 8]).is_err());
    }

    #[test]
    fn fully_transparent_decals_are_skipped_but_emissive_ones_are_not() {
        let (clip_from_world, world_from_clip) = camera();
        let mut clear = at(-5.0);
        clear.opacity = 0.0;
        let mut glow = clear.clone();
        glow.emissive = Vec3::ONE;
        let plan = plan(&[clear, glow], clip_from_world, world_from_clip, [8, 8]).unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].index, 1);
        assert_eq!(plan.report.culled, 1);
    }

    #[test]
    fn uniform_encodes_the_fade_cosines() {
        let (_, world_from_clip) = camera();
        let decal = Decal {
            max_angle: 60f32.to_radians(),
            angle_fade: 20f32.to_radians(),
            edge_fade: 0.25,
            ..Default::default()
        };
        let uniform = DecalUniform::new(&decal, world_from_clip, [200, 100]);
        assert!((uniform.params[0] - 0.25).abs() < 1e-6);
        assert!((uniform.params[1] - 0.5).abs() < 1e-5);
        assert!((uniform.params[2] - 40f32.to_radians().cos()).abs() < 1e-5);
        assert_eq!(uniform.size, [200.0, 100.0, 0.005, 0.01]);
        assert_eq!(std::mem::size_of::<DecalUniform>(), 208);
    }
}
