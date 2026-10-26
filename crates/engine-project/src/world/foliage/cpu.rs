//! CPU fallback: merged cluster meshes per (cell, type) at one LOD each, drawn through
//! `MeshRenderer` + `GeometryOverride`.
//!
//! Every cell is a candidate cluster per type. The cluster's level comes from the distance of
//! the camera to the cell's bounding box through the type's [`LodBands`] (with hysteresis
//! kept in a [`LodTracker`]): a mesh LOD, the impostor (two crossed quads, so it needs no
//! camera-dependent geometry) or nothing. Clusters merge their instances' transformed
//! geometry; the total vertex count is bounded by `cpu_vertex_budget`, nearest clusters first,
//! and a cluster that does not fit completely keeps as many whole instances as do. The
//! meshes are static: the CPU path does not sway in the wind.
use super::assets::Loaded;
use super::component::{FoliageCluster, FoliageGenerated};
use crate::content::{Mesh, Vertex};
use crate::rendering3d::MeshMaterial;
use crate::runtime::AuthoredId;
use crate::{GeometryOverride, MeshRenderer, Transform};
use bevy_ecs::prelude::*;
use engine_foliage::{
    CellKey, FoliageField, FoliageType, Instance, LodBands, LodLevel, LodTracker,
};
use glam::{Mat3, Vec3};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Vertices of the impostor geometry per instance (two quads).
pub const IMPOSTOR_VERTICES: usize = 8;

struct Cluster {
    entity: Entity,
    level: LodLevel,
    cell_revision: u64,
    instances: usize,
    vertices: usize,
}

/// Per-cell facts needed for distance selection, rebuilt when the cell's revision changes.
struct CellFacts {
    revision: u64,
    y: [f32; 2],
    counts: Vec<u32>,
}

/// CPU fallback state of one foliage entity.
#[derive(Default)]
pub(super) struct Cpu {
    clusters: BTreeMap<(CellKey, u16), Cluster>,
    tracker: LodTracker,
    facts: HashMap<CellKey, CellFacts>,
    /// Inputs of the last build that make every cluster stale when they change.
    stamp: Option<Stamp>,
    last_camera: Option<Vec3>,
    pub vertices: usize,
    pub budget_limited: bool,
    /// Clusters per level: mesh levels 0..4, impostor.
    pub levels: [usize; 5],
}

#[derive(Clone, PartialEq)]
struct Stamp {
    types: Vec<FoliageType>,
    culling: [f32; 3],
    budget: u32,
}

impl Cpu {
    pub fn cluster_count(&self) -> usize {
        self.clusters.len()
    }
    /// Despawns every cluster entity.
    pub fn clear(&mut self, world: &mut World) {
        for (_, cluster) in std::mem::take(&mut self.clusters) {
            world.despawn(cluster.entity);
        }
        self.tracker.clear();
        self.facts.clear();
        self.stamp = None;
        self.last_camera = None;
        self.vertices = 0;
        self.levels = [0; 5];
        self.budget_limited = false;
    }
}

/// What the build needs.
pub(super) struct Inputs<'a> {
    pub owner: Entity,
    pub owner_id: &'a str,
    pub field: &'a FoliageField,
    pub types: &'a [FoliageType],
    pub loaded: &'a Loaded,
    pub default_cull: f32,
    pub lod_scale: f32,
    pub hysteresis: f32,
    pub budget: u32,
    pub camera: Option<Vec3>,
}

