//! 2D to 3D proxy synthesizer (section 5.2 b).
//!
//! Footprints are convex polygons in game (world) units. With `s = 1 /
//! units_per_meter` the game point `(x, y)` maps to acoustic metres as:
//!
//! - `SideView`: `(x s, y s, z)`; footprints are prisms over `z` in
//!   `[-depth/2, +depth/2]`; sources and the listener sit at `z = 0`.
//! - `TopDown`: `(x s, h, -y s)`; footprints are prisms over `h` in
//!   `[0, wall_height]`; sources and the listener sit at `h = ear_height`, and game
//!   `+y` (screen up) is acoustic forward `-Z`.
//!
//! Circles become 12-gons and capsules two 6-segment semicircles joined into one
//! convex 14-gon (half height along local `y`). Polygons must be convex (either
//! winding) with at most `WorldLimits::max_footprint_vertices` points.
//!
//! The enclosure rectangle (`Auto`: footprint bounds plus `margin_m`; `Bounds`:
//! explicit, in game units) receives bounding faces:
//!
//! - Side view: bottom edge `floor_material` (if `floor`), top edge
//!   `ceiling_material` (if `ceiling && !open_sky`), left and right edges
//!   `material`, and front and back planes at `z = +-depth/2` with `side_material`
//!   (if `enclose_depth`).
//! - Top down: four walls `material` over `[0, wall_height]`, a floor at 0
//!   (`floor_material`, if `floor`) and a ceiling at `wall_height`
//!   (`ceiling_material`, if `ceiling && !open_sky`).
//!
//! With `Enclosure::None` there are no enclosure walls; the front/back planes (side
//! view) or floor/ceiling (top down) then span the footprint bounds, and no air
//! volume is reported. Footprint prisms omit end caps that coincide with those
//! planes. Static footprints share one static instance; each dynamic footprint gets
//! its own dynamic instance (see [`ProxyMapping::footprint_instance`] and
//! [`ProxyMapping::rigid_motion`]).

use crate::world::{AcousticWorldBuilder, InstanceData, invalid, limit, validate_transform};
use crate::{
    AcousticMesh, AcousticsError, Affine3, Enclosure, Footprint, Footprint2D, IDENTITY_AFFINE,
    InstanceId, ListenerPose, MaterialId, Mobility, Proxy2DSettings, ProxyPlane, Result, math,
};

/// Polygon sides used for a circle.
pub const CIRCLE_SEGMENTS: usize = 12;
/// Segments per capsule semicircle.
pub const CAPSULE_ARC_SEGMENTS: usize = 6;

/// Air enclosed by the proxy (the whole level, all air cells together).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct ProxyAir {
    pub volume_m3: f32,
    /// Reflecting boundary area (walls, floor, ceiling, front/back planes).
    pub surface_m2: f32,
    /// Boundary area left open (removed ceiling or floor, missing front/back).
    pub open_m2: f32,
}

