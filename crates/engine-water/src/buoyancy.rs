//! Buoyancy and water drag on a rigid body sampled at points.
//!
//! Each sample point stands for `volume / N` of the body's displaced volume, spread over a
//! vertical extent `height` centred on the point. Its submerged fraction is
//! `f = clamp((surface - y) / height + 1/2, 0, 1)` (linear in the water level, so slab-shaped
//! points that partition a box give its exact submerged volume in calm water). Per tick of
//! length `dt`:
//!
//! * **Buoyancy**: `J_i = -rho V_i f_i g dt` at the point (Archimedes), giving a linear impulse
//!   and the angular impulse `(p_i - c) x J_i` about the centre of mass `c`.
//! * **Linear drag** toward the water velocity `u` (submerged-weighted mean over the points):
//!   `J = m a_l (u - v)` with `a_l = linear_drag f_mean dt`; rivers add a horizontal
//!   `J = m a_f (u - v)_h` with `a_f = flow_drag f_mean dt`. When `a_l + a_f > 1` both are
//!   scaled so the sum is 1: drag never overshoots the water velocity (unconditionally
//!   stable for any `dt`).
//! * **Angular drag**: each point resists the rotational part of its velocity,
//!   `J_i = -(m / N) f_i a_w (w x r_i)` with `a_w = min(angular_drag dt, 1/4)`, applied as the
//!   angular impulse `r_i x J_i`.
use glam::Vec3;

/// One buoyancy sample point in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuoyancyPoint {
    pub position: Vec3,
    /// Displaced volume this point stands for when fully submerged (m^3).
    pub volume: f32,
    /// Vertical extent of the point's slab (m).
    pub height: f32,
}

/// The water at a world point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterAt {
    /// World height of the surface above the point.
    pub surface: f32,
    /// Water velocity (flow) at the point.
    pub velocity: Vec3,
    /// Fluid density in the units of the body densities (water defaults to 1).
    pub density: f32,
}

/// Rigid-body state the forces depend on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyMotion {
    pub center: Vec3,
    pub linear_velocity: Vec3,
    pub angular_velocity: Vec3,
    pub mass: f32,
}

/// Drag coefficients (per second) and gravity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragParams {
    pub linear_drag: f32,
    pub angular_drag: f32,
    pub flow_drag: f32,
    /// Gravity vector (m/s^2); buoyancy points against it.
    pub gravity: Vec3,
}

/// Impulses for one tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BuoyancyImpulse {
    /// Linear impulse at the centre of mass (N s).
    pub linear: Vec3,
    /// Angular impulse (N m s).
    pub angular: Vec3,
    /// Submerged volume (m^3) and mean submerged fraction of the points.
    pub submerged_volume: f32,
    pub submerged_fraction: f32,
    /// Buoyant force magnitude (N).
    pub buoyant_force: f32,
}

/// Submerged fraction of a slab of `height` centred `depth` metres below the surface.
pub fn submerged_fraction(depth: f32, height: f32) -> f32 {
    if height <= 0.0 {
        return f32::from(u8::from(depth > 0.0));
    }
    (depth / height + 0.5).clamp(0.0, 1.0)
}

/// Buoyancy and drag impulses over `dt` seconds (see the module docs). `water` returns the
/// water at a world point, or `None` where there is none.
pub fn buoyancy_impulse(
    points: &[BuoyancyPoint],
    body: &BodyMotion,
    params: &DragParams,
    dt: f32,
    water: impl Fn(Vec3) -> Option<WaterAt>,
) -> BuoyancyImpulse {
    let mut out = BuoyancyImpulse::default();
    if points.is_empty() || dt <= 0.0 {
        return out;
    }
    let count = points.len() as f32;
    let mut flow = Vec3::ZERO;
    let mut fraction_sum = 0.0;
    let up_g = params.gravity;
    let angular_rate = (params.angular_drag * dt).min(0.25);
    for point in points {
        let Some(at) = water(point.position) else {
            continue;
        };
        let f = submerged_fraction(at.surface - point.position.y, point.height);
        if f <= 0.0 {
            continue;
        }
        let r = point.position - body.center;
        let lift = -up_g * (at.density * point.volume * f * dt);
        out.linear += lift;
        out.angular += r.cross(lift);
        out.submerged_volume += point.volume * f;
        out.buoyant_force += lift.length() / dt;
        let spin = -(body.mass / count) * f * angular_rate * body.angular_velocity.cross(r);
        out.angular += r.cross(spin);
        flow += at.velocity * f;
        fraction_sum += f;
    }
    if fraction_sum <= 0.0 {
        return out;
    }
    let mean = fraction_sum / count;
    out.submerged_fraction = mean;
    let u = flow / fraction_sum;
    let mut linear = params.linear_drag * mean * dt;
    let mut along = params.flow_drag * mean * dt;
    let total = linear + along;
    if total > 1.0 {
        linear /= total;
        along /= total;
    }
    let relative = u - body.linear_velocity;
    out.linear += body.mass * linear * relative;
    out.linear += body.mass * along * Vec3::new(relative.x, 0.0, relative.z);
    out
}

/// Sample points of a box with `half_extents` (local space): a `cells` lattice of cell centres
/// (each cell `2 half / cells`), every point standing for an equal share of the box volume
/// and a slab as tall as one cell. Returns `(local points, point height)`.
pub fn box_points(half_extents: Vec3, cells: [u32; 3]) -> (Vec<Vec3>, f32) {
    let cells = cells.map(|c| c.max(1));
    let size = 2.0 * half_extents / Vec3::new(cells[0] as f32, cells[1] as f32, cells[2] as f32);
    let mut points = Vec::new();
    for y in 0..cells[1] {
        for z in 0..cells[2] {
            for x in 0..cells[0] {
                points.push(-half_extents + size * (Vec3::new(x as f32, y as f32, z as f32) + 0.5));
            }
        }
    }
    (points, size.y)
}
