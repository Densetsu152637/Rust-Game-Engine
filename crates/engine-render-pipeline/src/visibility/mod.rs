//! Bounded GPU compaction of Hi-Z masks into renderer-native group-3
//! instances and indirect commands. This is an opt-in adapter; it does not
//! change the built-in mesh path or decide projected bounds/LOD.
use crate::{GpuInstance, GpuMesh};
use anyhow::{Result, ensure};
use engine_render_hiz::VisibilityOutput;
use engine_render_wgpu::GpuContext;
use std::ops::Range;
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub const MAX_INDIRECT_INSTANCES: usize = 65_536;
pub const MAX_INDIRECT_BATCHES: usize = 256;
pub const MAX_INSTANCES_PER_BATCH: u32 = 1024;
const LANES: u32 = 64;
const INSTANCE_BYTES: u64 = std::mem::size_of::<GpuInstance>() as u64;
const DRAW_BYTES: u64 = 16;
const INDEXED_BYTES: u64 = 20;
const META_BYTES: u64 = 16;
const _: () = assert!(std::mem::size_of::<GpuInstance>() == 208);

/// One stable source range sharing a mesh. Ranges may be empty; an empty
/// batch still emits a valid zero-instance indirect command.
#[derive(Clone)]
pub struct VisibilityBatch {
    pub mesh: Arc<GpuMesh>,
    pub instances: Range<u32>,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BatchMeta {
    first: u32,
    count: u32,
    output_base: u32,
    draw_count: u32,
}

/// Renderer binding and command offsets for one batch.
pub struct IndirectBatch {
    pub mesh: Arc<GpuMesh>,
    pub instance_bind_group: wgpu::BindGroup,
    pub draw_offset: u64,
    pub indexed_offset: u64,
    /// Byte offset of the batch's aligned group-3 instance range.
    pub instance_offset: u64,
    pub source: Range<u32>,
}

/// Generated GPU records and indirect argument arrays. The generated
/// `instance_count` is unknown until GPU execution and lives only in args.
pub struct IndirectOutput {
    pub instances: wgpu::Buffer,
    pub draw_args: wgpu::Buffer,
    pub indexed_args: wgpu::Buffer,
    pub batches: Vec<IndirectBatch>,
}

impl IndirectOutput {
    /// Bind group 3 and issue the actual generated indexed/nonindexed command.
    /// The caller binds a compatible pipeline, view and material groups first.
    pub fn draw_batch(&self, pass: &mut wgpu::RenderPass<'_>, index: usize) -> Result<()> {
        let batch = self
            .batches
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("indirect batch index out of range"))?;
        pass.set_bind_group(3, &batch.instance_bind_group, &[]);
        pass.set_vertex_buffer(0, batch.mesh.vertex.slice(..));
        if let Some(indices) = &batch.mesh.index {
            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed_indirect(&self.indexed_args, batch.indexed_offset);
        } else {
            pass.draw_indirect(&self.draw_args, batch.draw_offset);
        }
        Ok(())
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn aligned_record(cursor: u64, records_per_alignment: u64) -> Result<u64> {
    cursor
        .checked_add(records_per_alignment - 1)
        .map(|value| value / records_per_alignment * records_per_alignment)
        .ok_or_else(|| anyhow::anyhow!("aligned instance offset overflow"))
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Immutable compute pipeline. No `INDIRECT_FIRST_INSTANCE` feature is
/// requested: every batch binds its own aligned record range and draws with
/// first_instance zero.
pub struct HiZIndirect {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl HiZIndirect {
    pub fn new(gpu: &GpuContext) -> Result<Self> {
        let limits = gpu.device.limits();
        ensure!(
            gpu.adapter
                .get_downlevel_capabilities()
                .flags
                .contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION),
            "adapter lacks indirect draw execution"
        );
        ensure!(
            limits.max_compute_workgroup_size_x >= LANES
                && limits.max_compute_workgroup_size_y >= 1
                && limits.max_compute_workgroup_size_z >= 1
                && limits.max_compute_invocations_per_workgroup >= LANES,
            "indirect compaction requires a 64-lane workgroup"
        );
        ensure!(
            limits.max_compute_workgroup_storage_size >= 260,
            "indirect compaction needs 260 bytes of workgroup storage"
        );
        ensure!(
            limits.max_storage_buffers_per_shader_stage >= 6
                && limits.max_bindings_per_bind_group >= 6
                && limits.max_bind_groups >= 1,
            "indirect compaction requires six storage bindings"
        );
        ensure!(
            limits.min_storage_buffer_offset_alignment > 0,
            "storage offset alignment must be positive"
        );
        crate::scoped(&gpu.device, || {
            let entries = [
                storage_entry(0, true),
                storage_entry(1, true),
                storage_entry(2, false),
                storage_entry(3, false),
                storage_entry(4, false),
                storage_entry(5, true),
            ];
            let layout = gpu
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("hiz indirect compaction"),
                    entries: &entries,
                });
            let module = gpu
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("hiz indirect compaction"),
                    source: wgpu::ShaderSource::Wgsl(include_str!("compact.wgsl").into()),
                });
            let pipeline_layout =
                gpu.device
                    .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("hiz indirect compaction"),
                        bind_group_layouts: &[Some(&layout)],
                        immediate_size: 0,
                    });
            let pipeline = gpu
                .device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("hiz indirect compaction"),
                    layout: Some(&pipeline_layout),
                    module: &module,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                });
            Ok(Self { layout, pipeline })
        })
    }

    /// Consume the exact GPU mask returned by `HiZKernel::encode` in the
    /// same ordered command stream. `instances[i]` corresponds to mask
    /// entry `i`; batch ranges index both arrays. On any error the caller
    /// should use its normal direct draw path for the whole frame.
    pub fn encode(
        &self,
        gpu: &GpuContext,
        encoder: &mut wgpu::CommandEncoder,
        masks: &VisibilityOutput,
        instances: &[GpuInstance],
        batches: &[VisibilityBatch],
        group3_layout: &wgpu::BindGroupLayout,
    ) -> Result<IndirectOutput> {
        let limits = gpu.device.limits();
        ensure!(
            instances.len() <= MAX_INDIRECT_INSTANCES,
            "indirect instance capacity exceeded"
        );
        ensure!(
            batches.len() <= MAX_INDIRECT_BATCHES,
            "indirect batch capacity exceeded"
        );
        ensure!(
            masks.count as usize == instances.len(),
            "mask and instance counts differ"
        );
        ensure!(
            masks.buffer.usage().contains(wgpu::BufferUsages::STORAGE),
            "mask needs STORAGE usage"
        );
        let mask_bytes = (instances.len() as u64)
            .max(1)
            .checked_mul(4)
            .ok_or_else(|| anyhow::anyhow!("mask byte count overflow"))?;
        ensure!(
            masks.buffer.size() >= mask_bytes
                && mask_bytes <= limits.max_storage_buffer_binding_size,
            "mask buffer is too small or exceeds storage binding limit"
        );
        ensure!(
            batches.len() as u32 <= limits.max_compute_workgroups_per_dimension,
            "batch count exceeds compute dispatch limit"
        );
        let input_bytes = (instances.len() as u64)
            .max(1)
            .checked_mul(INSTANCE_BYTES)
            .ok_or_else(|| anyhow::anyhow!("source instance bytes overflow"))?;
        ensure!(
            input_bytes <= limits.max_buffer_size
                && input_bytes <= limits.max_storage_buffer_binding_size,
            "source instances exceed device buffer limits"
        );

        let alignment = u64::from(limits.min_storage_buffer_offset_alignment);
        let records_per_alignment = alignment / gcd(INSTANCE_BYTES, alignment);
        let mut cursor = 0u64;
        let mut meta = Vec::with_capacity(batches.len().max(1));
        let mut ranges = Vec::with_capacity(batches.len());
        for batch in batches {
            ensure!(
                batch.instances.start <= batch.instances.end
                    && batch.instances.end as usize <= instances.len(),
                "batch range exceeds source instances"
            );
            ensure!(batch.mesh.count > 0, "batch mesh has no primitives");
            let count = u64::from(batch.instances.end - batch.instances.start);
            ensure!(
                count <= u64::from(MAX_INSTANCES_PER_BATCH),
                "batch candidate capacity exceeded"
            );
            cursor = aligned_record(cursor, records_per_alignment)?;
            let start = cursor;
            let bound_records = count.max(1); // a zero-candidate batch still has a legal binding
            cursor = cursor
                .checked_add(bound_records)
                .ok_or_else(|| anyhow::anyhow!("compacted instance count overflow"))?;
            ensure!(
                start <= u64::from(u32::MAX),
                "instance output offset exceeds u32 shader indexing"
            );
            let binding_bytes = bound_records
                .checked_mul(INSTANCE_BYTES)
                .ok_or_else(|| anyhow::anyhow!("batch binding byte count overflow"))?;
            ensure!(
                binding_bytes <= limits.max_storage_buffer_binding_size,
                "batch group-3 binding exceeds device storage limit"
            );
            meta.push(BatchMeta {
                first: batch.instances.start,
                count: count as u32,
                output_base: start as u32,
                draw_count: batch.mesh.count,
            });
            ranges.push((start * INSTANCE_BYTES, binding_bytes));
        }
        let output_bytes = cursor
            .max(1)
            .checked_mul(INSTANCE_BYTES)
            .ok_or_else(|| anyhow::anyhow!("compacted buffer byte count overflow"))?;
        ensure!(
            output_bytes <= limits.max_buffer_size
                && output_bytes <= limits.max_storage_buffer_binding_size,
            "compacted instances exceed device buffer limits"
        );
        let draw_bytes = (batches.len() as u64).max(1) * DRAW_BYTES;
        let indexed_bytes = (batches.len() as u64).max(1) * INDEXED_BYTES;
        let meta_bytes = (batches.len() as u64).max(1) * META_BYTES;
        ensure!(
            [draw_bytes, indexed_bytes, meta_bytes].iter().all(
                |&v| v <= limits.max_buffer_size && v <= limits.max_storage_buffer_binding_size
            ),
            "indirect metadata or arguments exceed device limits"
        );

        let device = &gpu.device;
        let input = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hiz indirect source instances"),
            size: input_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !instances.is_empty() {
            gpu.queue
                .write_buffer(&input, 0, bytemuck::cast_slice(instances));
        }
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hiz indirect compacted instances"),
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let draw_args = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hiz draw indirect args"),
            size: draw_bytes,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let indexed_args = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hiz draw indexed indirect args"),
            size: indexed_bytes,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let metas = if meta.is_empty() {
            vec![BatchMeta {
                first: 0,
                count: 0,
                output_base: 0,
                draw_count: 0,
            }]
        } else {
            meta
        };
        let metadata = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("hiz indirect batch metadata"),
            contents: bytemuck::cast_slice(&metas),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let render_batches = crate::scoped(device, || {
            Ok(batches
                .iter()
                .zip(ranges.iter())
                .enumerate()
                .map(|(i, (batch, &(offset, size)))| {
                    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("hiz compacted group 3"),
                        layout: group3_layout,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &output,
                                offset,
                                size: wgpu::BufferSize::new(size),
                            }),
                        }],
                    });
                    IndirectBatch {
                        mesh: batch.mesh.clone(),
                        instance_bind_group: group,
                        draw_offset: i as u64 * DRAW_BYTES,
                        indexed_offset: i as u64 * INDEXED_BYTES,
                        instance_offset: offset,
                        source: batch.instances.clone(),
                    }
                })
                .collect::<Vec<_>>())
        })?;
        if !batches.is_empty() {
            let compute = crate::scoped(device, || {
                Ok(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("hiz indirect compute"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: masks.buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: input.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: output.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: draw_args.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: indexed_args.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: metadata.as_entire_binding(),
                        },
                    ],
                }))
            })?;
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("hiz indirect compaction"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &compute, &[]);
            pass.dispatch_workgroups(batches.len() as u32, 1, 1);
        }
        Ok(IndirectOutput {
            instances: output,
            draw_args,
            indexed_args,
            batches: render_batches,
        })
    }
}

