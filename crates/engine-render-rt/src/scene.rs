//! [`RtScene`]: one BLAS per mesh, one TLAS of instances, the shared geometry
//! buffers the WGSL library reads, and incremental updates.

use crate::device::{Core, RtDevice};
use crate::error::{RtError, invalid};
use crate::layout::{GEOMETRY_HAS_NORMALS, GpuGeometryData, GpuInstanceData};
use crate::limits::{RtInstanceDesc, RtMeshData, RtSceneLimits, validate_instance, validate_mesh};
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Handle of a mesh (BLAS) in one [`RtScene`]. Stale handles are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeshId {
    index: u32,
    generation: u32,
}

impl MeshId {
    pub(crate) const fn from_raw(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// The geometry index (`RtSurface::geometry_index` in WGSL).
    pub fn index(&self) -> u32 {
        self.index
    }
}

/// Handle of an instance in one [`RtScene`]. Stale handles are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceId {
    index: u32,
    generation: u32,
}

impl InstanceId {
    /// The TLAS slot (`RtHit::instance_index` in WGSL).
    pub fn index(&self) -> u32 {
        self.index
    }
}

/// What one commit did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtCommitStats {
    /// BLAS (re)built in this commit.
    pub blas_built: u32,
    /// The TLAS was rebuilt (any instance edit, or a BLAS it references changed).
    pub tlas_built: bool,
    /// The shared geometry buffers were reallocated and repacked (mesh added,
    /// removed or resized); otherwise changed meshes were rewritten in place.
    pub geometry_repacked: bool,
    /// Build-input bytes consumed by the builds: 12 per vertex plus 4 per index
    /// of each rebuilt BLAS, plus 64 per TLAS slot when the TLAS was built.
    pub build_input_bytes: u64,
    /// Bytes written to GPU buffers by this commit.
    pub uploaded_bytes: u64,
}

/// Scene totals. wgpu 30 does not expose acceleration-structure memory sizes,
/// so sizes are the buffers this crate allocates and the build inputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtSceneStats {
    /// Live meshes, one BLAS each.
    pub blas_count: u32,
    pub instance_count: u32,
    /// Triangles over all meshes (each counted once).
    pub triangle_count: u64,
    /// Triangles over all live instances.
    pub instanced_triangle_count: u64,
    pub vertex_count: u64,
    /// Bytes of the shared position, normal, index, geometry and instance buffers.
    pub buffer_bytes: u64,
    /// Build-input bytes of every live BLAS (12 per vertex + 4 per index).
    pub blas_input_bytes: u64,
    pub tlas_capacity: u32,
    /// Commits performed.
    pub commits: u64,
    pub last_commit: RtCommitStats,
}

struct MeshRecord {
    positions: Vec<[f32; 3]>,
    normals: Option<Vec<[f32; 3]>>,
    indices: Vec<u32>,
    users: u32,
    vertex_offset: u32,
    index_offset: u32,
    data_dirty: bool,
    blas_dirty: bool,
    blas: Option<(wgpu::Blas, wgpu::BlasTriangleGeometrySizeDescriptor)>,
}

impl MeshRecord {
    fn triangles(&self) -> u32 {
        (self.indices.len() / 3) as u32
    }

    fn blas_input_bytes(&self) -> u64 {
        self.positions.len() as u64 * 12 + self.indices.len() as u64 * 4
    }

    fn size_descriptor(&self) -> wgpu::BlasTriangleGeometrySizeDescriptor {
        wgpu::BlasTriangleGeometrySizeDescriptor {
            vertex_format: wgpu::VertexFormat::Float32x3,
            vertex_count: self.positions.len() as u32,
            index_format: Some(wgpu::IndexFormat::Uint32),
            index_count: Some(self.indices.len() as u32),
            flags: wgpu::AccelerationStructureGeometryFlags::OPAQUE,
        }
    }
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// Slot storage with generation-checked handles and a free list.
struct Slots<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    live: u32,
}

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
        }
    }
}