/// Mapping from game space to proxy acoustic space, from
/// [`AcousticWorldBuilder::add_proxy_2d`].
#[derive(Clone, Debug, PartialEq)]
pub struct ProxyMapping {
    plane: ProxyPlane,
    scale: f32,
    ear_height_m: f32,
    air: Option<ProxyAir>,
    footprint_instances: Vec<InstanceId>,
    enclosure: Option<[[f32; 2]; 2]>,
}
impl ProxyMapping {
    /// Game position to acoustic position (side view `z = 0`, top down
    /// `h = ear_height`); the game `z` is ignored.
    pub fn to_acoustic(&self, world: [f32; 3]) -> [f32; 3] {
        let (x, y) = (world[0] * self.scale, world[1] * self.scale);
        match self.plane {
            ProxyPlane::SideView => [x, y, 0.0],
            ProxyPlane::TopDown => [x, self.ear_height_m, -y],
        }
    }
    /// Game listener pose to acoustic space. Side view keeps the game axes (the
    /// default listener faces `-Z` into the screen with `+Y` up). Top down faces the
    /// screen direction of the game `up` vector (`+y` maps to `-Z`) with acoustic
    /// `+Y` (the sky) up.
    pub fn pose_to_acoustic(&self, pose: ListenerPose) -> ListenerPose {
        let position = self.to_acoustic(pose.position);
        match self.plane {
            ProxyPlane::SideView => ListenerPose {
                position,
                forward: pose.forward,
                up: pose.up,
            },
            ProxyPlane::TopDown => ListenerPose {
                position,
                forward: math::normalize([pose.up[0], 0.0, -pose.up[1]])
                    .unwrap_or([0.0, 0.0, -1.0]),
                up: [0.0, 1.0, 0.0],
            },
        }
    }
    /// Acoustic metres per game unit (`1 / units_per_meter`).
    pub fn metres_per_unit(&self) -> f32 {
        self.scale
    }
    pub fn plane(&self) -> ProxyPlane {
        self.plane
    }
    /// Enclosed air, when there is an enclosure.
    pub fn air(&self) -> Option<ProxyAir> {
        self.air
    }
    /// Enclosure rectangle in acoustic metres (`[min, max]` of `(x s, y s)`).
    pub fn enclosure_m(&self) -> Option<[[f32; 2]; 2]> {
        self.enclosure
    }
    /// Instance holding footprint `index` (static footprints share one static
    /// instance; dynamic footprints have their own).
    pub fn footprint_instance(&self, index: usize) -> Option<InstanceId> {
        self.footprint_instances.get(index).copied()
    }
    pub fn footprint_instances(&self) -> &[InstanceId] {
        &self.footprint_instances
    }
    /// Acoustic transform of a dynamic footprint that moved rigidly since the build:
    /// rotated by `rotation` radians (counter-clockwise in game space) about the game
    /// point `pivot`, then translated by `translation` game units. Pass it to
    /// `AcousticWorld::set_instance_transform` for [`Self::footprint_instance`].
    pub fn rigid_motion(&self, translation: [f32; 2], rotation: f32, pivot: [f32; 2]) -> Affine3 {
        let (sn, c) = rotation.sin_cos();
        let s = self.scale;
        match self.plane {
            ProxyPlane::SideView => {
                let (px, py) = (pivot[0] * s, pivot[1] * s);
                [
                    c,
                    -sn,
                    0.0,
                    px + translation[0] * s - (c * px - sn * py),
                    sn,
                    c,
                    0.0,
                    py + translation[1] * s - (sn * px + c * py),
                    0.0,
                    0.0,
                    1.0,
                    0.0,
                ]
            }
            ProxyPlane::TopDown => {
                // Acoustic X = x s and Z = -y s, so a game rotation by `rotation`
                // is a rotation about +Y by the same angle.
                let (px, pz) = (pivot[0] * s, -pivot[1] * s);
                let (tx, tz) = (translation[0] * s, -translation[1] * s);
                [
                    c,
                    0.0,
                    sn,
                    px + tx - (c * px + sn * pz),
                    0.0,
                    1.0,
                    0.0,
                    0.0,
                    -sn,
                    0.0,
                    c,
                    pz + tz - (-sn * px + c * pz),
                ]
            }
        }
    }
}

type P2 = [f64; 2];

fn proxy_err(message: impl Into<String>) -> AcousticsError {
    AcousticsError::new("acoustics.proxy.invalid", message)
}
fn cross2(a: P2, b: P2) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn sub2(a: P2, b: P2) -> P2 {
    [a[0] - b[0], a[1] - b[1]]
}
fn signed_area(p: &[P2]) -> f64 {
    (0..p.len())
        .map(|i| cross2(p[i], p[(i + 1) % p.len()]))
        .sum::<f64>()
        * 0.5
}

