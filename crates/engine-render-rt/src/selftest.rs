//! The GPU self-test that gates the hardware tier: known rays against a scene
//! of transformed, masked instances, compared with an exact CPU reference.
//!
//! It goes through the public path (`RtScene` BLAS/TLAS builds, the WGSL
//! library and the probe kernel), so it checks transforms, instance masks,
//! TLAS custom data, primitive indexing, `t_min`/`t_max`, barycentrics,
//! facing, vertex-normal fetch and any-hit visibility.

use crate::device::Core;
use crate::error::{RtError, invalid};
use crate::layout::{ProbeHit, ProbeRay};
use crate::limits::{RtIndices, RtInstanceDesc, RtMeshData, RtSceneLimits};
use crate::probe::ProbeKernel;
use crate::scene::RtScene;
use std::sync::Arc;

/// Measured self-test values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelfTestReport {
    pub rays: u32,
    pub expected_hits: u32,
    pub expected_misses: u32,
    /// Hits whose instance, primitive, custom index, material and facing matched.
    pub matched_hits: u32,
    pub max_t_error: f32,
    pub max_barycentric_error: f32,
    /// `1 - dot(expected, actual)` over geometric and shading normals.
    pub max_normal_error: f32,
    pub max_position_error: f32,
    /// Rays whose closest hit is decided by an instance mask.
    pub mask_rays: u32,
    /// Rays that hit a triangle from its back side.
    pub back_face_hits: u32,
}

pub(crate) const T_TOLERANCE: f64 = 1e-4;
pub(crate) const BARYCENTRIC_TOLERANCE: f64 = 1e-4;
pub(crate) const NORMAL_TOLERANCE: f64 = 1e-4;
pub(crate) const POSITION_TOLERANCE: f64 = 1e-4;

/// Hardware facing convention measured on the RTX 5080 (Vulkan, wgpu 30.0.1): a
/// triangle is front-facing when its vertices appear counter-clockwise from the
/// ray origin in object space, i.e. when the ray travels against
/// `(v1 - v0) x (v2 - v0)`.
pub(crate) const FRONT_FACE_CLOCKWISE: bool = false;

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalize(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
}
fn f64x3(v: [f32; 3]) -> V3 {
    [v[0].into(), v[1].into(), v[2].into()]
}

/// Affine transform as rows of a 3x4 matrix, in f64.
#[derive(Clone, Copy)]
struct Affine {
    m: [[f64; 4]; 3],
}

impl Affine {
    fn from_rows(t: &[f32; 12]) -> Self {
        let mut m = [[0.0; 4]; 3];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().enumerate() {
                *v = t[r * 4 + c].into();
            }
        }
        Self { m }
    }
    fn linear(&self, v: V3) -> V3 {
        let m = &self.m;
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    }
    fn point(&self, v: V3) -> V3 {
        let l = self.linear(v);
        [
            l[0] + self.m[0][3],
            l[1] + self.m[1][3],
            l[2] + self.m[2][3],
        ]
    }
    fn inverse(&self) -> Self {
        let a = &self.m;
        let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
        let inv = |r: usize, c: usize| {
            // Cofactor of (c, r) / det.
            let (r0, r1) = match c {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let (c0, c1) = match r {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let minor = a[r0][c0] * a[r1][c1] - a[r0][c1] * a[r1][c0];
            let sign = if (r + c).is_multiple_of(2) { 1.0 } else { -1.0 };
            sign * minor / det
        };
        let mut m = [[0.0; 4]; 3];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().take(3).enumerate() {
                *v = inv(r, c);
            }
        }
        let mut out = Self { m };
        let t = out.linear([a[0][3], a[1][3], a[2][3]]);
        for (r, row) in out.m.iter_mut().enumerate() {
            row[3] = -t[r];
        }
        out
    }
    /// Inverse transpose applied to a normal.
    fn normal(&self, n: V3) -> V3 {
        let inv = self.inverse();
        let m = &inv.m;
        normalize([
            m[0][0] * n[0] + m[1][0] * n[1] + m[2][0] * n[2],
            m[0][1] * n[0] + m[1][1] * n[1] + m[2][1] * n[2],
            m[0][2] * n[0] + m[1][2] * n[1] + m[2][2] * n[2],
        ])
    }
}