impl<T> Slots<T> {
    fn insert(&mut self, value: T) -> (u32, u32) {
        self.live += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            return (index, slot.generation);
        }
        self.slots.push(Slot {
            generation: 0,
            value: Some(value),
        });
        ((self.slots.len() - 1) as u32, 0)
    }

    fn get(&self, index: u32, generation: u32) -> Option<&T> {
        self.slots
            .get(index as usize)
            .filter(|s| s.generation == generation)
            .and_then(|s| s.value.as_ref())
    }

    fn get_mut(&mut self, index: u32, generation: u32) -> Option<&mut T> {
        self.slots
            .get_mut(index as usize)
            .filter(|s| s.generation == generation)
            .and_then(|s| s.value.as_mut())
    }

    fn remove(&mut self, index: u32, generation: u32) -> Option<T> {
        let slot = self
            .slots
            .get_mut(index as usize)
            .filter(|s| s.generation == generation)?;
        let value = slot.value.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(index);
        self.live -= 1;
        Some(value)
    }

    fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.value.as_ref().map(|v| (i as u32, v)))
    }

    fn len(&self) -> u32 {
        self.slots.len() as u32
    }
}

struct GpuBuffers {
    positions: wgpu::Buffer,
    normals: wgpu::Buffer,
    indices: wgpu::Buffer,
    geometries: wgpu::Buffer,
    bytes: u64,
}

/// A ray-traceable scene on one [`RtDevice`].
///
/// Edits are validated immediately and applied on the GPU by
/// [`RtScene::commit`] (or [`RtScene::record_commit`] into the renderer's
/// encoder): a changed mesh rebuilds only its BLAS; any instance edit, or a
/// rebuilt BLAS, rebuilds the TLAS (wgpu 30 has no TLAS refit).
pub struct RtScene {
    core: Arc<Core>,
    limits: RtSceneLimits,
    meshes: Slots<MeshRecord>,
    instances: Slots<RtInstanceDesc>,
    total_triangles: u64,
    total_vertices: u64,
    layout_dirty: bool,
    instances_dirty: bool,
    tlas_dirty: bool,
    buffers: Option<GpuBuffers>,
    instance_buffer: Option<(wgpu::Buffer, u32)>,
    tlas: Option<(wgpu::Tlas, u32)>,
    bind_group: Option<wgpu::BindGroup>,
    bind_group_generation: u64,
    commits: u64,
    last_commit: RtCommitStats,
}

impl RtScene {
    /// An empty scene. `limits` must fit the device ([`RtSceneLimits::for_device`]).
    pub fn new(rt: &RtDevice, limits: RtSceneLimits) -> Result<Self, RtError> {
        rt.core().ensure_usable()?;
        Self::with_core(rt.core().clone(), limits)
    }

    pub(crate) fn with_core(core: Arc<Core>, limits: RtSceneLimits) -> Result<Self, RtError> {
        limits.check_device(&core.limits)?;
        Ok(Self {
            core,
            limits,
            meshes: Slots::default(),
            instances: Slots::default(),
            total_triangles: 0,
            total_vertices: 0,
            layout_dirty: true,
            instances_dirty: true,
            tlas_dirty: true,
            buffers: None,
            instance_buffer: None,
            tlas: None,
            bind_group: None,
            bind_group_generation: 0,
            commits: 0,
            last_commit: RtCommitStats::default(),
        })
    }

    pub fn limits(&self) -> &RtSceneLimits {
        &self.limits
    }

    fn check_totals(&self, add_triangles: u64, add_vertices: u64) -> Result<(), RtError> {
        let triangles = self.total_triangles + add_triangles;
        if triangles > u64::from(self.limits.max_total_triangles) {
            return Err(invalid(format!(
                "scene would hold {triangles} triangles; the limit is {}",
                self.limits.max_total_triangles
            )));
        }
        let vertices = self.total_vertices + add_vertices;
        if vertices > u64::from(self.limits.max_total_vertices) {
            return Err(invalid(format!(
                "scene would hold {vertices} vertices; the limit is {}",
                self.limits.max_total_vertices
            )));
        }
        Ok(())
    }

