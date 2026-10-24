//! The two-level acoustic triangle world: per-mesh BVHs (BLAS) plus an instance-level
//! BVH (TLAS), flattened into the `gpu_layout` arrays that the CPU traverses and the
//! GPU tiers upload unchanged.

use crate::bvh::{self, BuildError};
use crate::math::{self, Aabb, Vec3};
use crate::planes::{self, PlaneCluster};
use crate::volume::{RoomInfo, RoomSeed};
use crate::{AcousticMaterial, AcousticsError, BandGains, MaterialId, ProxyMapping, Result};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MeshId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstanceId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mobility {
    Static,
    Dynamic,
}
/// Row-major 3x4 affine transform, the layout of `wgpu::TlasInstance::transform`.
pub type Affine3 = [f32; 12];

/// `materials` has length 1 (uniform) or `triangles.len()`.
#[derive(Clone, Debug, PartialEq)]
pub struct AcousticMesh {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub materials: Vec<MaterialId>,
}

/// Triangles with an area below this (m^2, object space) are dropped and counted.
pub const DEGENERATE_AREA_M2: f32 = 1e-10;

/// Build limits (section 5.3). The default is the documented limits; a caller may
/// tighten them, and only root may loosen the defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldLimits {
    pub max_mesh_triangles: u32,
    pub max_static_triangles: u32,
    pub max_dynamic_triangles: u32,
    pub max_instances: u32,
    pub max_dynamic_instances: u32,
    pub max_materials: u32,
    pub max_volumes: u32,
    pub max_proxy_footprints: u32,
    pub max_footprint_vertices: u32,
    pub max_coordinate_m: u32,
    pub build_work_budget: u64,
    pub max_plane_clusters: u32,
}
impl Default for WorldLimits {
    fn default() -> Self {
        Self {
            max_mesh_triangles: 262_144,
            max_static_triangles: 524_288,
            max_dynamic_triangles: 65_536,
            max_instances: 16_384,
            max_dynamic_instances: 1_024,
            max_materials: 1_024,
            max_volumes: 4_096,
            max_proxy_footprints: 8_192,
            max_footprint_vertices: 32,
            max_coordinate_m: 100_000,
            build_work_budget: 64_000_000,
            max_plane_clusters: 65_536,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct WorldStats {
    /// Triangles over all instances (a mesh instanced twice counts twice).
    pub triangles: u32,
    pub instances: u32,
    pub materials: u32,
    pub dropped_degenerate: u32,
    /// Mesh-level plus instance-level nodes.
    pub bvh_nodes: u32,
    pub plane_clusters: u32,
    /// `[min, max]` in acoustic metres (zero for an empty world).
    pub bounds: [[f32; 3]; 2],
}

pub(crate) fn limit(message: impl Into<String>) -> AcousticsError {
    AcousticsError::new("acoustics.world.limit", message)
}
pub(crate) fn invalid(message: impl Into<String>) -> AcousticsError {
    AcousticsError::new("acoustics.world.invalid", message)
}

#[derive(Clone, Debug)]
pub(crate) struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    /// One per kept triangle.
    pub materials: Vec<MaterialId>,
    /// Original triangle index of each kept triangle.
    pub source: Vec<u32>,
    pub bounds: Aabb,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct InstanceData {
    pub mesh: u32,
    pub transform: Affine3,
    pub inverse: Affine3,
    pub mobility: Mobility,
    pub mask: u8,
}

static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

/// Validating builder. Every `add_*` call checks its input and the limits before it
/// changes anything, so a rejected call leaves the builder as it was. Materials
/// must be added before the meshes, volumes and footprints that use them.
#[derive(Clone, Debug)]
pub struct AcousticWorldBuilder {
    limits: WorldLimits,
    revision: Option<u64>,
    pub(crate) materials: Vec<AcousticMaterial>,
    pub(crate) meshes: Vec<MeshData>,
    pub(crate) instances: Vec<InstanceData>,
    pub(crate) rooms: Vec<RoomSeed>,
    pub(crate) proxy: Option<ProxyMapping>,
    pub(crate) volumes: u32,
    dropped_degenerate: u32,
    static_triangles: u64,
    dynamic_triangles: u64,
    dynamic_instances: u32,
}
impl AcousticWorldBuilder {
    pub fn new(limits: WorldLimits) -> Self {
        Self {
            limits,
            revision: None,
            materials: Vec::new(),
            meshes: Vec::new(),
            instances: Vec::new(),
            rooms: Vec::new(),
            proxy: None,
            volumes: 0,
            dropped_degenerate: 0,
            static_triangles: 0,
            dynamic_triangles: 0,
            dynamic_instances: 0,
        }
    }
    pub fn limits(&self) -> WorldLimits {
        self.limits
    }
    /// Pins the revision of the built world (for reproducible pipelines that count
    /// rebuilds themselves). Without it, `build` takes the next value of a
    /// process-wide counter, so every build has a new, larger revision.
    pub fn with_revision(mut self, revision: u64) -> Self {
        self.revision = Some(revision);
        self
    }

    pub fn add_material(&mut self, m: AcousticMaterial) -> Result<MaterialId> {
        m.validate()?;
        let max = self.limits.max_materials.min(65_536) as usize;
        if self.materials.len() >= max {
            return Err(limit(format!("a world holds at most {max} materials")));
        }
        self.materials.push(m);
        Ok(MaterialId((self.materials.len() - 1) as u16))
    }

    pub(crate) fn check_material(&self, id: MaterialId) -> Result<()> {
        if usize::from(id.0) < self.materials.len() {
            Ok(())
        } else {
            Err(AcousticsError::new(
                "acoustics.material.invalid",
                format!("material {} has not been added", id.0),
            ))
        }
    }
    pub(crate) fn check_coordinate(&self, p: Vec3, what: &str) -> Result<()> {
        let max = self.limits.max_coordinate_m as f32;
        if !math::finite3(p) {
            return Err(invalid(format!("{what} has a non-finite coordinate")));
        }
        if math::max_abs(p) > max {
            return Err(limit(format!(
                "{what} exceeds the {max} m coordinate limit"
            )));
        }
        Ok(())
    }

    /// Validates a mesh and drops degenerate triangles (counted in
    /// `WorldStats::dropped_degenerate`).
    pub fn add_mesh(&mut self, mesh: AcousticMesh) -> Result<MeshId> {
        let (data, dropped) = self.validate_mesh(mesh)?;
        Ok(self.push_mesh(data, dropped))
    }
    pub(crate) fn push_mesh(&mut self, data: MeshData, dropped: u32) -> MeshId {
        self.dropped_degenerate += dropped;
        self.meshes.push(data);
        MeshId((self.meshes.len() - 1) as u32)
    }

    pub(crate) fn validate_mesh(&self, mesh: AcousticMesh) -> Result<(MeshData, u32)> {
        if mesh.triangles.len() as u64 > u64::from(self.limits.max_mesh_triangles) {
            return Err(limit(format!(
                "mesh has {} triangles; the limit is {}",
                mesh.triangles.len(),
                self.limits.max_mesh_triangles
            )));
        }
        if mesh.positions.len() >= u32::MAX as usize {
            return Err(limit("mesh has too many positions"));
        }
        if mesh.materials.len() != 1 && mesh.materials.len() != mesh.triangles.len() {
            return Err(invalid(
                "mesh materials must have length 1 or one per triangle",
            ));
        }
        for &m in &mesh.materials {
            self.check_material(m)?;
        }
        for &p in &mesh.positions {
            self.check_coordinate(p, "mesh position")?;
        }
        let mut kept = MeshData {
            positions: Vec::new(),
            triangles: Vec::with_capacity(mesh.triangles.len()),
            materials: Vec::with_capacity(mesh.triangles.len()),
            source: Vec::with_capacity(mesh.triangles.len()),
            bounds: Aabb::EMPTY,
        };
        let mut dropped = 0u32;
        for (i, tri) in mesh.triangles.iter().enumerate() {
            if tri.iter().any(|&v| v as usize >= mesh.positions.len()) {
                return Err(invalid(format!("triangle {i} indexes a missing position")));
            }
            let [a, b, c] = tri.map(|v| mesh.positions[v as usize]);
            let area = 0.5 * math::length(math::cross(math::sub(b, a), math::sub(c, a)));
            if area.is_nan() || area < DEGENERATE_AREA_M2 {
                dropped += 1;
                continue;
            }
            for p in [a, b, c] {
                kept.bounds.grow(p);
            }
            kept.triangles.push(*tri);
            kept.materials.push(if mesh.materials.len() == 1 {
                mesh.materials[0]
            } else {
                mesh.materials[i]
            });
            kept.source.push(i as u32);
        }
        kept.positions = mesh.positions;
        Ok((kept, dropped))
    }

    pub fn add_instance(
        &mut self,
        mesh: MeshId,
        transform: Affine3,
        mobility: Mobility,
        mask: u8,
    ) -> Result<InstanceId> {
        let data = self.validate_instance(mesh, transform, mobility, mask)?;
        Ok(self.push_instance(data))
    }

    pub(crate) fn validate_instance(
        &self,
        mesh: MeshId,
        transform: Affine3,
        mobility: Mobility,
        mask: u8,
    ) -> Result<InstanceData> {
        let Some(m) = self.meshes.get(mesh.0 as usize) else {
            return Err(invalid(format!("mesh {} does not exist", mesh.0)));
        };
        self.check_instance_room(1, m.triangles.len() as u64, mobility)?;
        let inverse =
            validate_transform(&transform, &m.bounds, self.limits.max_coordinate_m as f32)?;
        Ok(InstanceData {
            mesh: mesh.0,
            transform,
            inverse,
            mobility,
            mask,
        })
    }

    /// Checks that `count` more instances with `triangles` triangles in total fit.
    pub(crate) fn check_instance_room(
        &self,
        count: u64,
        triangles: u64,
        mobility: Mobility,
    ) -> Result<()> {
        if self.instances.len() as u64 + count > u64::from(self.limits.max_instances) {
            return Err(limit(format!(
                "a world holds at most {} instances",
                self.limits.max_instances
            )));
        }
        match mobility {
            Mobility::Static => {
                if self.static_triangles + triangles > u64::from(self.limits.max_static_triangles) {
                    return Err(limit(format!(
                        "static triangles would exceed {}",
                        self.limits.max_static_triangles
                    )));
                }
            }
            Mobility::Dynamic => {
                if u64::from(self.dynamic_instances) + count
                    > u64::from(self.limits.max_dynamic_instances)
                {
                    return Err(limit(format!(
                        "a world holds at most {} dynamic instances",
                        self.limits.max_dynamic_instances
                    )));
                }
                if self.dynamic_triangles + triangles > u64::from(self.limits.max_dynamic_triangles)
                {
                    return Err(limit(format!(
                        "dynamic triangles would exceed {}",
                        self.limits.max_dynamic_triangles
                    )));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn push_instance(&mut self, data: InstanceData) -> InstanceId {
        let tris = self.meshes[data.mesh as usize].triangles.len() as u64;
        match data.mobility {
            Mobility::Static => self.static_triangles += tris,
            Mobility::Dynamic => {
                self.dynamic_triangles += tris;
                self.dynamic_instances += 1;
            }
        }
        self.instances.push(data);
        InstanceId((self.instances.len() - 1) as u32)
    }

    /// Builds the BVHs, flattens the GPU layout and discovers plane clusters. On any
    /// error no world is produced.
    pub fn build(self) -> Result<AcousticWorld> {
        let budget = self.limits.build_work_budget;
        let mut work = 0u64;
        let build_error = |e: BuildError| match e {
            BuildError::Budget => AcousticsError::new(
                "acoustics.world.budget",
                format!("BVH build exceeded the {budget} operation budget"),
            ),
            BuildError::Depth => limit(format!(
                "BVH deeper than {} levels; simplify or split the geometry",
                bvh::MAX_BVH_DEPTH
            )),
        };
        let mut nodes = Vec::new();
        let mut triangles = Vec::new();
        let mut triangle_source = Vec::new();
        let mut mesh_records = Vec::with_capacity(self.meshes.len());
        let mut order = Vec::new();
        for mesh in &self.meshes {
            let triangle_offset = triangles.len() as u32;
            let node_offset = nodes.len() as u32;
            let bounds: Vec<Aabb> = mesh
                .triangles
                .iter()
                .map(|t| {
                    let mut b = Aabb::EMPTY;
                    for &v in t {
                        b.grow(mesh.positions[v as usize]);
                    }
                    b
                })
                .collect();
            bvh::build(
                &bounds,
                bvh::MESH_MAX_LEAF,
                triangle_offset,
                &mut nodes,
                &mut order,
                &mut work,
                budget,
            )
            .map_err(build_error)?;
            for &i in &order {
                let [a, b, c] = mesh.triangles[i as usize].map(|v| mesh.positions[v as usize]);
                let material = u32::from(mesh.materials[i as usize].0);
                triangles.push(gpu_layout::GpuTriangle {
                    v0: [a[0], a[1], a[2], f32::from_bits(material)],
                    v1: [b[0], b[1], b[2], f32::from_bits(mesh.source[i as usize])],
                    v2: [c[0], c[1], c[2], 0.0],
                });
                triangle_source.push(mesh.source[i as usize]);
            }
            mesh_records.push(MeshRecord {
                node_offset,
                triangle_offset,
                triangle_count: mesh.triangles.len() as u32,
                bounds: mesh.bounds,
            });
        }

        let mut instances = Vec::with_capacity(self.instances.len());
        let mut records = Vec::with_capacity(self.instances.len());
        for inst in &self.instances {
            let mesh = &mesh_records[inst.mesh as usize];
            let empty = mesh.triangle_count == 0;
            instances.push(gpu_layout::GpuInstance {
                world_from_object: inst.transform,
                object_from_world: inst.inverse,
                node_offset: if empty { u32::MAX } else { mesh.node_offset },
                triangle_offset: mesh.triangle_offset,
                mask: u32::from(inst.mask),
                _pad: 0,
            });
            records.push(InstanceRecord {
                mesh: inst.mesh,
                mobility: inst.mobility,
                world_box: mesh.bounds.transformed(&inst.transform),
            });
        }

        let (top_nodes, top_members) =
            build_top_level(&records, &mut work, budget).map_err(build_error)?;

        let gpu_materials = self
            .materials
            .iter()
            .map(|m| gpu_layout::GpuMaterial {
                reflect: m.absorption.map(|a| 1.0 - a),
                transmission: m.transmission,
                scattering: m.scattering,
                _pad: [0.0; 3],
            })
            .collect();

        let mut world = AcousticWorld {
            revision: self
                .revision
                .unwrap_or_else(|| NEXT_REVISION.fetch_add(1, Ordering::Relaxed)),
            dynamic_generation: 0,
            limits: self.limits,
            materials: self.materials,
            gpu_materials,
            meshes: mesh_records,
            nodes,
            top_nodes,
            top_members,
            triangles,
            triangle_source,
            instances,
            records,
            clusters: Vec::new(),
            cluster_offsets: Vec::new(),
            tri_cluster: Vec::new(),
            rooms: Vec::new(),
            proxy: self.proxy,
            dropped_degenerate: self.dropped_degenerate,
        };
        let found = planes::discover(&world, self.limits.max_plane_clusters, &mut work, budget)?;
        world.clusters = found.clusters;
        world.cluster_offsets = found.offsets;
        world.tri_cluster = found.tri_cluster;
        world.rooms = self
            .rooms
            .iter()
            .map(|seed| RoomInfo::from_seed(seed, &world))
            .collect();
        Ok(world)
    }
}

pub(crate) fn validate_transform(
    t: &Affine3,
    local: &Aabb,
    max_coordinate: f32,
) -> Result<Affine3> {
    if t.iter().any(|v| !v.is_finite()) {
        return Err(invalid("transform has a non-finite value"));
    }
    let inverse = math::invert(t).ok_or_else(|| invalid("transform is singular"))?;
    if math::max_abs([t[3], t[7], t[11]]) > max_coordinate {
        return Err(limit(format!(
            "instance translation exceeds the {max_coordinate} m coordinate limit"
        )));
    }
    let world = local.transformed(t);
    if !world.is_empty()
        && (math::max_abs(world.min) > max_coordinate || math::max_abs(world.max) > max_coordinate)
    {
        return Err(limit(format!(
            "instance bounds exceed the {max_coordinate} m coordinate limit"
        )));
    }
    Ok(inverse)
}

/// Builds the instance-level tree over instances with non-empty meshes. Leaves hold
/// exactly one instance and store its index.
fn build_top_level(
    records: &[InstanceRecord],
    work: &mut u64,
    budget: u64,
) -> std::result::Result<(Vec<gpu_layout::BvhNode>, Vec<u32>), BuildError> {
    let members: Vec<u32> = (0..records.len() as u32)
        .filter(|&i| !records[i as usize].world_box.is_empty())
        .collect();
    let boxes: Vec<Aabb> = members
        .iter()
        .map(|&i| records[i as usize].world_box)
        .collect();
    let mut nodes = Vec::new();
    let mut order = Vec::new();
    bvh::build(&boxes, 1, 0, &mut nodes, &mut order, work, budget)?;
    for node in &mut nodes {
        if node.count > 0 {
            node.left_or_first = members[order[node.left_or_first as usize] as usize];
        }
    }
    Ok((nodes, members))
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MeshRecord {
    pub node_offset: u32,
    pub triangle_offset: u32,
    pub triangle_count: u32,
    pub bounds: Aabb,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct InstanceRecord {
    pub mesh: u32,
    pub mobility: Mobility,
    pub world_box: Aabb,
}

/// A nearest hit before it is resolved into the public [`Hit`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RawHit {
    pub t: f32,
    pub instance: u32,
    /// Index into `GpuWorldView::triangles`.
    pub triangle: u32,
}

/// Built world (structure of arrays). Clone it to give a copy to another thread.
#[derive(Clone, Debug)]
pub struct AcousticWorld {
    revision: u64,
    dynamic_generation: u64,
    limits: WorldLimits,
    materials: Vec<AcousticMaterial>,
    gpu_materials: Vec<gpu_layout::GpuMaterial>,
    pub(crate) meshes: Vec<MeshRecord>,
    nodes: Vec<gpu_layout::BvhNode>,
    top_nodes: Vec<gpu_layout::BvhNode>,
    top_members: Vec<u32>,
    pub(crate) triangles: Vec<gpu_layout::GpuTriangle>,
    triangle_source: Vec<u32>,
    pub(crate) instances: Vec<gpu_layout::GpuInstance>,
    pub(crate) records: Vec<InstanceRecord>,
    clusters: Vec<PlaneCluster>,
    cluster_offsets: Vec<u32>,
    tri_cluster: Vec<u32>,
    rooms: Vec<RoomInfo>,
    proxy: Option<ProxyMapping>,
    dropped_degenerate: u32,
}

/// Absolute part of the offset a ray takes when it leaves a surface (metres).
/// Shared with the GPU tracer.
pub const RAY_OFFSET_ABS_M: f32 = 1e-4;
/// Relative part of the surface offset (about 32 ulp of `f32`).
pub const RAY_OFFSET_REL: f32 = 4e-6;

/// Offset along the outgoing side's normal for a ray leaving a surface at `p`:
/// `RAY_OFFSET_ABS_M + RAY_OFFSET_REL * max(|p.x|, |p.y|, |p.z|)`.
#[inline]
pub fn ray_offset(p: [f32; 3]) -> f32 {
    RAY_OFFSET_ABS_M + RAY_OFFSET_REL * math::max_abs(p)
}

/// Fixed traversal stack size per tree (see `bvh::MAX_BVH_DEPTH`).
const STACK: usize = 64;

impl AcousticWorld {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Incremented by every `set_instance_transform`; backends compare it to detect
    /// moved dynamic instances without a rebuild.
    pub fn dynamic_generation(&self) -> u64 {
        self.dynamic_generation
    }
    pub fn limits(&self) -> WorldLimits {
        self.limits
    }
    pub fn stats(&self) -> WorldStats {
        let mut bounds = Aabb::EMPTY;
        let mut triangles = 0u64;
        for r in &self.records {
            triangles += u64::from(self.meshes[r.mesh as usize].triangle_count);
            if !r.world_box.is_empty() {
                bounds = bounds.union(&r.world_box);
            }
        }
        WorldStats {
            triangles: triangles.min(u64::from(u32::MAX)) as u32,
            instances: self.instances.len() as u32,
            materials: self.materials.len() as u32,
            dropped_degenerate: self.dropped_degenerate,
            bvh_nodes: (self.nodes.len() + self.top_nodes.len()) as u32,
            plane_clusters: self.clusters.len() as u32,
            bounds: if bounds.is_empty() {
                [[0.0; 3]; 2]
            } else {
                [bounds.min, bounds.max]
            },
        }
    }
    pub fn materials(&self) -> &[AcousticMaterial] {
        &self.materials
    }
    pub fn material(&self, id: MaterialId) -> Option<&AcousticMaterial> {
        self.materials.get(usize::from(id.0))
    }
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }
    pub fn instance_mobility(&self, id: InstanceId) -> Option<Mobility> {
        self.records.get(id.0 as usize).map(|r| r.mobility)
    }
    /// Ids of all dynamic instances, in order.
    pub fn dynamic_instances(&self) -> impl Iterator<Item = InstanceId> + '_ {
        self.records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.mobility == Mobility::Dynamic)
            .map(|(i, _)| InstanceId(i as u32))
    }
    /// Coplanar static surfaces used by the image-source method.
    pub fn plane_clusters(&self) -> &[PlaneCluster] {
        &self.clusters
    }
    /// Analytic rooms from `VolumeKind::Room` box volumes.
    pub fn rooms(&self) -> &[RoomInfo] {
        &self.rooms
    }
    /// The first room volume containing `p`, if any.
    pub fn room_at(&self, p: [f32; 3]) -> Option<&RoomInfo> {
        self.rooms.iter().find(|r| r.contains(self, p))
    }
    /// The 2D proxy mapping when the world was built with `add_proxy_2d`.
    pub fn proxy(&self) -> Option<&ProxyMapping> {
        self.proxy.as_ref()
    }

    /// Moves a dynamic instance and refits the instance-level BVH. The tree topology
    /// is kept, so large motions only cost traversal efficiency, never correctness.
    pub fn set_instance_transform(&mut self, id: InstanceId, t: Affine3) -> Result<()> {
        let Some(record) = self.records.get(id.0 as usize).copied() else {
            return Err(invalid(format!("instance {} does not exist", id.0)));
        };
        if record.mobility != Mobility::Dynamic {
            return Err(invalid(format!(
                "instance {} is static; only dynamic instances move",
                id.0
            )));
        }
        let mesh = self.meshes[record.mesh as usize];
        let inverse = validate_transform(&t, &mesh.bounds, self.limits.max_coordinate_m as f32)?;
        let inst = &mut self.instances[id.0 as usize];
        inst.world_from_object = t;
        inst.object_from_world = inverse;
        self.records[id.0 as usize].world_box = mesh.bounds.transformed(&t);
        let records = &self.records;
        bvh::refit(&mut self.top_nodes, |leaf| {
            records[leaf.left_or_first as usize].world_box
        });
        self.dynamic_generation += 1;
        Ok(())
    }

    /// Nearest hit along `origin + t * dir` for `t` in `(0, t_max]`, considering
    /// instances whose mask shares a bit with `mask`. `t` is in units of `dir`'s
    /// length. Returns `None` for zero or non-finite input.
    pub fn raycast(&self, origin: [f32; 3], dir: [f32; 3], t_max: f32, mask: u8) -> Option<Hit> {
        if !math::finite3(origin) || !math::finite3(dir) || dir == [0.0; 3] || t_max.is_nan() {
            return None;
        }
        self.nearest(origin, dir, 0.0, t_max, mask)
            .map(|h| self.resolve(h, dir))
    }

    /// Nearest hit for `t` in `(t_min, t_max]`. Among equal `t`, the lowest
    /// `(instance, triangle)` wins, so the result does not depend on traversal order.
    pub(crate) fn nearest(
        &self,
        origin: Vec3,
        dir: Vec3,
        t_min: f32,
        t_max: f32,
        mask: u8,
    ) -> Option<RawHit> {
        if self.top_nodes.is_empty() {
            return None;
        }
        let inv = dir.map(|d| 1.0 / d);
        let mut best: Option<RawHit> = None;
        let mut best_t = t_max;
        let mut stack = [0u32; STACK];
        let mut sp = 1usize;
        let mut mesh_stack = [0u32; STACK];
        while sp > 0 {
            sp -= 1;
            let node = self.top_nodes[stack[sp] as usize];
            if math::ray_aabb(node.min, node.max, origin, inv, t_min, best_t).is_none() {
                continue;
            }
            if node.count > 0 {
                let index = node.left_or_first;
                let inst = &self.instances[index as usize];
                if inst.mask & u32::from(mask) == 0 || inst.node_offset == u32::MAX {
                    continue;
                }
                let o = math::transform_point(&inst.object_from_world, origin);
                let d = math::transform_vector(&inst.object_from_world, dir);
                let mut walk = MeshWalk {
                    instance: index,
                    t_min,
                    best_t: &mut best_t,
                    best: &mut best,
                };
                self.trace_mesh(inst.node_offset, o, d, &mut walk, &mut mesh_stack);
                continue;
            }
            let (l, r) = (node.left_or_first as usize, node.left_or_first as usize + 1);
            let tl = math::ray_aabb(
                self.top_nodes[l].min,
                self.top_nodes[l].max,
                origin,
                inv,
                t_min,
                best_t,
            );
            let tr = math::ray_aabb(
                self.top_nodes[r].min,
                self.top_nodes[r].max,
                origin,
                inv,
                t_min,
                best_t,
            );
            push_ordered(&mut stack, &mut sp, (l as u32, tl), (r as u32, tr));
        }
        best
    }

    fn trace_mesh(
        &self,
        root: u32,
        o: Vec3,
        d: Vec3,
        walk: &mut MeshWalk<'_>,
        stack: &mut [u32; STACK],
    ) {
        let inv = d.map(|v| 1.0 / v);
        let mut sp = 1usize;
        stack[0] = root;
        while sp > 0 {
            sp -= 1;
            let node = self.nodes[stack[sp] as usize];
            if math::ray_aabb(node.min, node.max, o, inv, walk.t_min, *walk.best_t).is_none() {
                continue;
            }
            if node.count > 0 {
                for tri in node.left_or_first..node.left_or_first + node.count {
                    let g = &self.triangles[tri as usize];
                    let hit = math::ray_triangle(
                        o,
                        d,
                        [g.v0[0], g.v0[1], g.v0[2]],
                        [g.v1[0], g.v1[1], g.v1[2]],
                        [g.v2[0], g.v2[1], g.v2[2]],
                        walk.t_min,
                        *walk.best_t,
                    );
                    if let Some(t) = hit {
                        walk.offer(t, tri);
                    }
                }
                continue;
            }
            let (l, r) = (node.left_or_first as usize, node.left_or_first as usize + 1);
            let tl = math::ray_aabb(
                self.nodes[l].min,
                self.nodes[l].max,
                o,
                inv,
                walk.t_min,
                *walk.best_t,
            );
            let tr = math::ray_aabb(
                self.nodes[r].min,
                self.nodes[r].max,
                o,
                inv,
                walk.t_min,
                *walk.best_t,
            );
            push_ordered(stack, &mut sp, (l as u32, tl), (r as u32, tr));
        }
    }

    /// World-space unit normal of a hit, facing against `dir`.
    pub(crate) fn hit_normal(&self, hit: &RawHit, dir: Vec3) -> Vec3 {
        let inst = &self.instances[hit.instance as usize];
        let g = &self.triangles[hit.triangle as usize];
        let v0 = [g.v0[0], g.v0[1], g.v0[2]];
        let e1 = math::sub([g.v1[0], g.v1[1], g.v1[2]], v0);
        let e2 = math::sub([g.v2[0], g.v2[1], g.v2[2]], v0);
        let n = math::transform_normal(&inst.object_from_world, math::cross(e1, e2));
        let n = math::normalize(n).unwrap_or([0.0, 1.0, 0.0]);
        if math::dot(n, dir) > 0.0 {
            math::scale(n, -1.0)
        } else {
            n
        }
    }
    pub(crate) fn hit_material(&self, hit: &RawHit) -> MaterialId {
        MaterialId(self.triangles[hit.triangle as usize].v0[3].to_bits() as u16)
    }
    #[allow(dead_code)] // Used by the tracers.
    pub(crate) fn gpu_material(&self, id: MaterialId) -> &gpu_layout::GpuMaterial {
        &self.gpu_materials[usize::from(id.0)]
    }
    #[allow(dead_code)] // Used by the tracers.
    pub(crate) fn transmission(&self, id: MaterialId) -> BandGains {
        self.materials[usize::from(id.0)].transmission
    }
    /// Plane cluster of a hit, when it is on a static instance.
    #[allow(dead_code)] // Used by the tracers.
    pub(crate) fn hit_cluster(&self, hit: &RawHit) -> Option<u32> {
        let offset = *self.cluster_offsets.get(hit.instance as usize)?;
        if offset == u32::MAX {
            return None;
        }
        let local = hit.triangle - self.instances[hit.instance as usize].triangle_offset;
        self.tri_cluster.get((offset + local) as usize).copied()
    }
    fn resolve(&self, hit: RawHit, dir: Vec3) -> Hit {
        Hit {
            t: hit.t,
            instance: InstanceId(hit.instance),
            triangle: self.triangle_source[hit.triangle as usize],
            material: self.hit_material(&hit),
            normal: self.hit_normal(&hit, dir),
        }
    }

    /// Reference nearest hit that tests every triangle of every instance (no BVH),
    /// with the same intersection routine and tie-break as [`Self::raycast`]. For
    /// validation and parity tests.
    pub fn raycast_brute_force(
        &self,
        origin: [f32; 3],
        dir: [f32; 3],
        t_max: f32,
        mask: u8,
    ) -> Option<Hit> {
        if !math::finite3(origin) || !math::finite3(dir) || dir == [0.0; 3] || t_max.is_nan() {
            return None;
        }
        let mut best: Option<RawHit> = None;
        let mut best_t = t_max;
        for (i, inst) in self.instances.iter().enumerate() {
            if inst.mask & u32::from(mask) == 0 || inst.node_offset == u32::MAX {
                continue;
            }
            let mesh = &self.meshes[self.records[i].mesh as usize];
            let o = math::transform_point(&inst.object_from_world, origin);
            let d = math::transform_vector(&inst.object_from_world, dir);
            let mut walk = MeshWalk {
                instance: i as u32,
                t_min: 0.0,
                best_t: &mut best_t,
                best: &mut best,
            };
            for tri in mesh.triangle_offset..mesh.triangle_offset + mesh.triangle_count {
                let g = &self.triangles[tri as usize];
                let hit = math::ray_triangle(
                    o,
                    d,
                    [g.v0[0], g.v0[1], g.v0[2]],
                    [g.v1[0], g.v1[1], g.v1[2]],
                    [g.v2[0], g.v2[1], g.v2[2]],
                    0.0,
                    *walk.best_t,
                );
                if let Some(t) = hit {
                    walk.offer(t, tri);
                }
            }
        }
        best.map(|h| self.resolve(h, dir))
    }

    /// Borrowed slices of the flat layouts (conventions in [`gpu_layout`]).
    pub fn gpu_view(&self) -> GpuWorldView<'_> {
        GpuWorldView {
            nodes: &self.nodes,
            top_nodes: &self.top_nodes,
            triangles: &self.triangles,
            instances: &self.instances,
            materials: &self.gpu_materials,
        }
    }
    /// Instances referenced by the instance-level BVH (those with triangles).
    pub fn top_level_instances(&self) -> &[u32] {
        &self.top_members
    }
}

struct MeshWalk<'a> {
    instance: u32,
    t_min: f32,
    best_t: &'a mut f32,
    best: &'a mut Option<RawHit>,
}
impl MeshWalk<'_> {
    #[inline]
    fn offer(&mut self, t: f32, triangle: u32) {
        let better = match self.best {
            None => true,
            Some(b) => {
                t < b.t || (t == b.t && (self.instance, triangle) < (b.instance, b.triangle))
            }
        };
        if better {
            *self.best_t = t;
            *self.best = Some(RawHit {
                t,
                instance: self.instance,
                triangle,
            });
        }
    }
}

/// Pushes the children that the ray enters, farther first so the nearer is popped
/// next.
#[inline]
fn push_ordered(
    stack: &mut [u32; STACK],
    sp: &mut usize,
    (l, tl): (u32, Option<f32>),
    (r, tr): (u32, Option<f32>),
) {
    match (tl, tr) {
        (Some(a), Some(b)) => {
            let (near, far) = if b < a { (r, l) } else { (l, r) };
            stack[*sp] = far;
            stack[*sp + 1] = near;
            *sp += 2;
        }
        (Some(_), None) => {
            stack[*sp] = l;
            *sp += 1;
        }
        (None, Some(_)) => {
            stack[*sp] = r;
            *sp += 1;
        }
        (None, None) => {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub t: f32,
    pub instance: InstanceId,
    /// Index of the triangle in the mesh as it was added (before degenerate
    /// triangles were dropped and the BVH reordered them).
    pub triangle: u32,
    pub material: MaterialId,
    /// Unit world-space normal facing the incoming ray.
    pub normal: [f32; 3],
}

/// Borrowed slices of the flat layouts shared by CPU traversal and WGSL.
/// `top_nodes` is the instance-level BVH; `nodes` holds all per-mesh BVHs.
#[derive(Clone, Copy, Debug)]
pub struct GpuWorldView<'a> {
    pub nodes: &'a [gpu_layout::BvhNode],
    pub top_nodes: &'a [gpu_layout::BvhNode],
    pub triangles: &'a [gpu_layout::GpuTriangle],
    pub instances: &'a [gpu_layout::GpuInstance],
    pub materials: &'a [gpu_layout::GpuMaterial],
}

/// `#[repr(C)]` Pod layouts, identical for CPU traversal and WGSL.
///
/// Conventions beyond the field comments:
///
/// - `BvhNode`: children are absolute indices into the same array (`nodes` or
///   `top_nodes`) and always follow their parent. Mesh leaves have
///   `count` in `1..=4` and `left_or_first` indexes `triangles` absolutely.
///   Instance-level leaves have `count == 1` and `left_or_first` is the instance
///   index (`InstanceId`). The root of `top_nodes` is index 0; an empty world has no
///   `top_nodes`. Trees are at most `MAX_BVH_DEPTH` (48) levels deep, so a
///   depth-first walk pushing both children needs at most 49 entries per tree.
/// - `GpuTriangle`: object-space vertices of one mesh, in that mesh's leaf order
///   starting at `GpuInstance::triangle_offset`. `v0[3]` is the material index
///   (`MaterialId`) as `f32` bits, `v1[3]` the original triangle index as `f32`
///   bits, `v2[3]` is zero. Triangles are double-sided.
/// - `GpuInstance`: row-major 3x4 `world_from_object` and its inverse
///   `object_from_world`. Rays are transformed into object space without
///   renormalising the direction, so `t` stays in world units. The normal matrix is
///   the transpose of the linear part of `object_from_world`. `node_offset` is the
///   mesh's root in `nodes` (`u32::MAX` for a mesh without triangles; such
///   instances are not in `top_nodes`). `mask` holds the 8-bit instance mask.
/// - `GpuMaterial`: indexed by `MaterialId`; `reflect = 1 - absorption`.
pub mod gpu_layout {
    use bytemuck::{Pod, Zeroable};

    /// 32 bytes. `count == 0` is interior (children `left_or_first` and `+1`).
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
    pub struct BvhNode {
        pub min: [f32; 3],
        pub left_or_first: u32,
        pub max: [f32; 3],
        pub count: u32,
    }
    /// 48 bytes. `v0[3]` holds the material id as `f32` bits.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
    pub struct GpuTriangle {
        pub v0: [f32; 4],
        pub v1: [f32; 4],
        pub v2: [f32; 4],
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
    pub struct GpuInstance {
        pub world_from_object: [f32; 12],
        pub object_from_world: [f32; 12],
        pub node_offset: u32,
        pub triangle_offset: u32,
        pub mask: u32,
        pub _pad: u32,
    }
    /// `reflect` is `1 - absorption`.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
    pub struct GpuMaterial {
        pub reflect: [f32; 4],
        pub transmission: [f32; 4],
        pub scattering: f32,
        pub _pad: [f32; 3],
    }

    const _: () = {
        assert!(size_of::<BvhNode>() == 32);
        assert!(size_of::<GpuTriangle>() == 48);
        assert!(size_of::<GpuInstance>() == 112);
        assert!(size_of::<GpuMaterial>() == 48);
    };
}
