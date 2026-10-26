//! World extraction: ECS entities to a plain [`Collected`] description, then to an
//! `engine_acoustics::AcousticWorld`. The two steps are split so extraction is
//! deterministic and testable without the world builder.
//!
//! Sources of geometry (design record section 5.2):
//! - `MeshRenderer` (or `engine.AcousticGeometry.mesh`) meshes with their world
//!   transform become instances. World units are acoustic metres.
//! - With a `proxy2d` environment, non-trigger `Collider2D`, non-sensor
//!   `engine.PhysicsBody2D` (hull, trimesh and compound shapes contribute polygons or parts;
//!   cylinder and heightfield add a named warning) and `space: "proxy2d"` volumes become 2D footprints
//!   for the proxy synthesizer, and 3D meshes are not used.
//! - `engine.AcousticVolume` solids and rooms with `space: "world"`.
//! - Opt-in, non-sensor `engine.PhysicsBody3D` boxes without mesh geometry become
//!   masked acoustic instances with body-local geometry and current world pose.
use super::components::*;
use crate::{
    Error, Result, WorldTransform,
    content::{Mesh, Vertex},
    runtime::AuthoredId,
};
use bevy_ecs::prelude::*;
use engine_acoustics as ac;
use glam::{Mat4, Vec3};
use serde::Serialize;
use std::{collections::BTreeMap, sync::Arc};

/// Resolves a catalog `mesh` asset ID to loaded geometry.
pub type MeshLookup<'a> = dyn Fn(&str) -> Option<Arc<Mesh>> + 'a;