    fn record(mesh: &RtMeshData<'_>) -> MeshRecord {
        MeshRecord {
            positions: mesh.positions.to_vec(),
            normals: mesh.normals.map(<[[f32; 3]]>::to_vec),
            indices: mesh.indices.to_u32(),
            users: 0,
            vertex_offset: 0,
            index_offset: 0,
            data_dirty: true,
            blas_dirty: true,
            blas: None,
        }
    }

    /// Adds a mesh; its BLAS is built by the next commit.
    pub fn add_mesh(&mut self, mesh: &RtMeshData<'_>) -> Result<MeshId, RtError> {
        let triangles = validate_mesh(mesh, &self.limits)?;
        if self.meshes.live >= self.limits.max_meshes {
            return Err(invalid(format!(
                "scene already holds {} meshes (the limit)",
                self.limits.max_meshes
            )));
        }
        self.check_totals(u64::from(triangles), mesh.positions.len() as u64)?;
        self.total_triangles += u64::from(triangles);
        self.total_vertices += mesh.positions.len() as u64;
        let (index, generation) = self.meshes.insert(Self::record(mesh));
        self.layout_dirty = true;
        Ok(MeshId::from_raw(index, generation))
    }

    /// Replaces a mesh's geometry; the next commit rebuilds its BLAS (in place
    /// when vertex and index counts are unchanged) and the TLAS.
    pub fn update_mesh(&mut self, id: MeshId, mesh: &RtMeshData<'_>) -> Result<(), RtError> {
        let triangles = validate_mesh(mesh, &self.limits)?;
        let old = self
            .meshes
            .get(id.index, id.generation)
            .ok_or_else(|| invalid(format!("unknown or removed mesh {id:?}")))?;
        let (old_triangles, old_vertices) =
            (u64::from(old.triangles()), old.positions.len() as u64);
        let same_layout = old.positions.len() == mesh.positions.len()
            && old.indices.len() == mesh.indices.len()
            && old.normals.is_some() == mesh.normals.is_some();
        let new_triangles = self.total_triangles - old_triangles + u64::from(triangles);
        let new_vertices = self.total_vertices - old_vertices + mesh.positions.len() as u64;
        if new_triangles > u64::from(self.limits.max_total_triangles)
            || new_vertices > u64::from(self.limits.max_total_vertices)
        {
            return Err(invalid(format!(
                "scene would hold {new_triangles} triangles and {new_vertices} vertices; the \
                 limits are {} and {}",
                self.limits.max_total_triangles, self.limits.max_total_vertices
            )));
        }
        self.total_triangles = new_triangles;
        self.total_vertices = new_vertices;
        let record = self
            .meshes
            .get_mut(id.index, id.generation)
            .expect("checked above");
        let fresh = Self::record(mesh);
        record.positions = fresh.positions;
        record.normals = fresh.normals;
        record.indices = fresh.indices;
        record.data_dirty = true;
        record.blas_dirty = true;
        if !same_layout {
            self.layout_dirty = true;
        }
        self.tlas_dirty = true;
        Ok(())
    }

    /// Removes a mesh that no instance uses.
    pub fn remove_mesh(&mut self, id: MeshId) -> Result<(), RtError> {
        let record = self
            .meshes
            .get(id.index, id.generation)
            .ok_or_else(|| invalid(format!("unknown or removed mesh {id:?}")))?;
        if record.users > 0 {
            return Err(invalid(format!(
                "mesh {id:?} is used by {} instances",
                record.users
            )));
        }
        let record = self
            .meshes
            .remove(id.index, id.generation)
            .expect("checked");
        self.total_triangles -= u64::from(record.triangles());
        self.total_vertices -= record.positions.len() as u64;
        self.layout_dirty = true;
        Ok(())
    }

    fn mesh_record_mut(&mut self, mesh: MeshId) -> Result<&mut MeshRecord, RtError> {
        self.meshes
            .get_mut(mesh.index, mesh.generation)
            .ok_or_else(|| {
                invalid(format!(
                    "instance refers to unknown or removed mesh {mesh:?}"
                ))
            })
    }

