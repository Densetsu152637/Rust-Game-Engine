//! Uploads a validated [`FlatScene`] to GPU buffers, plus BLAS/TLAS for the
//! hardware ray-query tier.

use crate::device::{GpuDevice, Tier};
use crate::error;
use crate::scene::{FlatScene, MeshRange};
use engine_acoustics::BackendError;
use engine_acoustics::gpu_layout::{BvhNode, GpuInstance};
use wgpu::util::DeviceExt;

/// Acceleration structures for the ray-query tier.
pub(crate) struct AccelScene {
    /// One BLAS per distinct mesh; kept alive because the TLAS references them.
    blas: Vec<wgpu::Blas>,
    mesh_of_instance: Vec<u32>,
    pub tlas: wgpu::Tlas,
}

/// Scene buffers on the device.
pub(crate) struct GpuScene {
    pub triangles: wgpu::Buffer,
    pub instances: wgpu::Buffer,
    pub materials: wgpu::Buffer,
    /// Compute tier: mesh nodes followed by the top-level nodes at `top_base`.
    pub nodes: Option<wgpu::Buffer>,
    pub top_base: u32,
    pub accel: Option<AccelScene>,
    pub instance_count: u32,
}

fn storage(gpu: &GpuDevice, label: &str, bytes: &[u8]) -> wgpu::Buffer {
    // Zero-length storage bindings are invalid; pad to one 16-byte element.
    let padded;
    let contents = if bytes.is_empty() {
        padded = [0u8; 16];
        &padded[..]
    } else {
        bytes
    };
    gpu.device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        })
}