fn box_distance(camera: Vec3, rect: [[f32; 2]; 2], y: [f32; 2]) -> f32 {
    let dx = (rect[0][0] - camera.x).max(camera.x - rect[1][0]).max(0.0);
    let dz = (rect[0][1] - camera.z).max(camera.z - rect[1][1]).max(0.0);
    let dy = (y[0] - camera.y).max(camera.y - y[1]).max(0.0);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Merges `instances` of `mesh` into one mesh in world space.
pub fn merge_instances(mesh: &Mesh, instances: &[Instance]) -> Mesh {
    let n = mesh.vertices.len();
    let mut out = Mesh {
        normals: Vec::new(),
        vertices: Vec::with_capacity(n * instances.len()),
        indices: Vec::with_capacity(mesh.indices.len() * instances.len()),
    };
    let with_normals = mesh.normals.len() == n;
    if with_normals {
        out.normals.reserve(n * instances.len());
    }
    for instance in instances {
        let matrix = instance.matrix();
        let normal_matrix = Mat3::from_mat4(matrix).inverse().transpose();
        let base = out.vertices.len() as u32;
        for (i, v) in mesh.vertices.iter().enumerate() {
            out.vertices.push(Vertex {
                position: matrix.transform_point3(Vec3::from(v.position)).to_array(),
                uv: v.uv,
                color: v.color,
            });
            if with_normals {
                out.normals.push(
                    (normal_matrix * Vec3::from(mesh.normals[i]))
                        .normalize_or_zero()
                        .to_array(),
                );
            }
        }
        out.indices.extend(mesh.indices.iter().map(|&i| base + i));
    }
    out
}

/// Two crossed upright quads of `size` (width, height) standing on the origin.
pub fn impostor_mesh(size: [f32; 2]) -> Mesh {
    let (hw, h) = (size[0] * 0.5, size[1]);
    let quad = |axis: Vec3| -> [Vertex; 4] {
        let corner = |u: f32, y: f32, uv: [f32; 2]| Vertex {
            position: (axis * (u * hw) + Vec3::Y * y).to_array(),
            uv,
            color: [1.0; 4],
        };
        [
            corner(-1.0, 0.0, [0.0, 1.0]),
            corner(1.0, 0.0, [1.0, 1.0]),
            corner(1.0, h, [1.0, 0.0]),
            corner(-1.0, h, [0.0, 0.0]),
        ]
    };
    let mut vertices = Vec::with_capacity(IMPOSTOR_VERTICES);
    vertices.extend(quad(Vec3::X));
    vertices.extend(quad(Vec3::Z));
    let indices = [0u32, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]
        .into_iter()
        // Double sided: the reverse winding as well.
        .chain([0u32, 2, 1, 0, 3, 2, 4, 6, 5, 4, 7, 6])
        .collect();
    Mesh {
        normals: vec![[0.0, 1.0, 0.0]; IMPOSTOR_VERTICES],
        vertices,
        indices,
    }
}

fn level_slot(level: LodLevel) -> usize {
    match level {
        LodLevel::Mesh(n) => usize::from(n).min(3),
        LodLevel::Impostor | LodLevel::Culled => 4,
    }
}

struct Wanted {
    distance: f32,
    cell: CellKey,
    type_index: u16,
    level: LodLevel,
    cell_revision: u64,
}

/// Brings the cluster entities in line with the camera. Returns whether anything changed.
pub(super) fn update(world: &mut World, cpu: &mut Cpu, input: &Inputs<'_>) {
    let stamp = Stamp {
        types: input.types.to_vec(),
        culling: [input.default_cull, input.lod_scale, input.hysteresis],
        budget: input.budget,
    };
    if cpu.stamp.as_ref() != Some(&stamp) {
        cpu.clear(world);
        cpu.stamp = Some(stamp);
    }
    let Some(camera) = input.camera else {
        cpu.clear(world);
        return;
    };
    let threshold = (input.field.cell_size() * 0.05).max(0.25);
    if cpu
        .last_camera
        .is_some_and(|last| last.distance(camera) < threshold)
        && cpu.facts.len() == input.field.cell_count()
        && input
            .field
            .cells()
            .all(|(k, c)| cpu.facts.get(&k).is_some_and(|f| f.revision == c.revision))
    {
        return;
    }
    cpu.last_camera = Some(camera);
    let type_count = input.types.len();
    let bands: Vec<LodBands> = input
        .types
        .iter()
        .map(|t| LodBands::for_type(t, input.default_cull, input.lod_scale, input.hysteresis))
        .collect();
    cpu.facts.retain(|k, _| input.field.cell(*k).is_some());
    cpu.tracker.retain(|k| input.field.cell(k).is_some());
    let mut wanted: Vec<Wanted> = Vec::new();
    for (key, cell) in input.field.cells() {
        let facts = cpu.facts.entry(key).or_insert_with(|| CellFacts {
            revision: u64::MAX,
            y: [0.0; 2],
            counts: Vec::new(),
        });
        if facts.revision != cell.revision {
            let mut y = [f32::INFINITY, f32::NEG_INFINITY];
            let mut counts = vec![0u32; type_count];
            for i in &cell.instances {
                y = [y[0].min(i.position[1]), y[1].max(i.position[1])];
                if let Some(c) = counts.get_mut(usize::from(i.type_index)) {
                    *c += 1;
                }
            }
            *facts = CellFacts {
                revision: cell.revision,
                y,
                counts,
            };
        }
        let rect = input.field.cell_rect(key);
        let levels = cpu
            .tracker
            .levels_mut(key, cell.revision, type_count.max(1));
        for (t, count) in facts.counts.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            let distance = box_distance(camera, rect, facts.y);
            let previous = LodLevel::from_code(levels[t]);
            let level = bands[t].select(distance, previous);
            levels[t] = level.to_code();
            if level != LodLevel::Culled {
                wanted.push(Wanted {
                    distance,
                    cell: key,
                    type_index: t as u16,
                    level,
                    cell_revision: cell.revision,
                });
            }
        }
    }
    wanted.sort_by(|a, b| {
        a.distance
            .total_cmp(&b.distance)
            .then(a.cell.cmp(&b.cell))
            .then(a.type_index.cmp(&b.type_index))
    });
    let mut remaining = input.budget as usize;
    cpu.budget_limited = false;
    let mut keep: BTreeMap<(CellKey, u16), (LodLevel, u64, usize, usize)> = BTreeMap::new();
    for w in &wanted {
        let t = usize::from(w.type_index);
        let per_instance = match w.level {
            LodLevel::Mesh(n) => input.loaded.data[t].meshes[usize::from(n)].vertices.len(),
            _ => IMPOSTOR_VERTICES,
        }
        .max(1);
        let total = input.field.cell(w.cell).map_or(0, |c| {
            c.instances
                .iter()
                .filter(|i| i.type_index == w.type_index)
                .count()
        });
        let fit = (remaining / per_instance).min(total);
        if fit < total {
            cpu.budget_limited = true;
        }
        if fit == 0 {
            continue;
        }
        remaining -= fit * per_instance;
        keep.insert(
            (w.cell, w.type_index),
            (w.level, w.cell_revision, fit, fit * per_instance),
        );
    }
    // Remove clusters that are gone or stale.
    let stale: Vec<(CellKey, u16)> = cpu
        .clusters
        .iter()
        .filter(|(k, c)| {
            keep.get(k).is_none_or(|(level, revision, count, _)| {
                c.level != *level || c.cell_revision != *revision || c.instances != *count
            })
        })
        .map(|(k, _)| *k)
        .collect();
    for key in stale {
        if let Some(cluster) = cpu.clusters.remove(&key) {
            if keep.contains_key(&key) {
                // Same entity, new geometry: rebuilt below without respawning.
                cpu.clusters.insert(
                    key,
                    Cluster {
                        instances: usize::MAX,
                        ..cluster
                    },
                );
            } else {
                world.despawn(cluster.entity);
            }
        }
    }
    for (&(cell, type_index), &(level, cell_revision, count, vertices)) in &keep {
        if cpu
            .clusters
            .get(&(cell, type_index))
            .is_some_and(|c| c.instances != usize::MAX)
        {
            continue;
        }
        let t = usize::from(type_index);
        let ty = &input.types[t];
        let members: Vec<Instance> = input
            .field
            .cell(cell)
            .map(|c| {
                c.instances
                    .iter()
                    .filter(|i| i.type_index == type_index)
                    .take(count)
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        let (mesh, texture, transparent) = match level {
            LodLevel::Mesh(n) => (
                merge_instances(&input.loaded.data[t].meshes[usize::from(n)], &members),
                ty.texture.clone(),
                false,
            ),
            _ => {
                let size = ty.impostor.as_ref().map_or([1.0, 1.0], |i| i.size);
                (
                    merge_instances(&impostor_mesh(size), &members),
                    ty.impostor.as_ref().map(|i| i.texture.clone()),
                    true,
                )
            }
        };
        let geometry = GeometryOverride(Arc::new(mesh));
        let renderer = MeshRenderer {
            version: 1,
            mesh: String::new(),
            texture,
        };
        let tint = ty.tint.map(|v| v.clamp(0.0, 1.0));
        let material = (transparent || tint != [1.0; 4]).then(|| MeshMaterial {
            tint,
            transparent,
            ..MeshMaterial::default()
        });
        let entity = match cpu.clusters.get(&(cell, type_index)) {
            Some(existing) => {
                let entity = existing.entity;
                let mut e = world.entity_mut(entity);
                e.insert((geometry, renderer));
                match material {
                    Some(m) => {
                        e.insert(m);
                    }
                    None => {
                        e.remove::<MeshMaterial>();
                    }
                }
                entity
            }
            None => {
                let name = format!(
                    "foliage:{}/cell/{},{}/{}",
                    input.owner_id, cell.x, cell.z, ty.name
                );
                let mut spawned = world.spawn((
                    AuthoredId {
                        id: name.clone(),
                        name,
                    },
                    Transform::default(),
                    renderer,
                    geometry,
                    FoliageCluster {
                        owner: input.owner,
                        cell,
                        type_index,
                    },
                    FoliageGenerated { owner: input.owner },
                ));
                if let Some(m) = material {
                    spawned.insert(m);
                }
                spawned.id()
            }
        };
        cpu.clusters.insert(
            (cell, type_index),
            Cluster {
                entity,
                level,
                cell_revision,
                instances: count,
                vertices,
            },
        );
    }
    cpu.vertices = cpu.clusters.values().map(|c| c.vertices).sum();
    cpu.levels = [0; 5];
    for c in cpu.clusters.values() {
        cpu.levels[level_slot(c.level)] += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle() -> Mesh {
        Mesh {
            normals: vec![[0.0, 0.0, 1.0]; 3],
            vertices: vec![
                Vertex {
                    position: [0.0, 0.0, 0.0],
                    uv: [0.0, 0.0],
                    color: [1.0; 4],
                },
                Vertex {
                    position: [1.0, 0.0, 0.0],
                    uv: [1.0, 0.0],
                    color: [1.0; 4],
                },
                Vertex {
                    position: [0.0, 1.0, 0.0],
                    uv: [0.0, 1.0],
                    color: [1.0; 4],
                },
            ],
            indices: vec![0, 1, 2],
        }
    }

    #[test]
    fn merging_transforms_geometry_and_offsets_indices() {
        let mut a = Instance::new(0, [10.0, 0.0, 0.0]);
        a.scale = [2.0; 3];
        let b = Instance::new(0, [0.0, 5.0, 0.0]);
        let merged = merge_instances(&triangle(), &[a, b]);
        assert_eq!(merged.vertices.len(), 6);
        assert_eq!(merged.indices, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(merged.vertices[1].position, [12.0, 0.0, 0.0]);
        assert_eq!(merged.vertices[5].position, [0.0, 6.0, 0.0]);
        assert_eq!(merged.normals.len(), 6);
        assert!((Vec3::from(merged.normals[0]) - Vec3::Z).length() < 1e-6);
    }

    #[test]
    fn impostor_is_two_double_sided_quads_standing_on_the_origin() {
        let m = impostor_mesh([2.0, 4.0]);
        assert_eq!(m.vertices.len(), IMPOSTOR_VERTICES);
        assert_eq!(m.indices.len(), 24);
        assert!(
            m.vertices
                .iter()
                .all(|v| (0.0..=4.0).contains(&v.position[1]))
        );
        let width = m
            .vertices
            .iter()
            .map(|v| v.position[0].abs())
            .fold(0.0, f32::max);
        assert_eq!(width, 1.0);
    }

    #[test]
    fn box_distance_is_zero_inside_and_euclidean_outside() {
        let rect = [[0.0, 0.0], [10.0, 10.0]];
        assert_eq!(
            box_distance(Vec3::new(5.0, 1.0, 5.0), rect, [0.0, 2.0]),
            0.0
        );
        let d = box_distance(Vec3::new(13.0, 0.0, 14.0), rect, [0.0, 0.0]);
        assert!((d - 5.0).abs() < 1e-5);
        assert!((box_distance(Vec3::new(5.0, 7.0, 5.0), rect, [0.0, 2.0]) - 5.0).abs() < 1e-5);
    }
}
