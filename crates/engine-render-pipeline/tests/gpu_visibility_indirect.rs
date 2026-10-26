//! Real GPU tests for the opt-in Hi-Z indirect consumer.
use anyhow::{Context, Result};
use engine_render_hiz::{HiZKernel, VisibilityQuery};
use engine_render_pipeline::{
    ENGINE_VERTEX_LAYOUT, EngineVertex, GpuContext, GpuInstance, GpuMesh,
    visibility::{HiZIndirect, MAX_INSTANCES_PER_BATCH, VisibilityBatch, reference_batches},
};
use engine_render_wgpu::WgpuOptions;
use std::sync::Arc;

const SIZE: [u32; 2] = [80, 24];
const DEPTH_SIZE: [u32; 2] = [3, 5];
const SHADER: &str = r#"
struct EngineInstance {
    world_from_local: mat4x4<f32>,
    normal_from_local: mat4x4<f32>,
    previous_world_from_local: mat4x4<f32>,
    params: vec4<u32>,
}
@group(3) @binding(0) var<storage, read> instances: array<EngineInstance>;
@vertex fn vs(@location(0) position: vec3<f32>, @builtin(instance_index) i: u32)
    -> @builtin(position) vec4<f32> {
    let center = -0.75 + f32(instances[i].params.x) * 0.5;
    return vec4<f32>(center + position.x * 0.4, position.y * 0.9, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 1.0, 0.0, 1.0);
}
"#;

fn gpu() -> Result<GpuContext> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    eprintln!("Hi-Z indirect adapter: {}", gpu.adapter.get_info().name);
    Ok(gpu)
}

fn mesh(gpu: &GpuContext, indexed: bool) -> Result<Arc<GpuMesh>> {
    let vertex = |x, y| EngineVertex {
        position: [x, y, 0.0],
        uv: [0.0; 2],
        color: [1.0; 4],
        normal: [0.0, 0.0, 1.0],
    };
    GpuMesh::from_engine_vertices(
        &gpu.device,
        "indirect diagnostic triangle",
        &[vertex(-0.5, -0.5), vertex(0.5, -0.5), vertex(0.0, 0.5)],
        if indexed { &[0, 1, 2] } else { &[] },
    )
}

fn instance(id: u32) -> GpuInstance {
    let identity = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    GpuInstance {
        world_from_local: identity,
        normal_from_local: identity,
        previous_world_from_local: identity,
        params: [id, 0, 0, 0],
    }
}

fn group3(gpu: &GpuContext) -> wgpu::BindGroupLayout {
    gpu.device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("diagnostic group 3"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        })
}

fn pipeline(gpu: &GpuContext, group3: &wgpu::BindGroupLayout) -> wgpu::RenderPipeline {
    let empty = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("unused group"),
            entries: &[],
        });
    let layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("diagnostic indirect"),
            bind_group_layouts: &[Some(&empty), Some(&empty), Some(&empty), Some(group3)],
            immediate_size: 0,
        });
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("diagnostic indirect"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("diagnostic indirect"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(ENGINE_VERTEX_LAYOUT)],
            },
            primitive: Default::default(),
            depth_stencil: None, // omitted pixels must be caused by the mask
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            }),
            multiview_mask: None,
            cache: None,
        })
}