impl GpuScene {
    /// Records the uploads (and acceleration-structure builds) into the queue.
    /// The caller wraps this in a validation scope and submits nothing else
    /// until it returns.
    pub fn upload(
        gpu: &GpuDevice,
        tier: Tier,
        scene: &FlatScene,
        meshes: &[MeshRange],
        mesh_of_instance: &[u32],
    ) -> Result<Self, BackendError> {
        let max_binding = gpu.limits.max_storage_buffer_binding_size as usize;
        let largest = [
            size_of_val(scene.triangles.as_slice()),
            size_of_val(scene.instances.as_slice()),
            size_of_val(scene.nodes.as_slice()) + size_of_val(scene.top_nodes.as_slice()),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        if largest > max_binding {
            return Err(error::invalid(format!(
                "world needs a {largest}-byte storage binding; the device allows {max_binding}"
            )));
        }
        let triangles = storage(
            gpu,
            "acoustics triangles",
            bytemuck::cast_slice(&scene.triangles),
        );
        let instances = storage(
            gpu,
            "acoustics instances",
            bytemuck::cast_slice(&scene.instances),
        );
        let materials = storage(
            gpu,
            "acoustics materials",
            bytemuck::cast_slice(&scene.materials),
        );
        let (nodes, top_base) = match tier {
            Tier::Compute => {
                let mut all: Vec<BvhNode> =
                    Vec::with_capacity(scene.nodes.len() + scene.top_nodes.len());
                all.extend_from_slice(&scene.nodes);
                all.extend_from_slice(&scene.top_nodes);
                (
                    Some(storage(gpu, "acoustics bvh", bytemuck::cast_slice(&all))),
                    scene.nodes.len() as u32,
                )
            }
            Tier::RayQuery => (None, 0),
        };
        let accel = match tier {
            Tier::RayQuery => Some(AccelScene::build(gpu, scene, meshes, mesh_of_instance)?),
            Tier::Compute => None,
        };
        Ok(Self {
            triangles,
            instances,
            materials,
            nodes,
            top_base,
            accel,
            instance_count: scene.instances.len() as u32,
        })
    }

    /// Dynamic transforms: rewrites the instance records (and TLAS entries) of
    /// `changed`, plus the refitted top-level nodes for the compute tier.
    pub fn update_instances(
        &mut self,
        gpu: &GpuDevice,
        scene: &FlatScene,
        changed: &[usize],
    ) -> Result<(), BackendError> {
        for &i in changed {
            gpu.queue.write_buffer(
                &self.instances,
                (i * size_of::<GpuInstance>()) as u64,
                bytemuck::bytes_of(&scene.instances[i]),
            );
        }
        if let Some(nodes) = &self.nodes {
            gpu.queue.write_buffer(
                nodes,
                self.top_base as u64 * size_of::<BvhNode>() as u64,
                bytemuck::cast_slice(&scene.top_nodes),
            );
        }
        if let Some(accel) = &mut self.accel {
            accel.update(gpu, scene, changed);
        }
        Ok(())
    }
}

impl AccelScene {
    fn build(
        gpu: &GpuDevice,
        scene: &FlatScene,
        meshes: &[MeshRange],
        mesh_of_instance: &[u32],
    ) -> Result<Self, BackendError> {
        let limits = &gpu.limits;
        if scene.instances.len() as u64 > limits.max_tlas_instance_count as u64 {
            return Err(error::invalid(format!(
                "{} instances exceed the TLAS limit {}",
                scene.instances.len(),
                limits.max_tlas_instance_count
            )));
        }
        let mut sizes = Vec::with_capacity(meshes.len());
        let mut vertex_buffers = Vec::with_capacity(meshes.len());
        let mut blas = Vec::with_capacity(meshes.len());
        for (m, mesh) in meshes.iter().enumerate() {
            if mesh.triangle_count as u64 > limits.max_blas_primitive_count as u64 {
                return Err(error::invalid(format!(
                    "mesh {m} has {} triangles; the BLAS limit is {}",
                    mesh.triangle_count, limits.max_blas_primitive_count
                )));
            }
            // Non-indexed triangle list in the same order as `triangles`, so
            // the hit's primitive index is the mesh-relative triangle index.
            let first = mesh.triangle_offset as usize;
            let positions: Vec<[f32; 3]> = scene.triangles
                [first..first + mesh.triangle_count as usize]
                .iter()
                .flat_map(|t| {
                    [
                        [t.v0[0], t.v0[1], t.v0[2]],
                        [t.v1[0], t.v1[1], t.v1[2]],
                        [t.v2[0], t.v2[1], t.v2[2]],
                    ]
                })
                .collect();
            let size = wgpu::BlasTriangleGeometrySizeDescriptor {
                vertex_format: wgpu::VertexFormat::Float32x3,
                vertex_count: positions.len() as u32,
                index_format: None,
                index_count: None,
                flags: wgpu::AccelerationStructureGeometryFlags::OPAQUE,
            };
            blas.push(gpu.device.create_blas(
                &wgpu::CreateBlasDescriptor {
                    label: Some("acoustics mesh BLAS"),
                    flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                    update_mode: wgpu::AccelerationStructureUpdateMode::Build,
                },
                wgpu::BlasGeometrySizeDescriptors::Triangles {
                    descriptors: vec![size.clone()],
                },
            ));
            vertex_buffers.push(
                gpu.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("acoustics BLAS vertices"),
                        contents: bytemuck::cast_slice(&positions),
                        usage: wgpu::BufferUsages::BLAS_INPUT,
                    }),
            );
            sizes.push(size);
        }
        let mut tlas = gpu.device.create_tlas(&wgpu::CreateTlasDescriptor {
            label: Some("acoustics TLAS"),
            max_instances: scene.instances.len() as u32,
            flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
            update_mode: wgpu::AccelerationStructureUpdateMode::Build,
        });
        for (i, instance) in scene.instances.iter().enumerate() {
            tlas[i] = Some(tlas_instance(
                &blas[mesh_of_instance[i] as usize],
                i,
                instance,
            ));
        }
        let geometries: Vec<wgpu::BlasTriangleGeometry<'_>> = sizes
            .iter()
            .zip(&vertex_buffers)
            .map(|(size, buffer)| wgpu::BlasTriangleGeometry {
                size,
                vertex_buffer: buffer,
                first_vertex: 0,
                vertex_stride: size_of::<[f32; 3]>() as u64,
                index_buffer: None,
                first_index: None,
                transform_buffer: None,
                transform_buffer_offset: None,
            })
            .collect();
        let entries: Vec<wgpu::BlasBuildEntry<'_>> = blas
            .iter()
            .zip(geometries)
            .map(|(blas, geometry)| wgpu::BlasBuildEntry {
                blas,
                geometry: wgpu::BlasGeometries::TriangleGeometries(vec![geometry]),
            })
            .collect();
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("acoustics AS build"),
            });
        encoder.build_acceleration_structures(entries.iter(), std::iter::once(&tlas));
        gpu.queue.submit(Some(encoder.finish()));
        Ok(Self {
            blas,
            mesh_of_instance: mesh_of_instance.to_vec(),
            tlas,
        })
    }

    /// Rewrites the changed TLAS entries and rebuilds the TLAS.
    fn update(&mut self, gpu: &GpuDevice, scene: &FlatScene, changed: &[usize]) {
        for &i in changed {
            let blas = &self.blas[self.mesh_of_instance[i] as usize];
            self.tlas[i] = Some(tlas_instance(blas, i, &scene.instances[i]));
        }
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("acoustics TLAS update"),
            });
        encoder.build_acceleration_structures(std::iter::empty(), std::iter::once(&self.tlas));
        gpu.queue.submit(Some(encoder.finish()));
    }
}

fn tlas_instance(blas: &wgpu::Blas, index: usize, instance: &GpuInstance) -> wgpu::TlasInstance {
    // custom_data (24 bits) is the instance index; `FlatScene::validate` caps
    // the instance count at 2^24.
    wgpu::TlasInstance::new(
        blas,
        instance.world_from_object,
        index as u32,
        (instance.mask & 0xff) as u8,
    )
}