    /// Adds an instance; the next commit rebuilds the TLAS.
    pub fn add_instance(&mut self, desc: &RtInstanceDesc) -> Result<InstanceId, RtError> {
        validate_instance(desc)?;
        if self.instances.live >= self.limits.max_instances {
            return Err(invalid(format!(
                "scene already holds {} instances (the limit)",
                self.limits.max_instances
            )));
        }
        self.mesh_record_mut(desc.mesh)?.users += 1;
        let (index, generation) = self.instances.insert(*desc);
        self.instances_dirty = true;
        self.tlas_dirty = true;
        Ok(InstanceId { index, generation })
    }

    /// Replaces an instance's description (mesh, transform, mask, ids).
    pub fn set_instance(&mut self, id: InstanceId, desc: &RtInstanceDesc) -> Result<(), RtError> {
        validate_instance(desc)?;
        let old = *self
            .instances
            .get(id.index, id.generation)
            .ok_or_else(|| invalid(format!("unknown or removed instance {id:?}")))?;
        if old.mesh != desc.mesh {
            self.mesh_record_mut(desc.mesh)?.users += 1;
            if let Some(previous) = self.meshes.get_mut(old.mesh.index, old.mesh.generation) {
                previous.users -= 1;
            }
        }
        *self
            .instances
            .get_mut(id.index, id.generation)
            .expect("checked") = *desc;
        self.instances_dirty = true;
        self.tlas_dirty = true;
        Ok(())
    }

    /// Moves an instance; the next commit rebuilds the TLAS only.
    pub fn set_instance_transform(
        &mut self,
        id: InstanceId,
        transform: [f32; 12],
    ) -> Result<(), RtError> {
        let desc = *self
            .instances
            .get(id.index, id.generation)
            .ok_or_else(|| invalid(format!("unknown or removed instance {id:?}")))?;
        self.set_instance(id, &RtInstanceDesc { transform, ..desc })
    }

    pub fn remove_instance(&mut self, id: InstanceId) -> Result<(), RtError> {
        let desc = self
            .instances
            .remove(id.index, id.generation)
            .ok_or_else(|| invalid(format!("unknown or removed instance {id:?}")))?;
        if let Some(mesh) = self.meshes.get_mut(desc.mesh.index, desc.mesh.generation) {
            mesh.users -= 1;
        }
        self.instances_dirty = true;
        self.tlas_dirty = true;
        Ok(())
    }

    pub fn instance(&self, id: InstanceId) -> Option<&RtInstanceDesc> {
        self.instances.get(id.index, id.generation)
    }

    /// `true` when edits are waiting for a commit.
    pub fn is_dirty(&self) -> bool {
        self.layout_dirty
            || self.instances_dirty
            || self.tlas_dirty
            || self
                .meshes
                .iter()
                .any(|(_, m)| m.blas_dirty || m.data_dirty)
    }

