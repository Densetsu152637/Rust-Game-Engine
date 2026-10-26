//! Per-eye view math. OpenXR and the engine share conventions: right-handed,
//! metres, +Y up, the camera looking down -Z. Projections match the desktop
//! camera (`glam::camera::rh::proj::directx`): clip depth 0 at `near`, 1 at
//! `far` (not reversed), clip-space Y up.
use glam::{Mat4, Quat, Vec3, Vec4};

/// Field of view as four signed half-angles in radians (OpenXR `XrFovf`):
/// `left` and `down` are normally negative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fov {
    pub angle_left: f32,
    pub angle_right: f32,
    pub angle_up: f32,
    pub angle_down: f32,
}
impl Fov {
    /// A symmetric fov with vertical angle `fov_y` and `aspect` = width/height.
    pub fn symmetric(fov_y: f32, aspect: f32) -> Self {
        let half_y = fov_y * 0.5;
        let half_x = (half_y.tan() * aspect).atan();
        Self {
            angle_left: -half_x,
            angle_right: half_x,
            angle_up: half_y,
            angle_down: -half_y,
        }
    }
}
impl From<openxr::Fovf> for Fov {
    fn from(fov: openxr::Fovf) -> Self {
        Self {
            angle_left: fov.angle_left,
            angle_right: fov.angle_right,
            angle_up: fov.angle_up,
            angle_down: fov.angle_down,
        }
    }
}

/// Right-handed, depth [0,1] projection for an asymmetric fov.
pub fn projection_from_fov(fov: Fov, near: f32, far: f32) -> Result<Mat4, String> {
    let left = fov.angle_left.tan();
    let right = fov.angle_right.tan();
    let up = fov.angle_up.tan();
    let down = fov.angle_down.tan();
    if ![left, right, up, down, near, far]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err("projection inputs must be finite".into());
    }
    if right <= left || up <= down {
        return Err("fov right/up angles must exceed left/down angles".into());
    }
    if near <= 0.0 || far <= near {
        return Err("projection requires 0 < near < far".into());
    }
    let width = right - left;
    let height = up - down;
    let depth = far / (near - far);
    Ok(Mat4::from_cols(
        Vec4::new(2.0 / width, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 / height, 0.0, 0.0),
        Vec4::new((right + left) / width, (up + down) / height, depth, -1.0),
        Vec4::new(0.0, 0.0, near * depth, 0.0),
    ))
}

/// A rigid transform in the tracking space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub orientation: Quat,
    pub position: Vec3,
}
impl Pose {
    pub const IDENTITY: Self = Self {
        orientation: Quat::IDENTITY,
        position: Vec3::ZERO,
    };
    /// Object-to-tracking-space matrix (the camera's world matrix for a view).
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_rotation_translation(self.orientation, self.position)
    }
    /// Tracking-space-to-eye matrix of a view at this pose.
    pub fn view_matrix(&self) -> Mat4 {
        self.matrix().inverse()
    }
    /// True when the pose differs from identity by more than `epsilon`.
    pub fn is_identity(&self, epsilon: f32) -> bool {
        self.position.length() <= epsilon && self.orientation.abs_diff_eq(Quat::IDENTITY, epsilon)
    }
    /// Convert a runtime pose, normalising the quaternion. A zero or
    /// non-finite quaternion (an untracked pose) becomes identity rotation.
    pub fn from_xr(pose: openxr::Posef) -> Self {
        let q = pose.orientation;
        let orientation = Quat::from_xyzw(q.x, q.y, q.z, q.w);
        let orientation = if orientation.is_finite() && orientation.length_squared() > 1e-12 {
            orientation.normalize()
        } else {
            Quat::IDENTITY
        };
        let p = pose.position;
        let position = Vec3::new(p.x, p.y, p.z);
        Self {
            orientation,
            position: if position.is_finite() {
                position
            } else {
                Vec3::ZERO
            },
        }
    }
    /// Convert to the engine-project pose data (`[x,y,z,w]` quaternion).
    pub fn to_project(&self, tracked: bool) -> engine_project::xr::XrPose {
        engine_project::xr::XrPose {
            position: self.position.to_array(),
            orientation: self.orientation.to_array(),
            tracked,
        }
    }
}