pub(crate) struct TestMesh {
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
    as_u16: bool,
    normals: Option<Vec<[f32; 3]>>,
}

pub(crate) struct TestInstance {
    mesh: usize,
    transform: [f32; 12],
    mask: u8,
    custom_index: u32,
    material_index: u32,
}

/// Expected closest hit of one probe ray.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ExpectedHit {
    pub t: f64,
    pub barycentrics: [f64; 2],
    pub instance_index: u32,
    pub custom_index: u32,
    pub primitive_index: u32,
    pub material_index: u32,
    pub front_face: bool,
    pub geometric_normal: V3,
    pub shading_normal: V3,
    pub position: V3,
}

pub(crate) struct Scenario {
    pub meshes: Vec<TestMesh>,
    pub instances: Vec<TestInstance>,
    pub rays: Vec<ProbeRay>,
    pub expected: Vec<Option<ExpectedHit>>,
    pub mask_rays: u32,
}

fn rows(linear: [[f64; 3]; 3], translation: V3) -> [f32; 12] {
    let mut t = [0.0f32; 12];
    for r in 0..3 {
        for c in 0..3 {
            t[r * 4 + c] = linear[r][c] as f32;
        }
        t[r * 4 + 3] = translation[r] as f32;
    }
    t
}

fn scenario_scene() -> (Vec<TestMesh>, Vec<TestInstance>) {
    let quad_positions = vec![
        [-1.0, -1.0, 0.0],
        [1.0, -1.0, 0.0],
        [1.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
    ];
    let quad_normals = quad_positions
        .iter()
        .map(|p: &[f32; 3]| {
            let n = normalize([0.3 * f64::from(p[0]), 0.3 * f64::from(p[1]), 1.0]);
            [n[0] as f32, n[1] as f32, n[2] as f32]
        })
        .collect();
    let meshes = vec![
        TestMesh {
            positions: quad_positions,
            indices: vec![0, 1, 2, 0, 2, 3],
            as_u16: true,
            normals: Some(quad_normals),
        },
        TestMesh {
            positions: vec![[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
            indices: vec![0, 1, 2],
            as_u16: false,
            normals: None,
        },
    ];
    let (s, c) = 30f64.to_radians().sin_cos();
    let instances = vec![
        // Scaled (x 2), rotated 30 degrees about +X, moved to z = 5.
        TestInstance {
            mesh: 0,
            transform: rows(
                [[2.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
                [0.0, 0.0, 5.0],
            ),
            mask: 0x01,
            custom_index: 7,
            material_index: 3,
        },
        TestInstance {
            mesh: 1,
            transform: rows(
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                [10.0, 0.0, 5.0],
            ),
            mask: 0x02,
            custom_index: 0xABCDE,
            material_index: 5,
        },
        // The quad again, behind instance 0.
        TestInstance {
            mesh: 0,
            transform: rows(
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                [0.0, 0.0, 8.0],
            ),
            mask: 0x04,
            custom_index: 11,
            material_index: 4,
        },
    ];
    (meshes, instances)
}

/// Exact CPU closest hit; `None` also when the ray passes within `margin`
/// (barycentric) of any triangle edge, which makes it ambiguous.
fn reference(
    meshes: &[TestMesh],
    instances: &[TestInstance],
    ray: &ProbeRay,
    margin: f64,
) -> Result<Option<ExpectedHit>, ()> {
    let mut best: Option<ExpectedHit> = None;
    for (slot, instance) in instances.iter().enumerate() {
        let mesh = &meshes[instance.mesh];
        let to_world = Affine::from_rows(&instance.transform);
        let to_object = to_world.inverse();
        let origin = to_object.point(f64x3(ray.origin));
        let dir = to_object.linear(f64x3(ray.direction));
        let visible = u32::from(instance.mask) & ray.mask != 0;
        for (k, tri) in mesh.indices.chunks(3).enumerate() {
            let p = |i: u32| f64x3(mesh.positions[i as usize]);
            let (p0, p1, p2) = (p(tri[0]), p(tri[1]), p(tri[2]));
            let e1 = sub(p1, p0);
            let e2 = sub(p2, p0);
            let pv = cross(dir, e2);
            let det = dot(e1, pv);
            if det.abs() < 1e-12 {
                continue;
            }
            let tv = sub(origin, p0);
            let u = dot(tv, pv) / det;
            let qv = cross(tv, e1);
            let v = dot(dir, qv) / det;
            let t = dot(e2, qv) / det;
            let w = 1.0 - u - v;
            let in_range = t >= f64::from(ray.t_min) && t <= f64::from(ray.t_max);
            // A ray passing within `margin` of a triangle's edge is ambiguous.
            let near_edge =
                [u, v, w].iter().all(|x| *x > -margin) && [u, v, w].iter().any(|x| *x < margin);
            if near_edge && t > 0.0 {
                return Err(());
            }
            if !(visible && in_range && u >= 0.0 && v >= 0.0 && w >= 0.0) {
                continue;
            }
            if best.is_some_and(|b| b.t <= t) {
                continue;
            }
            let face = cross(e1, e2);
            let clockwise_from_origin = dot(face, dir) > 0.0;
            let geometric = to_world.normal(face);
            let shading = match &mesh.normals {
                Some(n) => {
                    let n = |i: u32| f64x3(n[i as usize]);
                    let (n0, n1, n2) = (n(tri[0]), n(tri[1]), n(tri[2]));
                    let mix = [
                        n0[0] * w + n1[0] * u + n2[0] * v,
                        n0[1] * w + n1[1] * u + n2[1] * v,
                        n0[2] * w + n1[2] * u + n2[2] * v,
                    ];
                    to_world.normal(mix)
                }
                None => geometric,
            };
            let o = f64x3(ray.origin);
            let d = f64x3(ray.direction);
            best = Some(ExpectedHit {
                t,
                barycentrics: [u, v],
                instance_index: slot as u32,
                custom_index: instance.custom_index,
                primitive_index: k as u32,
                material_index: instance.material_index,
                front_face: clockwise_from_origin == FRONT_FACE_CLOCKWISE,
                geometric_normal: geometric,
                shading_normal: shading,
                position: [o[0] + d[0] * t, o[1] + d[1] * t, o[2] + d[2] * t],
            });
        }
    }
    Ok(best)
}

/// The fixed self-test scenario: about 3,000 rays in eight sets.
pub(crate) fn scenario() -> Scenario {
    let (meshes, instances) = scenario_scene();
    let mut rays = Vec::new();
    let mut expected = Vec::new();
    let mut mask_rays = 0;
    let grid = |x0: f32, y0: f32, n: usize| {
        (0..n).flat_map(move |i| (0..n).map(move |j| (x0 + 0.21 * i as f32, y0 + 0.21 * j as f32)))
    };
    // (grid origin, z, dir z, mask, t_min, t_max)
    let sets: [(f32, f32, f32, f32, u8, f32, f32); 8] = [
        (-2.17, -2.17, 0.0, 1.0, 0xff, 0.0, 100.0),
        (-2.17, -2.17, 0.0, 1.0, 0x04, 0.0, 100.0),
        (-2.17, -2.17, 0.0, 1.0, 0x02, 0.0, 100.0),
        (-2.17, -2.17, 0.0, 1.0, 0xff, 0.0, 4.0),
        (-2.17, -2.17, 0.0, 1.0, 0xff, 6.0, 100.0),
        (-2.17, -2.17, 20.0, -1.0, 0x04, 0.0, 100.0),
        (-2.17, -2.17, 0.0, 2.0, 0x05, 0.0, 100.0),
        (8.83, -1.17, 0.0, 1.0, 0x03, 0.0, 100.0),
    ];
    for (x0, y0, z, dz, mask, t_min, t_max) in sets {
        for (x, y) in grid(x0, y0, 21) {
            let ray = ProbeRay::new([x, y, z], [0.0, 0.0, dz], t_min, t_max, mask);
            let Ok(hit) = reference(&meshes, &instances, &ray, 0.02) else {
                continue;
            };
            // Count rays where the mask changes the answer.
            let all = ProbeRay { mask: 0xff, ..ray };
            if mask != 0xff && reference(&meshes, &instances, &all, 0.0).ok().flatten() != hit {
                mask_rays += 1;
            }
            rays.push(ray);
            expected.push(hit);
        }
    }
    Scenario {
        meshes,
        instances,
        rays,
        expected,
        mask_rays,
    }
}

fn mismatch(i: usize, ray: &ProbeRay, what: String) -> String {
    format!(
        "self-test ray {i} (origin {:?}, dir {:?}, mask {:#04x}, t in [{}, {}]): {what}",
        ray.origin, ray.direction, ray.mask, ray.t_min, ray.t_max
    )
}

/// Compares GPU results with the reference; the first mismatch is the error,
/// with the measured and expected numbers.
pub(crate) fn compare(
    rays: &[ProbeRay],
    expected: &[Option<ExpectedHit>],
    actual: &[ProbeHit],
    mask_rays: u32,
) -> Result<SelfTestReport, String> {
    let mut report = SelfTestReport {
        rays: rays.len() as u32,
        mask_rays,
        ..Default::default()
    };
    if actual.len() != expected.len() {
        return Err(format!(
            "self-test returned {} results for {} rays",
            actual.len(),
            expected.len()
        ));
    }
    let normal_error = |e: V3, a: [f32; 3]| 1.0 - dot(e, f64x3(a));
    for (i, ((ray, want), got)) in rays.iter().zip(expected).zip(actual).enumerate() {
        let Some(want) = want else {
            report.expected_misses += 1;
            if got.hit() {
                return Err(mismatch(
                    i,
                    ray,
                    format!(
                        "expected a miss, hit instance {} primitive {} at t = {}",
                        got.instance_index, got.primitive_index, got.t
                    ),
                ));
            }
            if got.occluded() {
                return Err(mismatch(
                    i,
                    ray,
                    "expected a miss, rt_occluded is true".into(),
                ));
            }
            continue;
        };
        report.expected_hits += 1;
        if !got.hit() {
            return Err(mismatch(
                i,
                ray,
                format!(
                    "missed; expected instance {} primitive {} at t = {:.6}",
                    want.instance_index, want.primitive_index, want.t
                ),
            ));
        }
        if !got.occluded() {
            return Err(mismatch(i, ray, "hit, but rt_occluded is false".into()));
        }
        let ids = (
            got.instance_index,
            got.custom_index,
            got.primitive_index,
            got.material_index,
        );
        let want_ids = (
            want.instance_index,
            want.custom_index,
            want.primitive_index,
            want.material_index,
        );
        if ids != want_ids {
            return Err(mismatch(
                i,
                ray,
                format!(
                    "(instance, custom index, primitive, material) = {ids:?}, expected {want_ids:?}"
                ),
            ));
        }
        if got.front_face() != want.front_face {
            return Err(mismatch(
                i,
                ray,
                format!(
                    "front_face = {}, expected {}",
                    got.front_face(),
                    want.front_face
                ),
            ));
        }
        let t_error = (f64::from(got.t) - want.t).abs();
        if t_error.is_nan() || t_error > T_TOLERANCE * want.t.abs().max(1.0) {
            return Err(mismatch(
                i,
                ray,
                format!(
                    "t = {}, expected {:.6} (|error| {t_error:e})",
                    got.t, want.t
                ),
            ));
        }
        let b_error = (f64::from(got.barycentrics[0]) - want.barycentrics[0])
            .abs()
            .max((f64::from(got.barycentrics[1]) - want.barycentrics[1]).abs());
        if b_error.is_nan() || b_error > BARYCENTRIC_TOLERANCE {
            return Err(mismatch(
                i,
                ray,
                format!(
                    "barycentrics = {:?}, expected [{:.6}, {:.6}]",
                    got.barycentrics, want.barycentrics[0], want.barycentrics[1]
                ),
            ));
        }
        let n_error = normal_error(want.geometric_normal, got.geometric_normal)
            .max(normal_error(want.shading_normal, got.shading_normal));
        if n_error.is_nan() || n_error > NORMAL_TOLERANCE {
            return Err(mismatch(
                i,
                ray,
                format!(
                    "normals geometric {:?} shading {:?}, expected {:?} and {:?}",
                    got.geometric_normal,
                    got.shading_normal,
                    want.geometric_normal,
                    want.shading_normal
                ),
            ));
        }
        let p = f64x3(got.position);
        let p_error = dot(sub(p, want.position), sub(p, want.position)).sqrt();
        let p_scale = dot(want.position, want.position).sqrt().max(1.0);
        if p_error.is_nan() || p_error > POSITION_TOLERANCE * p_scale {
            return Err(mismatch(
                i,
                ray,
                format!("position {:?}, expected {:?}", got.position, want.position),
            ));
        }
        report.matched_hits += 1;
        if !want.front_face {
            report.back_face_hits += 1;
        }
        report.max_t_error = report.max_t_error.max(t_error as f32);
        report.max_barycentric_error = report.max_barycentric_error.max(b_error as f32);
        report.max_normal_error = report.max_normal_error.max(n_error.max(0.0) as f32);
        report.max_position_error = report.max_position_error.max(p_error as f32);
    }
    Ok(report)
}

fn build_scene(core: &Arc<Core>, scenario: &Scenario) -> Result<RtScene, RtError> {
    let mut scene = RtScene::with_core(core.clone(), RtSceneLimits::for_device(&core.limits))?;
    let mut ids = Vec::new();
    for mesh in &scenario.meshes {
        let narrow: Vec<u16>;
        let indices = if mesh.as_u16 {
            narrow = mesh.indices.iter().map(|&i| i as u16).collect();
            RtIndices::U16(&narrow)
        } else {
            RtIndices::U32(&mesh.indices)
        };
        ids.push(scene.add_mesh(&RtMeshData {
            positions: &mesh.positions,
            indices,
            normals: mesh.normals.as_deref(),
        })?);
    }
    for instance in &scenario.instances {
        scene.add_instance(&RtInstanceDesc {
            mesh: ids[instance.mesh],
            transform: instance.transform,
            mask: instance.mask,
            custom_index: instance.custom_index,
            material_index: instance.material_index,
        })?;
    }
    scene.commit()?;
    Ok(scene)
}

/// Runs the self-test. `Err(Invalid)` carries the mismatch description;
/// `Err(Gpu)` a GPU failure.
pub(crate) fn run(core: &Arc<Core>, probe: &ProbeKernel) -> Result<SelfTestReport, RtError> {
    let scenario = scenario();
    let scene = build_scene(core, &scenario).map_err(|e| match e {
        RtError::Invalid(m) => invalid(format!("self-test scene rejected: {m}")),
        other => other,
    })?;
    let actual = probe.cast(core, &scene, &scenario.rays)?;
    compare(
        &scenario.rays,
        &scenario.expected,
        &actual,
        scenario.mask_rays,
    )
    .map_err(|m| invalid(format!("self-test step 'closest hit' failed: {m}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_inverse_round_trips() {
        let (_, instances) = scenario_scene();
        let m = Affine::from_rows(&instances[0].transform);
        let p = [0.3, -0.7, 2.0];
        let back = m.inverse().point(m.point(p));
        assert!(sub(back, p).iter().all(|e| e.abs() < 1e-12), "{back:?}");
    }

    #[test]
    fn scenario_covers_hits_misses_masks_and_back_faces() {
        let s = scenario();
        let hits: Vec<_> = s.expected.iter().flatten().collect();
        let misses = s.expected.iter().filter(|e| e.is_none()).count();
        assert!(s.rays.len() > 1000, "{} rays", s.rays.len());
        assert!(
            misses > 200 && hits.len() > 300,
            "{misses} misses, {} hits",
            hits.len()
        );
        for instance in 0..3 {
            assert!(
                hits.iter().any(|h| h.instance_index == instance),
                "instance {instance}"
            );
        }
        assert!(hits.iter().any(|h| h.primitive_index == 0));
        assert!(hits.iter().any(|h| h.primitive_index == 1));
        assert!(hits.iter().any(|h| h.front_face) && hits.iter().any(|h| !h.front_face));
        assert!(hits.iter().any(|h| h.custom_index == 0xABCDE));
        assert!(s.mask_rays > 100, "{} mask rays", s.mask_rays);
        // Instance 0 is tilted: t varies across its hits.
        let t: Vec<f64> = hits
            .iter()
            .filter(|h| h.instance_index == 0)
            .map(|h| h.t)
            .collect();
        let spread =
            t.iter().cloned().fold(f64::MIN, f64::max) - t.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread > 0.5, "t spread {spread}");
    }

    fn as_probe(e: &ExpectedHit) -> ProbeHit {
        let f = |v: V3| [v[0] as f32, v[1] as f32, v[2] as f32];
        ProbeHit {
            t: e.t as f32,
            barycentrics: [e.barycentrics[0] as f32, e.barycentrics[1] as f32],
            flags: 1 | 4 | if e.front_face { 2 } else { 0 },
            custom_index: e.custom_index,
            instance_index: e.instance_index,
            primitive_index: e.primitive_index,
            material_index: e.material_index,
            geometric_normal: f(e.geometric_normal),
            shading_normal: f(e.shading_normal),
            position: f(e.position),
            ..Default::default()
        }
    }

    #[test]
    fn exact_results_pass_and_each_kind_of_mismatch_is_reported_with_numbers() {
        let s = scenario();
        let perfect: Vec<ProbeHit> = s
            .expected
            .iter()
            .map(|e| e.as_ref().map(as_probe).unwrap_or_default())
            .collect();
        let report = compare(&s.rays, &s.expected, &perfect, s.mask_rays).unwrap();
        assert_eq!(report.matched_hits, report.expected_hits);
        assert!(report.max_t_error < 1e-5);

        let first_hit = s.expected.iter().position(Option::is_some).unwrap();
        let first_miss = s.expected.iter().position(Option::is_none).unwrap();
        let fail = |edit: &dyn Fn(&mut ProbeHit), at: usize| {
            let mut bad = perfect.clone();
            edit(&mut bad[at]);
            compare(&s.rays, &s.expected, &bad, 0).unwrap_err()
        };
        assert!(fail(&|h| h.t += 0.01, first_hit).contains("|error|"));
        assert!(fail(&|h| h.barycentrics[0] += 0.01, first_hit).contains("barycentrics"));
        assert!(fail(&|h| h.flags = 0, first_hit).contains("missed"));
        assert!(fail(&|h| h.flags &= !4, first_hit).contains("rt_occluded is false"));
        assert!(fail(&|h| h.flags ^= 2, first_hit).contains("front_face"));
        assert!(fail(&|h| h.custom_index += 1, first_hit).contains("custom index"));
        assert!(fail(&|h| h.shading_normal = [1.0, 0.0, 0.0], first_hit).contains("normals"));
        assert!(fail(&|h| h.flags = 1, first_miss).contains("expected a miss"));
        assert!(fail(&|h| h.flags = 4, first_miss).contains("rt_occluded is true"));
    }
}