/// Footprint outline in game units, counter-clockwise.
fn outline(shape: &Footprint2D, max_vertices: u32) -> Result<Vec<P2>> {
    let finite = |v: &[f32]| v.iter().all(|x| x.is_finite());
    let place = |pts: Vec<P2>, center: [f32; 2], rotation: f32| -> Vec<P2> {
        let (s, c) = f64::from(rotation).sin_cos();
        pts.into_iter()
            .map(|[x, y]| {
                [
                    f64::from(center[0]) + c * x - s * y,
                    f64::from(center[1]) + s * x + c * y,
                ]
            })
            .collect()
    };
    let mut pts = match *shape {
        Footprint2D::Rect {
            center,
            half_extents,
            rotation,
        } => {
            if !finite(&[
                center[0],
                center[1],
                half_extents[0],
                half_extents[1],
                rotation,
            ]) || half_extents[0] <= 0.0
                || half_extents[1] <= 0.0
            {
                return Err(proxy_err(
                    "rect needs finite values and positive half extents",
                ));
            }
            let (hx, hy) = (f64::from(half_extents[0]), f64::from(half_extents[1]));
            place(
                vec![[-hx, -hy], [hx, -hy], [hx, hy], [-hx, hy]],
                center,
                rotation,
            )
        }
        Footprint2D::Circle { center, radius } => {
            if !finite(&[center[0], center[1], radius]) || radius <= 0.0 {
                return Err(proxy_err(
                    "circle needs a finite centre and positive radius",
                ));
            }
            let r = f64::from(radius);
            let pts = (0..CIRCLE_SEGMENTS)
                .map(|i| {
                    let a = std::f64::consts::TAU * i as f64 / CIRCLE_SEGMENTS as f64;
                    [r * a.cos(), r * a.sin()]
                })
                .collect();
            place(pts, center, 0.0)
        }
        Footprint2D::Capsule {
            center,
            half_height,
            radius,
            rotation,
        } => {
            if !finite(&[center[0], center[1], half_height, radius, rotation])
                || radius <= 0.0
                || half_height < 0.0
            {
                return Err(proxy_err(
                    "capsule needs finite values, a positive radius and half_height >= 0",
                ));
            }
            let (r, h) = (f64::from(radius), f64::from(half_height));
            let mut pts = Vec::with_capacity(2 * CAPSULE_ARC_SEGMENTS + 2);
            for (cy, start) in [(h, 0.0), (-h, std::f64::consts::PI)] {
                for i in 0..=CAPSULE_ARC_SEGMENTS {
                    if h == 0.0 && i == CAPSULE_ARC_SEGMENTS {
                        continue; // Coincides with the other arc's first point.
                    }
                    let a = start + std::f64::consts::PI * i as f64 / CAPSULE_ARC_SEGMENTS as f64;
                    pts.push([r * a.cos(), cy + r * a.sin()]);
                }
            }
            place(pts, center, rotation)
        }
        Footprint2D::Polygon { ref points } => {
            if points.len() < 3 || points.len() > max_vertices as usize {
                return Err(proxy_err(format!("polygon needs 3-{max_vertices} points")));
            }
            if !points.iter().all(|p| finite(p)) {
                return Err(proxy_err("polygon points must be finite"));
            }
            points.iter().map(|p| p.map(f64::from)).collect()
        }
    };
    let scale = pts
        .iter()
        .fold(0.0f64, |m, p| m.max(p[0].abs()).max(p[1].abs()))
        .max(1e-30);
    let area = signed_area(&pts);
    if area.abs() <= 1e-12 * scale * scale {
        return Err(proxy_err("footprint has no area"));
    }
    if area < 0.0 {
        pts.reverse();
    }
    let n = pts.len();
    for i in 0..n {
        let turn = cross2(
            sub2(pts[(i + 1) % n], pts[i]),
            sub2(pts[(i + 2) % n], pts[(i + 1) % n]),
        );
        if turn < -1e-9 * scale * scale {
            return Err(proxy_err("polygon footprints must be convex"));
        }
    }
    Ok(pts)
}