    /// Applies pending edits: uploads, BLAS builds and the TLAS build, submitted
    /// and checked in a validation scope. Does not wait for the GPU.
    pub fn commit(&mut self) -> Result<RtCommitStats, RtError> {
        self.core.ensure_usable()?;
        let scope = self
            .core
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let mut encoder =
            self.core
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("engine-render-rt scene commit"),
                });
        let stats = self.record_commit(&mut encoder);
        self.core.queue.submit(Some(encoder.finish()));
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(self
                .core
                .failed(format!("GPU validation error during the scene commit: {e}")));
        }
        stats
    }

    /// Like [`Self::commit`], but records the builds into the caller's encoder
    /// (buffer writes go through the queue and land before that submission).
    /// The caller owns GPU error handling for the submission.
    pub fn record_commit(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<RtCommitStats, RtError> {
        self.core.ensure_usable()?;
        let mut stats = RtCommitStats::default();
        let mut rebind = false;

        // 1. Shared geometry buffers.
        if self.layout_dirty || self.buffers.is_none() {
            stats.uploaded_bytes += self.repack();
            stats.geometry_repacked = true;
            rebind = true;
            self.layout_dirty = false;
        } else if let Some(buffers) = &self.buffers {
            for (_, mesh) in self.meshes.iter().filter(|(_, m)| m.data_dirty) {
                let positions = bytemuck::cast_slice(&mesh.positions);
                let at = u64::from(mesh.vertex_offset) * 12;
                self.core
                    .queue
                    .write_buffer(&buffers.positions, at, positions);
                if let Some(normals) = &mesh.normals {
                    self.core.queue.write_buffer(
                        &buffers.normals,
                        at,
                        bytemuck::cast_slice(normals),
                    );
                }
                let indices = bytemuck::cast_slice(&mesh.indices);
                self.core.queue.write_buffer(
                    &buffers.indices,
                    u64::from(mesh.index_offset) * 4,
                    indices,
                );
                stats.uploaded_bytes += (positions.len() * 2 + indices.len()) as u64;
            }
        }
        for slot in &mut self.meshes.slots {
            if let Some(mesh) = &mut slot.value {
                mesh.data_dirty = false;
            }
        }

        // 2. BLAS objects for changed meshes (reused when the size is unchanged).
        let mut built: Vec<u32> = Vec::new();
        for (index, slot) in self.meshes.slots.iter_mut().enumerate() {
            let Some(mesh) = &mut slot.value else {
                continue;
            };
            if !mesh.blas_dirty {
                continue;
            }
            let size = mesh.size_descriptor();
            let reuse = matches!(&mesh.blas, Some((_, old)) if *old == size);
            if !reuse {
                let blas = self.core.device.create_blas(
                    &wgpu::CreateBlasDescriptor {
                        label: Some("engine-render-rt mesh BLAS"),
                        flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                        update_mode: wgpu::AccelerationStructureUpdateMode::Build,
                    },
                    wgpu::BlasGeometrySizeDescriptors::Triangles {
                        descriptors: vec![size.clone()],
                    },
                );
                mesh.blas = Some((blas, size));
            }
            mesh.blas_dirty = false;
            stats.build_input_bytes += mesh.blas_input_bytes();
            built.push(index as u32);
        }
        stats.blas_built = built.len() as u32;
        if !built.is_empty() {
            self.tlas_dirty = true;
        }

        // 3. Instance records, indexed by TLAS slot.
        let slots = self.instances.len().max(1);
        let capacity = slots.next_power_of_two();
        if self.instances_dirty || self.instance_buffer.is_none() {
            let mut records = vec![GpuInstanceData::default(); capacity as usize];
            for (index, desc) in self.instances.iter() {
                records[index as usize] = GpuInstanceData {
                    geometry: desc.mesh.index,
                    material: desc.material_index,
                    custom_index: desc.custom_index,
                    flags: 0,
                };
            }
            let bytes: &[u8] = bytemuck::cast_slice(&records);
            match &self.instance_buffer {
                Some((buffer, cap)) if *cap == capacity => {
                    self.core.queue.write_buffer(buffer, 0, bytes);
                }
                _ => {
                    let buffer =
                        self.core
                            .device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("engine-render-rt instances"),
                                contents: bytes,
                                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                            });
                    self.instance_buffer = Some((buffer, capacity));
                    rebind = true;
                }
            }
            stats.uploaded_bytes += bytes.len() as u64;
            self.instances_dirty = false;
        }

        // 4. TLAS (recreated when it must grow).
        if self.tlas.as_ref().is_none_or(|(_, cap)| *cap < slots) {
            let tlas = self.core.device.create_tlas(&wgpu::CreateTlasDescriptor {
                label: Some("engine-render-rt TLAS"),
                max_instances: capacity,
                flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                update_mode: wgpu::AccelerationStructureUpdateMode::Build,
            });
            self.tlas = Some((tlas, capacity));
            self.tlas_dirty = true;
            rebind = true;
        }
        if self.tlas_dirty {
            let (tlas, tlas_capacity) = self.tlas.as_mut().expect("created above");
            for i in 0..*tlas_capacity {
                let entry = self.instances.slots.get(i as usize).and_then(|slot| {
                    let desc = slot.value.as_ref()?;
                    let mesh = self.meshes.get(desc.mesh.index, desc.mesh.generation)?;
                    let (blas, _) = mesh.blas.as_ref()?;
                    Some(wgpu::TlasInstance::new(
                        blas,
                        desc.transform,
                        desc.custom_index,
                        desc.mask,
                    ))
                });
                tlas[i as usize] = entry;
            }
            stats.build_input_bytes += u64::from(*tlas_capacity) * 64;
        }

        // 5. Builds.
        let buffers = self.buffers.as_ref().expect("repacked above");
        let sizes: Vec<(
            &wgpu::Blas,
            &wgpu::BlasTriangleGeometrySizeDescriptor,
            &MeshRecord,
        )> = built
            .iter()
            .filter_map(|&i| {
                let mesh = self.meshes.slots[i as usize].value.as_ref()?;
                let (blas, size) = mesh.blas.as_ref()?;
                Some((blas, size, mesh))
            })
            .collect();
        let entries: Vec<wgpu::BlasBuildEntry<'_>> = sizes
            .iter()
            .map(|(blas, size, mesh)| wgpu::BlasBuildEntry {
                blas,
                geometry: wgpu::BlasGeometries::TriangleGeometries(vec![
                    wgpu::BlasTriangleGeometry {
                        size,
                        vertex_buffer: &buffers.positions,
                        first_vertex: mesh.vertex_offset,
                        vertex_stride: 12,
                        index_buffer: Some(&buffers.indices),
                        first_index: Some(mesh.index_offset),
                        transform_buffer: None,
                        transform_buffer_offset: None,
                    },
                ]),
            })
            .collect();
        if !entries.is_empty() || self.tlas_dirty {
            let tlas = self
                .tlas
                .as_ref()
                .map(|(t, _)| t)
                .filter(|_| self.tlas_dirty);
            encoder.build_acceleration_structures(entries.iter(), tlas);
            stats.tlas_built = self.tlas_dirty;
        }
        self.tlas_dirty = false;

        // 6. Bind group.
        if rebind || self.bind_group.is_none() {
            self.bind_group = Some(self.create_bind_group());
            self.bind_group_generation += 1;
        }
        self.commits += 1;
        self.last_commit = stats;
        Ok(stats)
    }

    /// Reallocates the shared buffers and assigns every live mesh its offsets.
    /// Returns the uploaded byte count.
    fn repack(&mut self) -> u64 {
        let mut positions: Vec<[f32; 3]> = Vec::with_capacity(self.total_vertices as usize);
        let mut normals: Vec<[f32; 3]> = Vec::with_capacity(self.total_vertices as usize);
        let mut indices: Vec<u32> = Vec::with_capacity(self.total_triangles as usize * 3);
        let mut geometries = vec![GpuGeometryData::default(); self.meshes.len().max(1) as usize];
        for (index, slot) in self.meshes.slots.iter_mut().enumerate() {
            let Some(mesh) = &mut slot.value else {
                continue;
            };
            mesh.vertex_offset = positions.len() as u32;
            mesh.index_offset = indices.len() as u32;
            positions.extend_from_slice(&mesh.positions);
            match &mesh.normals {
                Some(n) => normals.extend_from_slice(n),
                None => normals.extend(std::iter::repeat_n([0.0; 3], mesh.positions.len())),
            }
            indices.extend_from_slice(&mesh.indices);
            geometries[index] = GpuGeometryData {
                vertex_offset: mesh.vertex_offset,
                index_offset: mesh.index_offset,
                triangle_count: mesh.triangles(),
                flags: if mesh.normals.is_some() {
                    GEOMETRY_HAS_NORMALS
                } else {
                    0
                },
            };
            // New offsets: the BLAS must be rebuilt from the new buffer
            // positions only if its data changed; AS data is self-contained.
        }
        let device = &self.core.device;
        let make = |label: &str, bytes: &[u8], blas_input: bool| {
            // Zero-length bindings are invalid; pad to 16 bytes.
            let padded;
            let contents = if bytes.is_empty() {
                padded = [0u8; 16];
                &padded[..]
            } else {
                bytes
            };
            let mut usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
            if blas_input {
                usage |= wgpu::BufferUsages::BLAS_INPUT;
            }
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        };
        let p: &[u8] = bytemuck::cast_slice(&positions);
        let n: &[u8] = bytemuck::cast_slice(&normals);
        let i: &[u8] = bytemuck::cast_slice(&indices);
        let g: &[u8] = bytemuck::cast_slice(&geometries);
        let bytes = (p.len() + n.len() + i.len() + g.len()) as u64;
        self.buffers = Some(GpuBuffers {
            positions: make("engine-render-rt positions", p, true),
            normals: make("engine-render-rt normals", n, false),
            indices: make("engine-render-rt indices", i, true),
            geometries: make("engine-render-rt geometries", g, false),
            bytes,
        });
        bytes
    }

    fn create_bind_group(&self) -> wgpu::BindGroup {
        let buffers = self.buffers.as_ref().expect("committed");
        let (instances, _) = self.instance_buffer.as_ref().expect("committed");
        let (tlas, _) = self.tlas.as_ref().expect("committed");
        fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        self.core
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("engine-render-rt scene"),
                layout: &self.core.scene_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: tlas.as_binding(),
                    },
                    entry(1, instances),
                    entry(2, &buffers.geometries),
                    entry(3, &buffers.indices),
                    entry(4, &buffers.positions),
                    entry(5, &buffers.normals),
                ],
            })
    }

    /// The scene bind group (layout [`RtDevice::scene_bind_group_layout`]), or
    /// `None` before the first commit.
    pub fn bind_group(&self) -> Option<&wgpu::BindGroup> {
        self.bind_group.as_ref()
    }

    /// Increments whenever [`Self::bind_group`] is replaced (buffers or TLAS
    /// reallocated), so passes caching it know to refresh.
    pub fn bind_group_generation(&self) -> u64 {
        self.bind_group_generation
    }

    pub(crate) fn committed_bind_group(&self) -> Result<&wgpu::BindGroup, RtError> {
        if self.is_dirty() {
            return Err(invalid(
                "the scene has uncommitted edits; call commit first",
            ));
        }
        self.bind_group
            .as_ref()
            .ok_or_else(|| invalid("the scene was never committed"))
    }

    pub(crate) fn is_from(&self, core: &Arc<Core>) -> bool {
        Arc::ptr_eq(&self.core, core)
    }

    pub fn stats(&self) -> RtSceneStats {
        let instanced = self
            .instances
            .iter()
            .filter_map(|(_, d)| self.meshes.get(d.mesh.index, d.mesh.generation))
            .map(|m| u64::from(m.triangles()))
            .sum();
        RtSceneStats {
            blas_count: self.meshes.live,
            instance_count: self.instances.live,
            triangle_count: self.total_triangles,
            instanced_triangle_count: instanced,
            vertex_count: self.total_vertices,
            buffer_bytes: self.buffers.as_ref().map_or(0, |b| b.bytes)
                + self
                    .instance_buffer
                    .as_ref()
                    .map_or(0, |(_, cap)| u64::from(*cap) * 16),
            blas_input_bytes: self.meshes.iter().map(|(_, m)| m.blas_input_bytes()).sum(),
            tlas_capacity: self.tlas.as_ref().map_or(0, |(_, cap)| *cap),
            commits: self.commits,
            last_commit: self.last_commit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_reject_stale_handles_and_reuse_indices() {
        let mut slots = Slots::default();
        let (a, ga) = slots.insert("a");
        let (b, _) = slots.insert("b");
        assert_eq!((a, b), (0, 1));
        assert_eq!(slots.remove(a, ga), Some("a"));
        assert_eq!(slots.get(a, ga), None);
        assert_eq!(slots.remove(a, ga), None);
        let (c, gc) = slots.insert("c");
        assert_eq!(c, a);
        assert_ne!(gc, ga);
        assert_eq!(slots.get(c, gc), Some(&"c"));
        assert_eq!(slots.live, 2);
        assert_eq!(slots.iter().count(), 2);
    }
}
