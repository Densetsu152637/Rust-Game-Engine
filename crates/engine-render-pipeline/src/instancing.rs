//! Instanced draw submission: per-instance data grouped by mesh and
//! material into one buffer and one draw call per group.

use crate::resources::GpuMesh;
use anyhow::{Result, ensure};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

/// Most instances one batcher uploads per frame.
pub const MAX_INSTANCES: usize = 1 << 20;

/// How pushed instances become batches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grouping {
    /// Every instance with the same mesh and material joins one batch;
    /// batches keep the order of their first instance (opaque geometry).
    ByKey,
    /// Only consecutive instances with the same mesh and material merge, so
    /// submission order is kept exactly (blended geometry sorted by depth).
    Consecutive,
}

/// Group `(mesh, material)` keys into batches: member indices in output
/// order plus the batch key.
pub fn group_keys(keys: &[(u64, u64)], grouping: Grouping) -> Vec<(Vec<usize>, (u64, u64))> {
    let mut groups: Vec<(Vec<usize>, (u64, u64))> = Vec::new();
    match grouping {
        Grouping::ByKey => {
            let mut index: HashMap<(u64, u64), usize> = HashMap::new();
            for (i, key) in keys.iter().enumerate() {
                let slot = *index.entry(*key).or_insert_with(|| {
                    groups.push((Vec::new(), *key));
                    groups.len() - 1
                });
                groups[slot].0.push(i);
            }
        }
        Grouping::Consecutive => {
            for (i, key) in keys.iter().enumerate() {
                match groups.last_mut() {
                    Some((members, k)) if k == key => members.push(i),
                    _ => groups.push((vec![i], *key)),
                }
            }
        }
    }
    groups
}

/// One instanced draw call.
#[derive(Clone, Debug)]
pub struct InstancedBatch {
    pub mesh: Arc<GpuMesh>,
    /// Caller-chosen material/pipeline key (equal keys draw identically).
    pub material: u64,
    /// Instance range in the batcher's buffer (`first_instance` based, so
    /// `@builtin(instance_index)` indexes the buffer directly).
    pub instances: Range<u32>,
}

/// Collects `(mesh, material, instance data)` submissions and draws each
/// group with one instanced call. The instance buffer (usage `VERTEX |
/// STORAGE`) can be read either as a per-instance vertex buffer or as a
/// `var<storage, read> array<T>` indexed by `instance_index`.
pub struct InstanceBatcher<T: bytemuck::Pod> {
    label: &'static str,
    pending: Vec<(Arc<GpuMesh>, u64, T)>,
    batches: Vec<InstancedBatch>,
    buffer: Option<wgpu::Buffer>,
    capacity: u64,
}

impl<T: bytemuck::Pod> InstanceBatcher<T> {
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            pending: Vec::new(),
            batches: Vec::new(),
            buffer: None,
            capacity: 0,
        }
    }

    /// Queue one instance of `mesh` drawn with `material`.
    pub fn push(&mut self, mesh: &Arc<GpuMesh>, material: u64, instance: T) {
        self.pending.push((mesh.clone(), material, instance));
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Group the queued instances, upload them and return the batches. The
    /// queue is emptied; the batches stay valid until the next `finish`.
    pub fn finish(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grouping: Grouping,
    ) -> Result<&[InstancedBatch]> {
        ensure!(
            self.pending.len() <= MAX_INSTANCES,
            "at most {MAX_INSTANCES} instances per batcher and frame"
        );
        let pending = std::mem::take(&mut self.pending);
        let keys: Vec<(u64, u64)> = pending.iter().map(|(m, k, _)| (m.id(), *k)).collect();
        let mut data: Vec<T> = Vec::with_capacity(pending.len());
        self.batches.clear();
        for (members, (_, material)) in group_keys(&keys, grouping) {
            let start = data.len() as u32;
            data.extend(members.iter().map(|&i| pending[i].2));
            self.batches.push(InstancedBatch {
                mesh: pending[members[0]].0.clone(),
                material,
                instances: start..data.len() as u32,
            });
        }
        if !data.is_empty() {
            let bytes: &[u8] = bytemuck::cast_slice(&data);
            if self.capacity < bytes.len() as u64 || self.buffer.is_none() {
                let capacity = (bytes.len() as u64).next_power_of_two().max(256);
                self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(self.label),
                    size: capacity,
                    usage: wgpu::BufferUsages::VERTEX
                        | wgpu::BufferUsages::STORAGE
                        | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.capacity = capacity;
            }
            if let Some(buffer) = &self.buffer {
                queue.write_buffer(buffer, 0, bytes);
            }
        }
        Ok(&self.batches)
    }

    pub fn batches(&self) -> &[InstancedBatch] {
        &self.batches
    }

    /// The instance buffer of the last `finish` (`None` before any data).
    pub fn buffer(&self) -> Option<&wgpu::Buffer> {
        self.buffer.as_ref()
    }

    /// Draw every batch: `bind` sets the pipeline and bind groups for a
    /// material key when it changes. With `vertex_slot` the instance buffer
    /// is also bound as a per-instance vertex buffer at that slot.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        vertex_slot: Option<u32>,
        mut bind: impl FnMut(&mut wgpu::RenderPass<'_>, u64),
    ) -> DrawStats {
        let mut stats = DrawStats::default();
        if let (Some(slot), Some(buffer)) = (vertex_slot, &self.buffer) {
            pass.set_vertex_buffer(slot, buffer.slice(..));
        }
        let mut current = None;
        for batch in &self.batches {
            if current != Some(batch.material) {
                bind(pass, batch.material);
                current = Some(batch.material);
            }
            batch.mesh.draw(pass, batch.instances.clone());
            stats.draw_calls += 1;
            stats.instances += batch.instances.len();
        }
        stats
    }
}

/// Draw calls and instances recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawStats {
    pub draw_calls: usize,
    pub instances: usize,
}

impl std::ops::AddAssign for DrawStats {
    fn add_assign(&mut self, other: Self) {
        self.draw_calls += other.draw_calls;
        self.instances += other.instances;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_mesh_and_material_keys_share_one_batch_in_first_use_order() {
        let keys = [(1, 10), (2, 10), (1, 10), (1, 11), (2, 10)];
        let groups = group_keys(&keys, Grouping::ByKey);
        let members: Vec<_> = groups.iter().map(|(m, k)| (m.clone(), *k)).collect();
        assert_eq!(
            members,
            [
                (vec![0, 2], (1, 10)),
                (vec![1, 4], (2, 10)),
                (vec![3], (1, 11)),
            ]
        );
    }

    #[test]
    fn consecutive_grouping_keeps_submission_order() {
        let keys = [(1, 10), (1, 10), (2, 10), (1, 10)];
        let groups = group_keys(&keys, Grouping::Consecutive);
        let members: Vec<_> = groups.into_iter().map(|(m, _)| m).collect();
        assert_eq!(members, [vec![0, 1], vec![2], vec![3]]);
        assert!(group_keys(&[], Grouping::ByKey).is_empty());
    }
}