/// Sutherland-Hodgman clip of a CCW convex polygon to an axis-aligned rectangle.
fn clip_to_rect(poly: &[P2], min: P2, max: P2) -> Vec<P2> {
    let mut out = poly.to_vec();
    for (axis, bound, keep_below) in [
        (0, min[0], false),
        (0, max[0], true),
        (1, min[1], false),
        (1, max[1], true),
    ] {
        let inside = |p: &P2| {
            if keep_below {
                p[axis] <= bound
            } else {
                p[axis] >= bound
            }
        };
        let input = std::mem::take(&mut out);
        for i in 0..input.len() {
            let (a, b) = (input[i], input[(i + 1) % input.len()]);
            let (ia, ib) = (inside(&a), inside(&b));
            if ia {
                out.push(a);
            }
            if ia != ib {
                let t = (bound - a[axis]) / (b[axis] - a[axis]);
                out.push([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
            }
        }
        if out.is_empty() {
            break;
        }
    }
    out
}

/// Area and perimeter of the union of CCW convex polygons, plus the union boundary
/// length on each rectangle edge (bottom, right, top, left). Coincident
/// same-direction edges count once (for the lower index) and facing edges are
/// interior, so abutting and overlapping footprints are handled exactly.
fn union_boundary(polys: &[Vec<P2>], (min, max): (P2, P2)) -> (f64, f64, [f64; 4]) {
    let scale = polys
        .iter()
        .flatten()
        .fold(1.0f64, |m, p| m.max(p[0].abs()).max(p[1].abs()));
    let eps = 1e-9 * scale;
    let mut area2 = 0.0;
    let mut perimeter = 0.0;
    let mut on_rect = [0.0f64; 4];
    for (i, poly) in polys.iter().enumerate() {
        for k in 0..poly.len() {
            let (a, b) = (poly[k], poly[(k + 1) % poly.len()]);
            let e = sub2(b, a);
            let len = (e[0] * e[0] + e[1] * e[1]).sqrt();
            if len <= eps {
                continue;
            }
            let mut covered: Vec<(f64, f64)> = Vec::new();
            for (j, other) in polys.iter().enumerate() {
                if j == i {
                    continue;
                }
                let (mut t0, mut t1) = (0.0f64, 1.0f64);
                let mut empty = false;
                for m in 0..other.len() {
                    let (q0, q1) = (other[m], other[(m + 1) % other.len()]);
                    let d = sub2(q1, q0);
                    let dl = (d[0] * d[0] + d[1] * d[1]).sqrt();
                    if dl <= eps {
                        continue;
                    }
                    // Outward unit normal of a CCW edge.
                    let n = [d[1] / dl, -d[0] / dl];
                    let fa = n[0] * (a[0] - q0[0]) + n[1] * (a[1] - q0[1]);
                    let fe = n[0] * e[0] + n[1] * e[1];
                    if fe.abs() <= 1e-9 * len {
                        if fa.abs() <= 10.0 * eps {
                            // On this edge's line: a same-direction edge is a shared
                            // outer boundary kept by the lower index; an opposite one
                            // is interior.
                            let same = e[0] * d[0] + e[1] * d[1] > 0.0;
                            if same && i < j {
                                empty = true;
                                break;
                            }
                        } else if fa > 0.0 {
                            empty = true;
                            break;
                        }
                        continue;
                    }
                    let t = -fa / fe;
                    if fe > 0.0 {
                        t1 = t1.min(t);
                    } else {
                        t0 = t0.max(t);
                    }
                }
                if !empty && t1 > t0 {
                    covered.push((t0, t1));
                }
            }
            covered.sort_by(|x, y| x.0.total_cmp(&y.0));
            let mut pieces: Vec<(f64, f64)> = Vec::new();
            let mut cursor = 0.0f64;
            for (s0, s1) in covered {
                if s0 > cursor {
                    pieces.push((cursor, s0));
                }
                cursor = cursor.max(s1);
            }
            if cursor < 1.0 {
                pieces.push((cursor, 1.0));
            }
            for (s0, s1) in pieces {
                let p0 = [a[0] + s0 * e[0], a[1] + s0 * e[1]];
                let p1 = [a[0] + s1 * e[0], a[1] + s1 * e[1]];
                let l = (s1 - s0) * len;
                if l <= eps {
                    continue;
                }
                area2 += cross2(p0, p1);
                perimeter += l;
                let on = |v: f64, at: f64| (v - at).abs() <= 10.0 * eps;
                if on(p0[1], min[1]) && on(p1[1], min[1]) {
                    on_rect[0] += l;
                } else if on(p0[0], max[0]) && on(p1[0], max[0]) {
                    on_rect[1] += l;
                } else if on(p0[1], max[1]) && on(p1[1], max[1]) {
                    on_rect[2] += l;
                } else if on(p0[0], min[0]) && on(p1[0], min[0]) {
                    on_rect[3] += l;
                }
            }
        }
    }
    (area2 * 0.5, perimeter, on_rect)
}

struct MeshParts {
    positions: Vec<[f32; 3]>,
    triangles: Vec<[u32; 3]>,
    materials: Vec<MaterialId>,
}
impl MeshParts {
    fn new() -> Self {
        Self {
            positions: Vec::new(),
            triangles: Vec::new(),
            materials: Vec::new(),
        }
    }
    fn quad(&mut self, q: [[f32; 3]; 4], m: MaterialId) {
        let base = self.positions.len() as u32;
        self.positions.extend(q);
        self.triangles.push([base, base + 1, base + 2]);
        self.triangles.push([base, base + 2, base + 3]);
        self.materials.extend([m, m]);
    }
    fn fan(&mut self, ring: &[[f32; 3]], m: MaterialId) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(ring);
        for i in 1..ring.len() as u32 - 1 {
            self.triangles.push([base, base + i, base + i + 1]);
            self.materials.push(m);
        }
    }
    fn into_mesh(self) -> AcousticMesh {
        AcousticMesh {
            positions: self.positions,
            triangles: self.triangles,
            materials: self.materials,
        }
    }
}

struct Extrusion {
    plane: ProxyPlane,
    w0: f32,
    w1: f32,
    cap_low: bool,
    cap_high: bool,
}
impl Extrusion {
    /// A 2D acoustic point at extrusion coordinate `w`.
    fn lift(&self, p: P2, w: f32) -> [f32; 3] {
        let (x, y) = (p[0] as f32, p[1] as f32);
        match self.plane {
            ProxyPlane::SideView => [x, y, w],
            ProxyPlane::TopDown => [x, w, -y],
        }
    }
    fn wall(&self, parts: &mut MeshParts, a: P2, b: P2, m: MaterialId) {
        parts.quad(
            [
                self.lift(a, self.w0),
                self.lift(b, self.w0),
                self.lift(b, self.w1),
                self.lift(a, self.w1),
            ],
            m,
        );
    }
    fn prism(&self, parts: &mut MeshParts, pts: &[P2], m: MaterialId) {
        for k in 0..pts.len() {
            self.wall(parts, pts[k], pts[(k + 1) % pts.len()], m);
        }
        for (cap, w) in [(self.cap_low, self.w0), (self.cap_high, self.w1)] {
            if cap {
                let ring: Vec<[f32; 3]> = pts.iter().map(|&p| self.lift(p, w)).collect();
                parts.fan(&ring, m);
            }
        }
    }
    fn rect(&self, (min, max): (P2, P2), w: f32) -> [[f32; 3]; 4] {
        [min, [max[0], min[1]], max, [min[0], max[1]]].map(|p| self.lift(p, w))
    }
}

impl AcousticWorldBuilder {
    /// Synthesizes the proxy world from 2D footprints. A world holds at most one
    /// proxy. Every footprint and setting is validated before anything is added.
    pub fn add_proxy_2d(
        &mut self,
        footprints: &[Footprint],
        settings: &Proxy2DSettings,
    ) -> Result<ProxyMapping> {
        let limits = self.limits();
        let st = settings;
        if self.proxy.is_some() {
            return Err(proxy_err("a world holds at most one 2D proxy"));
        }
        if footprints.len() > limits.max_proxy_footprints as usize {
            return Err(limit(format!(
                "at most {} proxy footprints",
                limits.max_proxy_footprints
            )));
        }
        let positive = |v: f32| v.is_finite() && v > 0.0;
        if !(st.units_per_meter.is_finite() && (0.001..=1000.0).contains(&st.units_per_meter)) {
            return Err(proxy_err("units_per_meter must be 0.001-1000"));
        }
        if !positive(st.depth_m) || !positive(st.wall_height_m) {
            return Err(proxy_err(
                "depth_m and wall_height_m must be finite and positive",
            ));
        }
        if !(st.ear_height_m.is_finite() && st.ear_height_m >= 0.0) {
            return Err(proxy_err("ear_height_m must be finite and >= 0"));
        }
        if st.plane == ProxyPlane::TopDown && st.ear_height_m > st.wall_height_m {
            return Err(proxy_err("ear_height_m must not exceed wall_height_m"));
        }
        for m in [
            st.material,
            st.floor_material,
            st.ceiling_material,
            st.side_material,
        ] {
            self.check_material(m)?;
        }
        let s = 1.0 / f64::from(st.units_per_meter);
        let mut outlines = Vec::with_capacity(footprints.len());
        for (i, f) in footprints.iter().enumerate() {
            self.check_material(f.material)?;
            let pts = outline(&f.shape, limits.max_footprint_vertices)
                .map_err(|e| proxy_err(format!("footprint {i}: {}", e.message)))?;
            if pts.len() > limits.max_footprint_vertices as usize {
                return Err(proxy_err(format!(
                    "footprint {i} has more than {} vertices",
                    limits.max_footprint_vertices
                )));
            }
            outlines.push(
                pts.into_iter()
                    .map(|p| [p[0] * s, p[1] * s])
                    .collect::<Vec<P2>>(),
            );
        }
        let mut bounds = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in outlines.iter().flatten() {
            for k in 0..2 {
                bounds.0[k] = bounds.0[k].min(p[k]);
                bounds.1[k] = bounds.1[k].max(p[k]);
            }
        }
        let enclosure: Option<(P2, P2)> = match st.enclosure {
            Enclosure::None => None,
            Enclosure::Auto { margin_m } => {
                if !(margin_m.is_finite() && margin_m >= 0.0) {
                    return Err(proxy_err("margin_m must be finite and >= 0"));
                }
                if outlines.is_empty() {
                    return Err(proxy_err("an automatic enclosure needs footprints"));
                }
                let m = f64::from(margin_m);
                Some((
                    [bounds.0[0] - m, bounds.0[1] - m],
                    [bounds.1[0] + m, bounds.1[1] + m],
                ))
            }
            Enclosure::Bounds { min, max } => {
                if !(min.iter().chain(&max).all(|v| v.is_finite())
                    && min[0] < max[0]
                    && min[1] < max[1])
                {
                    return Err(proxy_err("enclosure bounds need finite min < max"));
                }
                Some((
                    [f64::from(min[0]) * s, f64::from(min[1]) * s],
                    [f64::from(max[0]) * s, f64::from(max[1]) * s],
                ))
            }
        };
        // Planes spanning the level: the enclosure, else the footprint bounds.
        let span = enclosure.or((!outlines.is_empty()).then_some(bounds));
        let max_c = f64::from(limits.max_coordinate_m);
        let outside = |p: &P2| p[0].abs() > max_c || p[1].abs() > max_c;
        if outlines.iter().flatten().any(outside)
            || span.is_some_and(|r| outside(&r.0) || outside(&r.1))
            || f64::from(st.depth_m.max(st.wall_height_m)) > max_c
        {
            return Err(limit(format!(
                "proxy exceeds the {max_c} m coordinate limit"
            )));
        }

        let top_open = !st.ceiling || st.open_sky;
        let ex = match st.plane {
            ProxyPlane::SideView => Extrusion {
                plane: st.plane,
                w0: -0.5 * st.depth_m,
                w1: 0.5 * st.depth_m,
                cap_low: !st.enclose_depth,
                cap_high: !st.enclose_depth,
            },
            ProxyPlane::TopDown => Extrusion {
                plane: st.plane,
                w0: 0.0,
                w1: st.wall_height_m,
                cap_low: !st.floor,
                cap_high: top_open,
            },
        };
        let mut statics = MeshParts::new();
        let mut dynamics: Vec<(usize, AcousticMesh)> = Vec::new();
        for (i, (f, pts)) in footprints.iter().zip(&outlines).enumerate() {
            match f.mobility {
                Mobility::Static => ex.prism(&mut statics, pts, f.material),
                Mobility::Dynamic => {
                    let mut parts = MeshParts::new();
                    ex.prism(&mut parts, pts, f.material);
                    dynamics.push((i, parts.into_mesh()));
                }
            }
        }
        if let Some((min, max)) = enclosure {
            let corners = [min, [max[0], min[1]], max, [min[0], max[1]]];
            let side_view = st.plane == ProxyPlane::SideView;
            for (edge, m, present) in [
                (0usize, st.floor_material, !side_view || st.floor),
                (1, st.material, true),
                (2, st.ceiling_material, !side_view || !top_open),
                (3, st.material, true),
            ] {
                if present {
                    let m = if side_view { m } else { st.material };
                    ex.wall(&mut statics, corners[edge], corners[(edge + 1) % 4], m);
                }
            }
        }
        if let Some(rect) = span {
            match st.plane {
                ProxyPlane::SideView => {
                    if st.enclose_depth {
                        statics.quad(ex.rect(rect, ex.w0), st.side_material);
                        statics.quad(ex.rect(rect, ex.w1), st.side_material);
                    }
                }
                ProxyPlane::TopDown => {
                    if st.floor {
                        statics.quad(ex.rect(rect, ex.w0), st.floor_material);
                    }
                    if !top_open {
                        statics.quad(ex.rect(rect, ex.w1), st.ceiling_material);
                    }
                }
            }
        }

        // Validate every mesh and the instance room before adding anything.
        let static_mesh = if statics.triangles.is_empty() {
            None
        } else {
            Some(self.validate_mesh(statics.into_mesh())?)
        };
        let mut dynamic_meshes = Vec::with_capacity(dynamics.len());
        for (i, mesh) in dynamics {
            dynamic_meshes.push((i, self.validate_mesh(mesh)?));
        }
        let static_count = u64::from(static_mesh.is_some());
        if let Some((m, _)) = &static_mesh {
            self.check_instance_room(0, m.triangles.len() as u64, Mobility::Static)?;
        }
        let dynamic_tris: u64 = dynamic_meshes
            .iter()
            .map(|(_, (m, _))| m.triangles.len() as u64)
            .sum();
        self.check_instance_room(dynamic_meshes.len() as u64, dynamic_tris, Mobility::Dynamic)?;
        if self.instances.len() as u64 + static_count + dynamic_meshes.len() as u64
            > u64::from(limits.max_instances)
        {
            return Err(limit(format!(
                "a world holds at most {} instances",
                limits.max_instances
            )));
        }
        let max_coordinate = limits.max_coordinate_m as f32;
        for (m, _) in static_mesh
            .iter()
            .chain(dynamic_meshes.iter().map(|(_, d)| d))
        {
            validate_transform(&IDENTITY_AFFINE, &m.bounds, max_coordinate)?;
        }

        let air = enclosure.map(|rect| proxy_air(&outlines, rect, st));
        let mut footprint_instances = vec![InstanceId(u32::MAX); footprints.len()];
        if let Some((data, dropped)) = static_mesh {
            let mesh = self.push_mesh(data, dropped);
            let id = self.push_instance(identity_instance(mesh.0, Mobility::Static));
            for (slot, f) in footprint_instances.iter_mut().zip(footprints) {
                if f.mobility == Mobility::Static {
                    *slot = id;
                }
            }
        }
        for (i, (data, dropped)) in dynamic_meshes {
            let mesh = self.push_mesh(data, dropped);
            footprint_instances[i] =
                self.push_instance(identity_instance(mesh.0, Mobility::Dynamic));
        }
        if footprint_instances.contains(&InstanceId(u32::MAX)) {
            return Err(invalid("a proxy footprint produced no geometry"));
        }
        let mapping = ProxyMapping {
            plane: st.plane,
            scale: s as f32,
            ear_height_m: st.ear_height_m,
            air,
            footprint_instances,
            enclosure: enclosure.map(|(a, b)| [a.map(|v| v as f32), b.map(|v| v as f32)]),
        };
        self.proxy = Some(mapping.clone());
        Ok(mapping)
    }
}

fn identity_instance(mesh: u32, mobility: Mobility) -> InstanceData {
    InstanceData {
        mesh,
        transform: IDENTITY_AFFINE,
        inverse: IDENTITY_AFFINE,
        mobility,
        mask: u8::MAX,
    }
}

/// Exact air volume and boundary areas: the enclosure rectangle minus the union of
/// the footprints (clipped to it), extruded over the depth or wall height.
fn proxy_air(outlines: &[Vec<P2>], (min, max): (P2, P2), st: &Proxy2DSettings) -> ProxyAir {
    let clipped: Vec<Vec<P2>> = outlines
        .iter()
        .map(|p| clip_to_rect(p, min, max))
        .filter(|p| p.len() >= 3 && signed_area(p) > 0.0)
        .collect();
    let (union_area, union_perimeter, on_rect) = union_boundary(&clipped, (min, max));
    let edge = [
        max[0] - min[0],
        max[1] - min[1],
        max[0] - min[0],
        max[1] - min[1],
    ];
    let air_area = (edge[0] * edge[1] - union_area).max(0.0);
    let uncovered: Vec<f64> = (0..4).map(|k| (edge[k] - on_rect[k]).max(0.0)).collect();
    let air_perimeter =
        edge.iter().sum::<f64>() + union_perimeter - 2.0 * on_rect.iter().sum::<f64>();
    let top_open = !st.ceiling || st.open_sky;
    let (volume, surface, open) = match st.plane {
        ProxyPlane::SideView => {
            let d = f64::from(st.depth_m);
            let mut open_len = 0.0;
            if !st.floor {
                open_len += uncovered[0];
            }
            if top_open {
                open_len += uncovered[2];
            }
            let mut surface = (air_perimeter - open_len) * d;
            let mut open = open_len * d;
            if st.enclose_depth {
                surface += 2.0 * air_area;
            } else {
                open += 2.0 * air_area;
            }
            (air_area * d, surface, open)
        }
        ProxyPlane::TopDown => {
            let h = f64::from(st.wall_height_m);
            let mut surface = air_perimeter * h;
            let mut open = 0.0;
            for present in [st.floor, !top_open] {
                if present {
                    surface += air_area;
                } else {
                    open += air_area;
                }
            }
            (air_area * h, surface, open)
        }
    };
    ProxyAir {
        volume_m3: volume as f32,
        surface_m2: surface as f32,
        open_m2: open as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<P2> {
        vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
    }

    #[test]
    fn union_of_abutting_and_overlapping_boxes_is_exact() {
        let polys = vec![
            sq(0.0, 0.0, 1.0, 1.0),
            sq(1.0, 0.0, 2.0, 1.0),
            sq(1.5, 0.0, 2.5, 1.0),
        ];
        let (area, perimeter, on) = union_boundary(&polys, ([0.0, 0.0], [3.0, 1.0]));
        assert!((area - 2.5).abs() < 1e-9, "area {area}");
        assert!((perimeter - 7.0).abs() < 1e-9, "perimeter {perimeter}");
        assert!((on[0] - 2.5).abs() < 1e-9 && (on[3] - 1.0).abs() < 1e-9);
        let twice = [sq(0.0, 0.0, 1.0, 1.0), sq(0.0, 0.0, 1.0, 1.0)];
        let (area, perimeter, _) = union_boundary(&twice, ([0.0, 0.0], [1.0, 1.0]));
        assert!((area - 1.0).abs() < 1e-9 && (perimeter - 4.0).abs() < 1e-9);
    }
}