fn invalid(message: impl Into<String>) -> Error {
    Error::new("acoustics.world.invalid", message)
}
pub(super) fn from_engine(error: ac::AcousticsError) -> Error {
    Error::new(error.code, error.message)
}
fn entity_key(entity: &EntityRef) -> String {
    entity.get::<AuthoredId>().map_or_else(
        || format!("entity:{}", entity.id().to_bits()),
        |a| a.id.clone(),
    )
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InstanceSpec {
    pub key: String,
    /// Mesh asset ID (acoustic override or render mesh).
    pub mesh: String,
    /// Index into [`Collected::materials`].
    pub material: usize,
    /// Row-major 3x4, the layout of `engine_acoustics::Affine3`.
    pub transform: [f32; 12],
    pub mobility: Mobility,
    pub mask: u8,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum FootprintShape {
    Rect {
        center: [f32; 2],
        half_extents: [f32; 2],
        rotation: f32,
    },
    Circle {
        center: [f32; 2],
        radius: f32,
    },
    Capsule {
        center: [f32; 2],
        half_height: f32,
        radius: f32,
        rotation: f32,
    },
    /// Convex polygon in world XY; the proxy synthesizer fixes its winding.
    Polygon {
        points: Vec<[f32; 2]>,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FootprintSpec {
    pub key: String,
    pub shape: FootprintShape,
    pub material: usize,
    pub mobility: Mobility,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum VolumeGeometry {
    Box { half_extents: [f32; 3] },
    Mesh(String),
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VolumeSpec {
    pub key: String,
    pub geometry: VolumeGeometry,
    pub kind: VolumeKind,
    pub open_mask: u8,
    pub material: usize,
    pub transform: [f32; 12],
    pub mobility: Mobility,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProxySpec {
    pub settings: Proxy2D,
    pub material: usize,
    pub floor_material: usize,
    pub ceiling_material: usize,
    pub side_material: usize,
}

/// Everything the world builder needs, in deterministic (authored-ID) order.
#[derive(Clone, Debug)]
pub struct Collected {
    pub environment: AcousticEnvironment,
    pub materials: Vec<AcousticMaterialSpec>,
    pub instances: Vec<InstanceSpec>,
    pub footprints: Vec<FootprintSpec>,
    pub volumes: Vec<VolumeSpec>,
    pub proxy: Option<ProxySpec>,
    pub meshes: BTreeMap<String, Arc<Mesh>>,
    /// Non-fatal findings (ignored components, unused sources).
    pub warnings: Vec<String>,
}

/// The single environment of the world, if any.
pub fn find_environment(world: &World) -> Result<Option<(Entity, AcousticEnvironment)>> {
    let mut found: Option<(Entity, AcousticEnvironment)> = None;
    for entity in world.iter_entities() {
        if let Some(env) = entity.get::<AcousticEnvironment>() {
            if found.is_some() {
                return Err(Error::new(
                    "acoustics.environment.multiple",
                    "At most one engine.AcousticEnvironment per scene",
                ));
            }
            found = Some((entity.id(), env.clone()));
        }
    }
    Ok(found)
}

struct Materials(Vec<AcousticMaterialSpec>);
impl Materials {
    fn index(&mut self, spec: AcousticMaterialSpec) -> usize {
        self.0.iter().position(|m| *m == spec).unwrap_or_else(|| {
            self.0.push(spec);
            self.0.len() - 1
        })
    }
    fn preset(&mut self, preset: Preset) -> usize {
        self.index(AcousticMaterialSpec {
            preset,
            ..Default::default()
        })
    }
}

fn affine(matrix: &WorldTransform) -> [f32; 12] {
    let c = matrix.matrix;
    [
        c[0][0], c[1][0], c[2][0], c[3][0], c[0][1], c[1][1], c[2][1], c[3][1], c[0][2], c[1][2],
        c[2][2], c[3][2],
    ]
}
fn mesh_geometry(
    id: &str,
    lookup: &MeshLookup,
    cache: &mut BTreeMap<String, Arc<Mesh>>,
) -> Result<()> {
    if cache.contains_key(id) {
        return Ok(());
    }
    let mesh = lookup(id).ok_or_else(|| invalid(format!("Unknown acoustic mesh asset {id}")))?;
    if mesh.indices.len() % 3 != 0
        || mesh
            .indices
            .iter()
            .any(|i| *i as usize >= mesh.vertices.len())
    {
        return Err(invalid(format!(
            "Mesh {id} needs triangle indices within its vertex range"
        )));
    }
    cache.insert(id.to_owned(), mesh);
    Ok(())
}
/// A deterministic, outward-facing twelve-triangle box in body-local coordinates.
fn physics_box_mesh(half_extents: [f32; 3]) -> Mesh {
    let [x, y, z] = half_extents;
    let positions = [
        [-x, -y, -z],
        [x, -y, -z],
        [x, y, -z],
        [-x, y, -z],
        [-x, -y, z],
        [x, -y, z],
        [x, y, z],
        [-x, y, z],
    ];
    let vertices = positions
        .into_iter()
        .map(|position| Vertex {
            position,
            uv: [0.; 2],
            color: [1.; 4],
        })
        .collect();
    Mesh {
        normals: vec![],
        vertices,
        indices: vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 3, 7, 6, 3, 6, 2, 0, 4, 7, 0, 7,
            3, 1, 2, 6, 1, 6, 5,
        ],
    }
}
fn physics_box_key(half_extents: [f32; 3]) -> String {
    format!(
        "\0physics3d/box/{:08x}/{:08x}/{:08x}",
        half_extents[0].to_bits(),
        half_extents[1].to_bits(),
        half_extents[2].to_bits(),
    )
}
/// Planar pose of a 2D entity: rotation about Z and the XY scale lengths.
fn planar(matrix: &Mat4) -> (f32, f32, f32) {
    let x = matrix.x_axis.truncate();
    let y = matrix.y_axis.truncate();
    (x.y.atan2(x.x), x.truncate().length(), y.truncate().length())
}
/// Convex hull of planar points (monotone chain), counter-clockwise, without collinear points.
fn convex_hull_2d(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut pts: Vec<[f32; 2]> = points
        .iter()
        .copied()
        .filter(|p| p[0].is_finite() && p[1].is_finite())
        .collect();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut hull: Vec<[f32; 2]> = Vec::with_capacity(pts.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        for &p in &pts {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
        if pass == 0 {
            pts.reverse();
        }
    }
    hull
}
/// Limits the proxy builder enforces on footprints.
fn footprint_limits() -> (usize, usize) {
    let limits = ac::WorldLimits::default();
    (
        limits.max_footprint_vertices as usize,
        limits.max_proxy_footprints as usize,
    )
}
/// Reduces a convex counter-clockwise polygon to at most `max` vertices by repeatedly dropping
/// the vertex whose triangle with its neighbours has the least area. The result stays convex
/// (it only ever cuts corners) and the choice is deterministic.
fn simplify_convex(mut hull: Vec<[f32; 2]>, max: usize) -> Vec<[f32; 2]> {
    let corner = |p: [f32; 2], v: [f32; 2], n: [f32; 2]| {
        ((v[0] - p[0]) * (n[1] - p[1]) - (v[1] - p[1]) * (n[0] - p[0])).abs()
    };
    while hull.len() > max.max(3) {
        let count = hull.len();
        let mut best = (f32::INFINITY, 0);
        for i in 0..count {
            let area = corner(
                hull[(i + count - 1) % count],
                hull[i],
                hull[(i + 1) % count],
            );
            if area < best.0 {
                best = (area, i);
            }
        }
        hull.remove(best.1);
    }
    hull
}
/// World-space convex polygon of local planar `points` under `m`, or `None` without area. A
/// hull with more vertices than the builder accepts is simplified; the second value is the
/// vertex count before simplification when that happened.
fn world_polygon(m: &Mat4, points: &[[f32; 2]]) -> Option<(FootprintShape, Option<usize>)> {
    let world: Vec<[f32; 2]> = points
        .iter()
        .map(|p| {
            let w = m.transform_point3(Vec3::new(p[0], p[1], 0.0));
            [w.x, w.y]
        })
        .collect();
    let hull = convex_hull_2d(&world);
    let area: f32 = (0..hull.len())
        .map(|i| {
            let (a, b) = (hull[i], hull[(i + 1) % hull.len()]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum();
    if hull.len() < 3 || area.abs() <= 1e-12 {
        return None;
    }
    let (max_vertices, _) = footprint_limits();
    let original = hull.len();
    let simplified = original > max_vertices;
    let points = if simplified {
        simplify_convex(hull, max_vertices)
    } else {
        hull
    };
    Some((
        FootprintShape::Polygon { points },
        simplified.then_some(original),
    ))
}
/// Footprints of one 2D physics shape (or compound part) posed by `m`. A shape with no planar
/// footprint adds a named warning instead of vanishing silently.
fn push_body_footprints(
    key: &str,
    shape: crate::simulation::Shape,
    body: &crate::simulation::PhysicsBody2D,
    part: Option<&crate::simulation::CompoundPart2D>,
    m: &Mat4,
    out: &mut Vec<FootprintShape>,
    warnings: &mut Vec<String>,
) {
    use crate::simulation::Shape;
    let (rotation, sx, sy) = planar(m);
    let center = [m.w_axis.x, m.w_axis.y];
    let radius_scale = sx.max(sy);
    let (half_extents, radius, half_height, points) = part.map_or(
        (
            body.half_extents,
            body.radius,
            body.half_height,
            &body.points,
        ),
        |p| (p.half_extents, p.radius, p.half_height, &p.points),
    );
    let unsupported = |warnings: &mut Vec<String>, what: &str| {
        warnings.push(format!(
            "{key}: {what} has no acoustic proxy footprint and is ignored"
        ));
    };
    match shape {
        Shape::Box => out.push(FootprintShape::Rect {
            center,
            half_extents: [half_extents[0] * sx, half_extents[1] * sy],
            rotation,
        }),
        Shape::Ball => out.push(FootprintShape::Circle {
            center,
            radius: radius * radius_scale,
        }),
        Shape::Capsule => out.push(FootprintShape::Capsule {
            center,
            half_height: half_height * sy,
            radius: radius * radius_scale,
            rotation,
        }),
        Shape::ConvexHull => match world_polygon(m, points) {
            Some((polygon, simplified)) => {
                if let Some(original) = simplified {
                    warnings.push(format!(
                        "{key}: convex_hull footprint simplified from {original} to {} vertices (the proxy builder accepts at most {})",
                        footprint_limits().0,
                        footprint_limits().0
                    ));
                }
                out.push(polygon);
            }
            None => unsupported(
                warnings,
                "convex_hull shape (fewer than 3 distinct points or no area)",
            ),
        },
        Shape::TriMesh if part.is_none() => {
            let before = out.len();
            for tri in &body.indices {
                let corners: Option<Vec<[f32; 2]>> = tri
                    .iter()
                    .map(|&i| body.vertices.get(i as usize).copied())
                    .collect();
                if let Some((polygon, _)) = corners.and_then(|c| world_polygon(m, &c)) {
                    out.push(polygon);
                }
            }
            if out.len() == before {
                unsupported(warnings, "trimesh shape (no triangle with area)");
            }
        }
        Shape::Compound if part.is_none() => {
            for p in &body.parts {
                let local = *m
                    * Mat4::from_translation(Vec3::new(p.position[0], p.position[1], 0.0))
                    * Mat4::from_rotation_z(p.rotation);
                push_body_footprints(key, p.shape, body, Some(p), &local, out, warnings);
            }
        }
        Shape::Cylinder => unsupported(warnings, "cylinder shape (3D only)"),
        Shape::HeightField => unsupported(warnings, "heightfield shape"),
        Shape::TriMesh | Shape::Compound => {
            unsupported(warnings, &format!("nested {shape:?} part"))
        }
    }
}
fn footprint_of(
    key: &str,
    entity: &EntityRef,
    matrix: &WorldTransform,
    proxy: &Proxy2D,
    warnings: &mut Vec<String>,
) -> Vec<(FootprintShape, Mobility)> {
    let m = Mat4::from_cols_array_2d(&matrix.matrix);
    let (rotation, sx, sy) = planar(&m);
    if proxy.colliders
        && let Some(b) = entity.get::<crate::Collider2D>().filter(|b| !b.trigger)
    {
        let center = m.transform_point3(Vec3::new(b.offset[0], b.offset[1], 0.0));
        return vec![(
            FootprintShape::Rect {
                center: [center.x, center.y],
                half_extents: [b.size[0] * 0.5 * sx, b.size[1] * 0.5 * sy],
                rotation,
            },
            Mobility::Static,
        )];
    }
    if proxy.physics_bodies
        && let Some(b) = entity
            .get::<crate::simulation::PhysicsBody2D>()
            .filter(|b| !b.material.sensor)
    {
        let mobility = if b.motion == crate::simulation::Motion::Fixed {
            Mobility::Static
        } else {
            Mobility::Dynamic
        };
        let mut shapes = vec![];
        push_body_footprints(key, b.shape, b, None, &m, &mut shapes, warnings);
        return shapes.into_iter().map(|s| (s, mobility)).collect();
    }
    vec![]
}

/// Collect the acoustic scene description, or `None` without an enabled
/// environment. Errors are coded `acoustics.*` and never produce a partial world.
pub fn collect(world: &World, lookup: &MeshLookup) -> Result<Option<Collected>> {
    let Some((_, environment)) = find_environment(world)? else {
        return Ok(None);
    };
    if !environment.enabled {
        return Ok(None);
    }
    let mut materials = Materials(vec![]);
    let default_material = materials.preset(environment.geometry.default_material);
    let proxy_active = environment.proxy2d.is_some();
    let mut out = Collected {
        environment: environment.clone(),
        materials: vec![],
        instances: vec![],
        footprints: vec![],
        volumes: vec![],
        proxy: None,
        meshes: BTreeMap::new(),
        warnings: vec![],
    };
    if let Some(settings) = &environment.proxy2d {
        out.proxy = Some(ProxySpec {
            settings: settings.clone(),
            material: materials.preset(settings.material),
            floor_material: materials.preset(settings.floor_material),
            ceiling_material: materials.preset(settings.ceiling_material),
            side_material: materials.preset(settings.side_material),
        });
    }
    let mut entities: Vec<_> = world
        .iter_entities()
        .filter(|e| e.contains::<WorldTransform>())
        .map(|e| (entity_key(&e), e))
        .collect();
    entities.sort_by(|a, b| a.0.cmp(&b.0));
    let mut skipped_meshes = 0usize;
    for (key, entity) in &entities {
        let matrix = entity
            .get::<WorldTransform>()
            .copied()
            .expect("filtered on WorldTransform");
        let material = entity
            .get::<AcousticMaterialSpec>()
            .cloned()
            .map(|spec| materials.index(spec));
        let geometry = entity.get::<AcousticGeometry>();
        if let Some(volume) = entity.get::<AcousticVolume>() {
            let material = material.unwrap_or(default_material);
            match (volume.space, &environment.proxy2d) {
                (Space::Proxy2d, Some(proxy)) => {
                    let m = Mat4::from_cols_array_2d(&matrix.matrix);
                    let (rotation, sx, sy) = planar(&m);
                    let p = m.w_axis;
                    out.footprints.push(FootprintSpec {
                        key: key.clone(),
                        shape: FootprintShape::Rect {
                            center: [p.x, p.y],
                            half_extents: [
                                volume.half_extents[0] * sx,
                                volume.half_extents[1] * sy,
                            ],
                            rotation,
                        },
                        material: if entity.contains::<AcousticMaterialSpec>() {
                            material
                        } else {
                            materials.preset(proxy.material)
                        },
                        mobility: Mobility::Static,
                    });
                }
                (Space::World, None) => {
                    out.volumes.push(VolumeSpec {
                        key: key.clone(),
                        geometry: match (&volume.mesh, volume.shape) {
                            (Some(id), VolumeShape::Mesh) => {
                                mesh_geometry(id, lookup, &mut out.meshes)?;
                                VolumeGeometry::Mesh(id.clone())
                            }
                            _ => VolumeGeometry::Box {
                                half_extents: volume.half_extents,
                            },
                        },
                        kind: volume.kind,
                        open_mask: volume.open_mask(),
                        material,
                        transform: affine(&matrix),
                        mobility: volume.mobility,
                    });
                }
                (Space::Proxy2d, None) => out.warnings.push(format!(
                    "{key}: proxy2d volume ignored because the environment has no proxy2d"
                )),
                (Space::World, Some(_)) => out.warnings.push(format!(
                    "{key}: world-space volume ignored because the environment uses proxy2d"
                )),
            }
            continue;
        }
        if let Some(proxy) = &environment.proxy2d {
            for (shape, mobility) in footprint_of(key, entity, &matrix, proxy, &mut out.warnings) {
                out.footprints.push(FootprintSpec {
                    key: key.clone(),
                    shape,
                    material: material.unwrap_or_else(|| materials.preset(proxy.material)),
                    mobility,
                });
            }
            if entity.contains::<crate::MeshRenderer>() {
                skipped_meshes += 1;
            }
            continue;
        }
        let mode = geometry.map_or(GeometryMode::Static, |g| g.mode);
        let renderer_mesh = entity
            .get::<crate::MeshRenderer>()
            .filter(|_| environment.geometry.render_meshes)
            .map(|m| m.mesh.as_str());
        let override_mesh = geometry.and_then(|g| g.mesh.as_deref());
        if mode == GeometryMode::Exclude {
            continue;
        }
        if let Some(mesh) = override_mesh.or(renderer_mesh) {
            mesh_geometry(mesh, lookup, &mut out.meshes)?;
            out.instances.push(InstanceSpec {
                key: key.clone(),
                mesh: mesh.to_owned(),
                material: material.unwrap_or(default_material),
                transform: affine(&matrix),
                mobility: if mode == GeometryMode::Dynamic {
                    Mobility::Dynamic
                } else {
                    Mobility::Static
                },
                mask: geometry.map_or(environment.geometry.mask, |g| g.mask),
            });
            continue;
        }
        if !environment.geometry.physics_bodies {
            continue;
        }
        let Some(body) = entity.get::<crate::simulation::PhysicsBody3D>() else {
            continue;
        };
        if body.material.sensor {
            continue;
        }
        if body.shape != crate::simulation::Shape::Box {
            out.warnings.push(format!(
                "{key}: PhysicsBody3D {:?} has no acoustic collider proxy (only box is supported)",
                body.shape
            ));
            continue;
        }
        let half_extents = body.half_extents;
        if !half_extents.iter().all(|v| v.is_finite() && *v > 0.0) {
            out.warnings
                .push(format!("{key}: invalid PhysicsBody3D box omitted"));
            continue;
        }
        let mesh = physics_box_key(half_extents);
        out.meshes
            .entry(mesh.clone())
            .or_insert_with(|| Arc::new(physics_box_mesh(half_extents)));
        out.instances.push(InstanceSpec {
            key: key.clone(),
            mesh,
            material: material.unwrap_or(default_material),
            transform: affine(&matrix),
            mobility: if body.motion == crate::simulation::Motion::Fixed {
                Mobility::Static
            } else {
                Mobility::Dynamic
            },
            mask: geometry.map_or(environment.geometry.mask, |g| g.mask),
        });
    }
    if proxy_active && skipped_meshes > 0 {
        out.warnings.push(format!(
            "{skipped_meshes} MeshRenderer entities ignored because the environment uses proxy2d"
        ));
    }
    let cap = footprint_limits().1;
    if out.footprints.len() > cap {
        let ignored = out.footprints.len() - cap;
        out.footprints.truncate(cap);
        out.warnings.push(format!(
            "{ignored} proxy2d footprints beyond the limit of {cap} were ignored"
        ));
    }
    out.materials = materials.0;
    Ok(Some(out))
}

fn put(hasher: &mut blake3::Hasher, text: &str) {
    hasher.update(&(text.len() as u64).to_le_bytes());
    hasher.update(text.as_bytes());
}
fn put_floats(hasher: &mut blake3::Hasher, values: &[f32]) {
    for value in values {
        hasher.update(&value.to_bits().to_le_bytes());
    }
}
impl Collected {
    /// Hash of everything that needs a world rebuild when it changes. Poses of
    /// dynamic instances and volumes are excluded (they use `update_dynamic`),
    /// but their membership and mesh are included, so spawning or removing a
    /// dynamic entity rebuilds. 2D footprints are always rebuild-tracked,
    /// because the proxy synthesizer does not expose per-footprint handles.
    pub fn signature(&self) -> blake3::Hash {
        let mut h = blake3::Hasher::new();
        h.update(&serde_json::to_vec(&(&self.materials, &self.proxy)).expect("finite settings"));
        put(
            &mut h,
            self.environment.geometry.asset.as_deref().unwrap_or(""),
        );
        for i in &self.instances {
            put(&mut h, &i.key);
            put(&mut h, &i.mesh);
            h.update(&[i.mask, i.mobility as u8]);
            h.update(&(i.material as u64).to_le_bytes());
            if i.mobility == Mobility::Static {
                put_floats(&mut h, &i.transform);
            }
        }
        h.update(b"footprints");
        for f in &self.footprints {
            put(&mut h, &f.key);
            h.update(&(f.material as u64).to_le_bytes());
            h.update(&serde_json::to_vec(&f.shape).expect("finite shape"));
        }
        h.update(b"volumes");
        for v in &self.volumes {
            put(&mut h, &v.key);
            h.update(
                &serde_json::to_vec(&(&v.geometry, v.kind, v.open_mask, v.material))
                    .expect("finite volume"),
            );
            if v.mobility == Mobility::Static {
                put_floats(&mut h, &v.transform);
            }
        }
        h.finalize()
    }
    /// Current poses of dynamic instances and volumes, keyed by authored ID.
    pub fn dynamic_transforms(&self) -> BTreeMap<String, [f32; 12]> {
        let instances = self
            .instances
            .iter()
            .filter(|i| i.mobility == Mobility::Dynamic)
            .map(|i| (i.key.clone(), i.transform));
        let volumes = self
            .volumes
            .iter()
            .filter(|v| v.mobility == Mobility::Dynamic)
            .map(|v| (v.key.clone(), v.transform));
        instances.chain(volumes).collect()
    }
}

/// A built acoustic world and the handles that drive it afterwards.
pub struct WorldBuild {
    pub world: ac::AcousticWorld,
    /// Present for `proxy2d` environments.
    pub proxy: Option<ac::ProxyMapping>,
    /// Authored ID of every dynamic instance with its engine handle.
    pub dynamic: Vec<(String, ac::InstanceId)>,
}

fn acoustic_mesh(mesh: &Mesh, material: ac::MaterialId) -> ac::AcousticMesh {
    ac::AcousticMesh {
        positions: mesh.vertices.iter().map(|v| v.position).collect(),
        triangles: mesh.indices.as_chunks::<3>().0.to_vec(),
        materials: vec![material],
    }
}
fn footprint(
    shape: &FootprintShape,
    material: ac::MaterialId,
    mobility: Mobility,
) -> ac::Footprint {
    ac::Footprint {
        shape: match *shape {
            FootprintShape::Rect {
                center,
                half_extents,
                rotation,
            } => ac::Footprint2D::Rect {
                center,
                half_extents,
                rotation,
            },
            FootprintShape::Circle { center, radius } => ac::Footprint2D::Circle { center, radius },
            FootprintShape::Capsule {
                center,
                half_height,
                radius,
                rotation,
            } => ac::Footprint2D::Capsule {
                center,
                half_height,
                radius,
                rotation,
            },
            FootprintShape::Polygon { ref points } => ac::Footprint2D::Polygon {
                points: points.clone(),
            },
        },
        material,
        mobility: mobility.into(),
    }
}
fn proxy_settings(spec: &ProxySpec, ids: &[ac::MaterialId]) -> ac::Proxy2DSettings {
    let p = &spec.settings;
    ac::Proxy2DSettings {
        plane: match p.plane {
            Plane::SideView => ac::ProxyPlane::SideView,
            Plane::TopDown => ac::ProxyPlane::TopDown,
        },
        units_per_meter: p.units_per_meter,
        depth_m: p.depth_m,
        enclose_depth: p.enclose_depth,
        wall_height_m: p.wall_height_m,
        ear_height_m: p.ear_height_m,
        floor: p.floor,
        ceiling: p.ceiling,
        open_sky: p.open_sky,
        enclosure: match (p.enclosure, p.bounds) {
            (EnclosureMode::None, _) => ac::Enclosure::None,
            (EnclosureMode::Auto, _) => ac::Enclosure::Auto {
                margin_m: p.margin_m,
            },
            (EnclosureMode::Bounds, b) => {
                let b = b.unwrap_or_default();
                ac::Enclosure::Bounds {
                    min: [b[0], b[1]],
                    max: [b[2], b[3]],
                }
            }
        },
        material: ids[spec.material],
        floor_material: ids[spec.floor_material],
        ceiling_material: ids[spec.ceiling_material],
        side_material: ids[spec.side_material],
    }
}

/// Build the acoustic world. Fails with the builder's `acoustics.*` code (limits,
/// invalid material or proxy) and never returns a partial world.
pub fn build_world(collected: &Collected) -> Result<WorldBuild> {
    let mut builder = ac::AcousticWorldBuilder::new(ac::WorldLimits::default());
    let mut ids = Vec::with_capacity(collected.materials.len());
    for spec in &collected.materials {
        let material = spec.resolve().map_err(from_engine)?;
        ids.push(builder.add_material(material).map_err(from_engine)?);
    }
    let mut meshes: BTreeMap<(&str, usize), ac::MeshId> = BTreeMap::new();
    let mut dynamic = vec![];
    for i in &collected.instances {
        let mesh = match meshes.get(&(i.mesh.as_str(), i.material)) {
            Some(id) => *id,
            None => {
                let id = builder
                    .add_mesh(acoustic_mesh(&collected.meshes[&i.mesh], ids[i.material]))
                    .map_err(from_engine)?;
                meshes.insert((i.mesh.as_str(), i.material), id);
                id
            }
        };
        let id = builder
            .add_instance(mesh, i.transform, i.mobility.into(), i.mask)
            .map_err(from_engine)?;
        if i.mobility == Mobility::Dynamic {
            dynamic.push((i.key.clone(), id));
        }
    }
    for v in &collected.volumes {
        let shape = match &v.geometry {
            VolumeGeometry::Box { half_extents } => ac::VolumeShape::Box {
                half_extents: *half_extents,
            },
            VolumeGeometry::Mesh(asset) => ac::VolumeShape::Mesh(
                builder
                    .add_mesh(acoustic_mesh(&collected.meshes[asset], ids[v.material]))
                    .map_err(from_engine)?,
            ),
        };
        let volume = ac::AcousticVolume {
            shape,
            kind: match v.kind {
                VolumeKind::Solid => ac::VolumeKind::Solid,
                VolumeKind::Room => ac::VolumeKind::Room {
                    open_faces: v.open_mask,
                },
            },
            material: ids[v.material],
            mobility: v.mobility.into(),
        };
        let id = builder
            .add_volume(volume, v.transform)
            .map_err(from_engine)?;
        if v.mobility == Mobility::Dynamic {
            dynamic.push((v.key.clone(), id));
        }
    }
    let proxy = match &collected.proxy {
        Some(spec) => {
            // Footprints are rebuild-tracked (see `Collected::signature`), so they
            // are submitted as static even when a physics body can move.
            let footprints: Vec<_> = collected
                .footprints
                .iter()
                .map(|f| footprint(&f.shape, ids[f.material], Mobility::Static))
                .collect();
            Some(
                builder
                    .add_proxy_2d(&footprints, &proxy_settings(spec, &ids))
                    .map_err(from_engine)?,
            )
        }
        None => None,
    };
    let world = builder.build().map_err(from_engine)?;
    Ok(WorldBuild {
        world,
        proxy,
        dynamic,
    })
}

/// Which geometry families a baked document keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BakeInclude {
    pub meshes: bool,
    pub proxy2d: bool,
    pub volumes: bool,
}
impl Default for BakeInclude {
    fn default() -> Self {
        Self {
            meshes: true,
            proxy2d: true,
            volumes: true,
        }
    }
}

/// Source geometry of the collected world as an `AcousticWorldDocument` (the
/// `"acoustics"` asset kind). It stores meshes, instances, volumes and proxy
/// footprints, never a BVH; loading replays it through the builder. Dynamic
/// instances keep their `dynamic` mobility. The result is sealed with its content
/// hash.
pub fn to_document(
    collected: &Collected,
    include: BakeInclude,
    source_scene: Option<String>,
) -> Result<ac::AcousticWorldDocument> {
    let mut doc = ac::AcousticWorldDocument::new(source_scene);
    let mut ids = Vec::with_capacity(collected.materials.len());
    for (index, spec) in collected.materials.iter().enumerate() {
        doc.materials.push(spec.resolve().map_err(from_engine)?);
        ids.push(ac::MaterialId(
            u16::try_from(index).map_err(|_| invalid("more than 65535 acoustic materials"))?,
        ));
    }
    let mut meshes: BTreeMap<(&str, usize), ac::MeshId> = BTreeMap::new();
    if include.meshes {
        for i in &collected.instances {
            let mesh = match meshes.get(&(i.mesh.as_str(), i.material)) {
                Some(id) => *id,
                None => {
                    let id = ac::MeshId(doc.meshes.len() as u32);
                    doc.meshes
                        .push(acoustic_mesh(&collected.meshes[&i.mesh], ids[i.material]));
                    meshes.insert((i.mesh.as_str(), i.material), id);
                    id
                }
            };
            doc.instances.push(ac::DocumentInstance {
                mesh,
                transform: i.transform,
                mobility: i.mobility.into(),
                mask: i.mask,
            });
        }
    }
    if include.volumes {
        for v in &collected.volumes {
            let shape = match &v.geometry {
                VolumeGeometry::Box { half_extents } => ac::VolumeShape::Box {
                    half_extents: *half_extents,
                },
                VolumeGeometry::Mesh(asset) => {
                    let id = ac::MeshId(doc.meshes.len() as u32);
                    doc.meshes
                        .push(acoustic_mesh(&collected.meshes[asset], ids[v.material]));
                    ac::VolumeShape::Mesh(id)
                }
            };
            doc.volumes.push(ac::DocumentVolume {
                volume: ac::AcousticVolume {
                    shape,
                    kind: match v.kind {
                        VolumeKind::Solid => ac::VolumeKind::Solid,
                        VolumeKind::Room => ac::VolumeKind::Room {
                            open_faces: v.open_mask,
                        },
                    },
                    material: ids[v.material],
                    mobility: v.mobility.into(),
                },
                transform: v.transform,
            });
        }
    }
    if include.proxy2d
        && let Some(spec) = &collected.proxy
    {
        doc.proxy = Some(ac::DocumentProxy {
            footprints: collected
                .footprints
                .iter()
                .map(|f| footprint(&f.shape, ids[f.material], Mobility::Static))
                .collect(),
            settings: proxy_settings(spec, &ids),
        });
    }
    doc.seal();
    Ok(doc)
}