/// Everything a renderer needs for one eye this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XrView {
    pub eye: crate::Eye,
    /// Eye pose in the tracking space.
    pub pose: Pose,
    pub fov: Fov,
    /// View-to-clip projection (depth [0,1], right-handed).
    pub projection: Mat4,
    /// Tracking-space-to-view matrix.
    pub view: Mat4,
    /// Render target size in pixels (recommended size times render scale).
    pub extent: [u32; 2],
}
impl XrView {
    pub fn new(
        eye: crate::Eye,
        pose: Pose,
        fov: Fov,
        near: f32,
        far: f32,
        extent: [u32; 2],
    ) -> Result<Self, String> {
        Ok(Self {
            eye,
            pose,
            fov,
            projection: projection_from_fov(fov, near, far)?,
            view: pose.view_matrix(),
            extent,
        })
    }
    /// Tracking-space-to-clip matrix.
    pub fn view_projection(&self) -> Mat4 {
        self.projection * self.view
    }
}

/// The per-eye render size: the runtime's recommended size scaled by
/// `render_scale`, clamped to [1, max] on each axis.
pub fn scaled_extent(recommended: [u32; 2], max: [u32; 2], render_scale: f32) -> [u32; 2] {
    let scale = if render_scale.is_finite() && render_scale > 0.0 {
        render_scale
    } else {
        1.0
    };
    [0, 1].map(|axis| {
        let scaled = (recommended[axis] as f32 * scale).round() as u32;
        scaled.clamp(1, max[axis].max(1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(m: Mat4, point: Vec3) -> Vec3 {
        let clip = m * point.extend(1.0);
        clip.truncate() / clip.w
    }

    #[test]
    fn symmetric_fov_matches_the_desktop_camera_projection() {
        let (fov_y, aspect, near, far) = (1.2_f32, 1.25_f32, 0.1, 500.0);
        let ours = projection_from_fov(Fov::symmetric(fov_y, aspect), near, far).unwrap();
        let desktop = glam::camera::rh::proj::directx::perspective(fov_y, aspect, near, far);
        assert!(ours.abs_diff_eq(desktop, 1e-5), "{ours:?}\n{desktop:?}");
    }

    #[test]
    fn asymmetric_fov_maps_frustum_edges_to_ndc_bounds() {
        // A typical left-eye fov: wider to the left than to the right.
        let fov = Fov {
            angle_left: -0.9,
            angle_right: 0.7,
            angle_up: 0.8,
            angle_down: -0.95,
        };
        let (near, far) = (0.05, 100.0);
        let m = projection_from_fov(fov, near, far).unwrap();
        let at = |x: f32, y: f32, z: f32| project(m, Vec3::new(x * z, y * z, -z));
        let left_near = at(fov.angle_left.tan(), 0.0, near);
        assert!(
            (left_near.x + 1.0).abs() < 1e-5 && left_near.z.abs() < 1e-5,
            "{left_near}"
        );
        let right_far = at(fov.angle_right.tan(), 0.0, far);
        assert!(
            (right_far.x - 1.0).abs() < 1e-4 && (right_far.z - 1.0).abs() < 1e-5,
            "{right_far}"
        );
        let top = at(0.0, fov.angle_up.tan(), 1.0);
        assert!((top.y - 1.0).abs() < 1e-5, "{top}");
        let bottom = at(0.0, fov.angle_down.tan(), 1.0);
        assert!((bottom.y + 1.0).abs() < 1e-5, "{bottom}");
        // The forward axis lands off-centre exactly by the asymmetry.
        let forward = at(0.0, 0.0, 2.0);
        let expected_x = -(fov.angle_right.tan() + fov.angle_left.tan())
            / (fov.angle_right.tan() - fov.angle_left.tan());
        assert!(
            (forward.x - expected_x).abs() < 1e-5,
            "{forward} vs {expected_x}"
        );
        // Exact matrix entries.
        let (l, r, u, d) = (0.9_f32.tan(), 0.7_f32.tan(), 0.8_f32.tan(), 0.95_f32.tan());
        assert!((m.x_axis.x - 2.0 / (r + l)).abs() < 1e-6);
        assert!((m.y_axis.y - 2.0 / (u + d)).abs() < 1e-6);
        assert!((m.z_axis.x - (r - l) / (r + l)).abs() < 1e-6);
        assert!((m.z_axis.y - (u - d) / (u + d)).abs() < 1e-6);
        assert!((m.z_axis.z - far / (near - far)).abs() < 1e-6);
        assert_eq!(m.z_axis.w, -1.0);
        assert!((m.w_axis.z - near * far / (near - far)).abs() < 1e-6);
    }

    #[test]
    fn invalid_projection_inputs_are_rejected() {
        let fov = Fov::symmetric(1.0, 1.0);
        assert!(projection_from_fov(fov, 0.0, 10.0).is_err());
        assert!(projection_from_fov(fov, 1.0, 1.0).is_err());
        let inverted = Fov {
            angle_left: 0.5,
            angle_right: -0.5,
            ..fov
        };
        assert!(projection_from_fov(inverted, 0.1, 10.0).is_err());
        assert!(
            projection_from_fov(
                Fov {
                    angle_up: f32::NAN,
                    ..fov
                },
                0.1,
                10.0
            )
            .is_err()
        );
    }

    #[test]
    fn view_matrix_inverts_the_eye_pose() {
        let pose = Pose {
            orientation: Quat::from_rotation_y(0.5),
            position: Vec3::new(0.032, 1.6, 0.1),
        };
        let view = XrView::new(
            crate::Eye::Left,
            pose,
            Fov::symmetric(1.5, 0.9),
            0.05,
            100.0,
            [100, 100],
        )
        .unwrap();
        let eye_in_view = view.view.transform_point3(pose.position);
        assert!(eye_in_view.length() < 1e-5);
        let ahead = pose.position + pose.orientation * Vec3::NEG_Z;
        assert!(
            view.view
                .transform_point3(ahead)
                .abs_diff_eq(Vec3::NEG_Z, 1e-5)
        );
        assert!(!pose.is_identity(1e-4));
        assert!(Pose::IDENTITY.is_identity(1e-6));
    }

    #[test]
    fn runtime_poses_are_normalised_and_untracked_rotations_become_identity() {
        let pose = Pose::from_xr(openxr::Posef {
            orientation: openxr::Quaternionf {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 2.0,
            },
            position: openxr::Vector3f {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
        });
        assert_eq!(pose.orientation, Quat::IDENTITY);
        assert_eq!(pose.position, Vec3::new(1.0, 2.0, 3.0));
        let zero = Pose::from_xr(openxr::Posef {
            orientation: openxr::Quaternionf {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 0.0,
            },
            position: openxr::Vector3f {
                x: f32::NAN,
                y: 0.0,
                z: 0.0,
            },
        });
        assert_eq!(zero, Pose::IDENTITY);
        let project = Pose::from_xr(openxr::Posef::IDENTITY).to_project(true);
        assert_eq!(project.orientation, [0.0, 0.0, 0.0, 1.0]);
        assert!(project.tracked);
    }

    #[test]
    fn render_scale_scales_and_clamps_the_recommended_extent() {
        assert_eq!(scaled_extent([2064, 2208], [4128, 4416], 1.0), [2064, 2208]);
        assert_eq!(scaled_extent([2064, 2208], [4128, 4416], 0.5), [1032, 1104]);
        assert_eq!(scaled_extent([2064, 2208], [3000, 3000], 2.0), [3000, 3000]);
        assert_eq!(scaled_extent([1, 1], [10, 10], 0.25), [1, 1]);
        assert_eq!(scaled_extent([100, 100], [200, 200], f32::NAN), [100, 100]);
    }
}