fn read_buffer(gpu: &GpuContext, buffer: &wgpu::Buffer) -> Result<Vec<u8>> {
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("indirect readback"),
        size: buffer.size(),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, buffer.size());
    gpu.queue.submit([encoder.finish()]);
    let slice = staging.slice(..);
    let (send, receive) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = send.send(result);
    });
    gpu.wait_idle()?;
    receive
        .recv()
        .context("indirect readback callback absent")??;
    let mapped = slice
        .get_mapped_range()
        .map_err(|e| anyhow::anyhow!("indirect map: {e:?}"))?;
    let bytes = mapped.to_vec();
    drop(mapped);
    staging.unmap();
    Ok(bytes)
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[test]
#[ignore = "requires a real GPU adapter"]
fn actual_hiz_masks_compact_and_draw_indexed_and_nonindexed_tiles() -> Result<()> {
    let gpu = gpu()?;
    let hiz = HiZKernel::new(&gpu)?;
    let view = hiz.create_view(&gpu, DEPTH_SIZE, 4)?;
    let adapter = HiZIndirect::new(&gpu)?;
    let group = group3(&gpu);
    let draw_pipeline = pipeline(&gpu, &group);
    let records = [instance(0), instance(1), instance(2), instance(3)];
    let plain = mesh(&gpu, false)?;
    let indexed = mesh(&gpu, true)?;
    let batches = [
        VisibilityBatch {
            mesh: plain.clone(),
            instances: 0..2,
        },
        VisibilityBatch {
            mesh: indexed.clone(),
            instances: 2..4,
        },
        VisibilityBatch {
            mesh: indexed.clone(),
            instances: 4..4,
        },
    ];
    let depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test forward depth"),
        size: wgpu::Extent3d {
            width: DEPTH_SIZE[0],
            height: DEPTH_SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("indirect diagnostic tiles"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let depth_view = depth.create_view(&Default::default());
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear forward depth 0.2"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.2),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
    }
    let queries = [0.5, 0.1, 0.1, 0.5].map(|near| {
        VisibilityQuery::new(
            [0, 0, DEPTH_SIZE[0] as i32, DEPTH_SIZE[1] as i32],
            near,
            false,
        )
    });
    let masks = hiz.encode(
        &gpu,
        &mut encoder,
        &view,
        &depth,
        [0, 0, DEPTH_SIZE[0], DEPTH_SIZE[1]],
        &queries,
    )?;
    assert!(
        adapter
            .encode(&gpu, &mut encoder, &masks, &records[..3], &batches, &group)
            .is_err()
    );
    let output = adapter.encode(&gpu, &mut encoder, &masks, &records, &batches, &group)?;
    let target_view = target.create_view(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("depth-disabled indirect diagnostic"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&draw_pipeline);
        for i in 0..batches.len() {
            output.draw_batch(&mut pass, i)?;
        }
    }
    gpu.queue.submit([encoder.finish()]);
    gpu.wait_idle()?;
    let mask_bytes = read_buffer(&gpu, &masks.buffer)?;
    let mask_values: Vec<u32> = (0..4).map(|i| word(&mask_bytes, i * 4)).collect();
    assert_eq!(mask_values, vec![0, 1, 1, 0]);
    let expected = reference_batches(&mask_values, &batches)?;
    assert_eq!(expected, vec![vec![1], vec![2], vec![]]);
    let draws = read_buffer(&gpu, &output.draw_args)?;
    let indexed_draws = read_buffer(&gpu, &output.indexed_args)?;
    let compacted = read_buffer(&gpu, &output.instances)?;
    for (i, batch) in output.batches.iter().enumerate() {
        assert_eq!(word(&draws, i * 16), batch.mesh.count);
        assert_eq!(word(&indexed_draws, i * 20), batch.mesh.count);
        assert_eq!(word(&draws, i * 16 + 4), expected[i].len() as u32);
        assert_eq!(word(&indexed_draws, i * 20 + 4), expected[i].len() as u32);
        assert_eq!(word(&draws, i * 16 + 12), 0); // first_instance
        assert_eq!(word(&indexed_draws, i * 20 + 16), 0);
        assert_eq!(
            batch.instance_offset
                % u64::from(gpu.device.limits().min_storage_buffer_offset_alignment),
            0
        );
        for (local, &source) in expected[i].iter().enumerate() {
            assert_eq!(
                word(
                    &compacted,
                    batch.instance_offset as usize + local * 208 + 192
                ),
                records[source as usize].params[0]
            );
        }
    }
    assert!(output.batches[1].instance_offset > output.batches[0].instance_offset);
    let pixels = engine_render_wgpu::read_texture_rgba(&gpu, &target, SIZE[0], SIZE[1])?;
    for (tile, expected_green) in [(0, false), (1, true), (2, true), (3, false)] {
        let x = (10 + tile * 20) as usize;
        let at = (12 * SIZE[0] as usize + x) * 4;
        assert_eq!(pixels[at + 1] > 200, expected_green, "tile {tile}");
    }
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter"]
fn actual_hiz_all_visible_and_zero_visible_counts() -> Result<()> {
    let gpu = gpu()?;
    let hiz = HiZKernel::new(&gpu)?;
    let view = hiz.create_view(&gpu, DEPTH_SIZE, 4)?;
    let adapter = HiZIndirect::new(&gpu)?;
    let group = group3(&gpu);
    let records = [instance(0), instance(1), instance(2), instance(3)];
    let plain = mesh(&gpu, false)?;
    let indexed = mesh(&gpu, true)?;
    let batches = [
        VisibilityBatch {
            mesh: plain,
            instances: 0..2,
        },
        VisibilityBatch {
            mesh: indexed,
            instances: 2..4,
        },
    ];
    let depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test depth all cases"),
        size: wgpu::Extent3d {
            width: 3,
            height: 5,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    for (nearest, wanted) in [(0.1, 2), (0.5, 0)] {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let depth_view = depth.create_view(&Default::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear depth all cases"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.2),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
        }
        let queries = [VisibilityQuery::new([0, 0, 3, 5], nearest, false); 4];
        let masks = hiz.encode(&gpu, &mut encoder, &view, &depth, [0, 0, 3, 5], &queries)?;
        let output = adapter.encode(&gpu, &mut encoder, &masks, &records, &batches, &group)?;
        gpu.queue.submit([encoder.finish()]);
        gpu.wait_idle()?;
        let args = read_buffer(&gpu, &output.draw_args)?;
        for i in 0..2 {
            assert_eq!(word(&args, i * 16 + 4), wanted);
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter"]
fn restricted_storage_caps_reject_cleanly() -> Result<()> {
    let options = WgpuOptions::default();
    let instance = GpuContext::instance();
    let adapter = pollster::block_on(GpuContext::request_adapter(&instance, None, &options))?;
    let composed = GpuContext::compose_device(&options, &adapter)?;
    let mut descriptor = composed.descriptor("hiz indirect restricted storage");
    descriptor
        .required_limits
        .max_storage_buffers_per_shader_stage = 5;
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))?;
    let gpu = GpuContext::from_device(instance, adapter, device, queue);
    let error = HiZIndirect::new(&gpu)
        .err()
        .context("restricted device accepted indirect compaction")?;
    assert!(error.to_string().contains("six storage"), "{error}");
    gpu.wait_idle()?;
    assert_eq!(MAX_INSTANCES_PER_BATCH, 1024);
    Ok(())
}
#[test]
#[ignore = "requires a real GPU adapter"]
fn oversized_batch_returns_fallback_error_without_encoding_draws() -> Result<()> {
    let gpu = gpu()?;
    let hiz = HiZKernel::new(&gpu)?;
    let adapter = HiZIndirect::new(&gpu)?;
    let group = group3(&gpu);
    let count = MAX_INSTANCES_PER_BATCH + 1;
    let view = hiz.create_view(&gpu, DEPTH_SIZE, count)?;
    let depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("oversized batch depth"),
        size: wgpu::Extent3d {
            width: 3,
            height: 5,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let depth_view = depth.create_view(&Default::default());
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear oversized depth"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
    }
    let queries = vec![VisibilityQuery::new([0, 0, 3, 5], 0.1, false); count as usize];
    let masks = hiz.encode(&gpu, &mut encoder, &view, &depth, [0, 0, 3, 5], &queries)?;
    let records = vec![instance(0); count as usize];
    let batch = [VisibilityBatch {
        mesh: mesh(&gpu, false)?,
        instances: 0..count,
    }];
    let error = adapter
        .encode(&gpu, &mut encoder, &masks, &records, &batch, &group)
        .err()
        .context("oversized batch was accepted")?;
    assert!(
        error.to_string().contains("batch candidate capacity"),
        "{error}"
    );
    gpu.queue.submit([encoder.finish()]); // the preceding Hi-Z work remains valid
    gpu.wait_idle()?;
    Ok(())
}