/// CPU reference of stable survivors, independent of the GPU scan.
pub fn reference_ranges(masks: &[u32], ranges: &[Range<u32>]) -> Result<Vec<Vec<u32>>> {
    let mut result = Vec::with_capacity(ranges.len());
    for range in ranges {
        ensure!(
            range.start <= range.end && range.end as usize <= masks.len(),
            "batch range exceeds masks"
        );
        result.push(range.clone().filter(|&i| masks[i as usize] != 0).collect());
    }
    Ok(result)
}

pub fn reference_batches(masks: &[u32], batches: &[VisibilityBatch]) -> Result<Vec<Vec<u32>>> {
    let ranges: Vec<_> = batches
        .iter()
        .map(|batch| batch.instances.clone())
        .collect();
    reference_ranges(masks, &ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_keeps_stable_survivors_and_empty_batches() {
        let ranges = [0..3, 3..6, 6..6];
        let survivors = reference_ranges(&[0, 1, 1, 1, 0, 1], &ranges).unwrap();
        assert_eq!(survivors, vec![vec![1, 2], vec![3, 5], vec![]]);
        assert!(reference_ranges(&[1], &[0..2]).is_err());
    }

    #[test]
    fn record_alignment_is_a_common_multiple() {
        let record = INSTANCE_BYTES;
        for alignment in [1, 16, 64, 256, 512] {
            let records = alignment / gcd(record, alignment);
            let start = aligned_record(1, records).unwrap();
            assert_eq!(start * record % alignment, 0);
            assert_eq!(start, records);
            assert_eq!(aligned_record(start, records).unwrap(), start);
        }
    }
}
