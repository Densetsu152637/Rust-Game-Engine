//! Real-adapter readback tests. Run with --ignored --test-threads=1.
use anyhow::{Context, Result, ensure};
use engine_render_hiz::*;
use engine_render_wgpu::{GpuContext, WgpuOptions};

fn gpu() -> Result<GpuContext> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    eprintln!("Hi-Z adapter: {}", gpu.adapter.get_info().name);
    Ok(gpu)
}

fn depth_texture(gpu: &GpuContext, size: [u32; 2], values: &[f32]) -> wgpu::Texture {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hiz test depth"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::DepthOnly,
        },
        bytemuck::cast_slice(values),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size[0] * 4),
            rows_per_image: Some(size[1]),
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    texture
}

fn read_buffer(gpu: &GpuContext, source: &wgpu::Buffer, bytes: u64) -> Result<Vec<u8>> {
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hiz test readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(source, 0, &readback, 0, bytes);
    gpu.queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    let (send, receive) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = send.send(result);
    });
    gpu.wait_idle()?;
    receive.recv().context("readback callback absent")??;
    let mapped = slice
        .get_mapped_range()
        .map_err(|e| anyhow::anyhow!("readback mapping: {e:?}"))?;
    let values = mapped.to_vec();
    drop(mapped);
    readback.unmap();
    Ok(values)
}

fn read_mip(
    gpu: &GpuContext,
    texture: &wgpu::Texture,
    level: u32,
    size: [u32; 2],
) -> Result<Vec<f32>> {
    let row = size[0] * 4;
    let padded =
        row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bytes = u64::from(padded) * u64::from(size[1]);
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hiz mip readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(size[1]),
            },
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    let (send, receive) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = send.send(result);
    });
    gpu.wait_idle()?;
    receive.recv().context("mip callback absent")??;
    let mapped = slice
        .get_mapped_range()
        .map_err(|e| anyhow::anyhow!("mip mapping: {e:?}"))?;
    let mut values = Vec::with_capacity(size[0] as usize * size[1] as usize);
    for y in 0..size[1] as usize {
        for x in 0..size[0] as usize {
            let start = y * padded as usize + x * 4;
            values.push(f32::from_le_bytes(mapped[start..start + 4].try_into()?));
        }
    }
    drop(mapped);
    readback.unmap();
    Ok(values)
}

fn run_case(
    gpu: &GpuContext,
    kernel: &HiZKernel,
    view: &HiZView,
    values: &[f32],
    queries: &[VisibilityQuery],
) -> Result<()> {
    let size = view.size();
    let depth = depth_texture(gpu, size, values);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let output = kernel.encode(
        gpu,
        &mut encoder,
        view,
        &depth,
        [0, 0, size[0], size[1]],
        queries,
    )?;
    gpu.queue.submit([encoder.finish()]);
    gpu.wait_idle()?;
    let expected = reference_pyramid(size, values)?;
    for (level, &dimensions) in view.mip_dimensions().iter().enumerate() {
        let actual = read_mip(gpu, view.texture(), level as u32, dimensions)?;
        assert_eq!(actual, expected[level], "mip {level} size {size:?}");
    }
    if !queries.is_empty() {
        let bytes = read_buffer(gpu, &output.buffer, u64::from(output.count) * 4)?;
        for (i, query) in queries.iter().enumerate() {
            let actual = u32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into()?);
            let expected = reference_visibility(size, &expected, *query)?;
            assert_eq!(actual, expected, "query {i} size {size:?}");
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter"]
fn all_mips_queries_resize_and_independent_views() -> Result<()> {
    let gpu = gpu()?;
    let kernel = HiZKernel::new(&gpu)?;
    let queries = |size: [u32; 2]| {
        let mut queries = Vec::new();
        for y0 in 0..size[1] {
            for x0 in 0..size[0] {
                for y1 in y0 + 1..=size[1] {
                    for x1 in x0 + 1..=size[0] {
                        for nearest in [0.2, 0.8, 0.81] {
                            queries.push(VisibilityQuery::new(
                                [x0 as i32, y0 as i32, x1 as i32, y1 as i32],
                                nearest,
                                false,
                            ));
                        }
                    }
                }
            }
        }
        queries.extend([
            VisibilityQuery::new([-1, 0, 1, 1], 0.9, false),
            VisibilityQuery::new([0, 0, size[0] as i32 + 1, size[1] as i32], 0.9, false),
            VisibilityQuery::new([0, 0, size[0] as i32, size[1] as i32], 0.9, true),
            VisibilityQuery::new([0, 0, size[0] as i32, size[1] as i32], f32::NAN, false),
        ]);
        queries
    };
    for size in [[3, 5], [9, 7], [1, 17], [17, 1]] {
        let mut view = kernel.create_view(&gpu, size, 4096)?;
        let mut values: Vec<f32> = (0..size[0] * size[1])
            .map(|i| (i * 37 % 97) as f32 / 100.0)
            .collect();
        *values.last_mut().context("empty depth")? = 0.8;
        run_case(&gpu, &kernel, &view, &values, &queries(size))?;
        values.fill(1.0); // a fresh frame must fully replace all old mip contents
        run_case(&gpu, &kernel, &view, &values, &queries(size))?;
        view.resize(&gpu, [5, 3])?;
        let resized = vec![0.3; 15];
        run_case(&gpu, &kernel, &view, &resized, &queries([5, 3]))?;
    }
    let first = kernel.create_view(&gpu, [3, 5], 4096)?;
    let second = kernel.create_view(&gpu, [3, 5], 4096)?;
    run_case(&gpu, &kernel, &first, &[0.1; 15], &queries([3, 5]))?;
    run_case(&gpu, &kernel, &second, &[0.9; 15], &queries([3, 5]))?;
    ensure!(
        kernel.create_view(&gpu, [0, 1], 1).is_err(),
        "invalid size accepted"
    );
    ensure!(
        kernel.create_view(&gpu, [1, 1], MAX_QUERIES + 1).is_err(),
        "excess capacity accepted"
    );
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter"]
fn input_contract_rejects_wrong_depth_and_viewport() -> Result<()> {
    let gpu = gpu()?;
    let kernel = HiZKernel::new(&gpu)?;
    let view = kernel.create_view(&gpu, [3, 5], 1)?;
    let depth = depth_texture(&gpu, [3, 5], &[1.0; 15]);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    assert!(
        kernel
            .encode(&gpu, &mut encoder, &view, &depth, [1, 0, 3, 5], &[])
            .is_err()
    );
    assert!(
        kernel
            .encode(&gpu, &mut encoder, &view, &depth, [0, 0, 2, 5], &[])
            .is_err()
    );
    assert!(
        kernel
            .encode(
                &gpu,
                &mut encoder,
                &view,
                &depth,
                [0, 0, 3, 5],
                &[VisibilityQuery::new([0, 0, 1, 1], 0.5, false); 2]
            )
            .is_err()
    );
    let wrong_format = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hiz wrong-format input"),
        size: wgpu::Extent3d {
            width: 3,
            height: 5,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    assert!(
        kernel
            .encode(&gpu, &mut encoder, &view, &wrong_format, [0, 0, 3, 5], &[])
            .is_err()
    );
    let missing_usage = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hiz missing-usage input"),
        size: wgpu::Extent3d {
            width: 3,
            height: 5,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    assert!(
        kernel
            .encode(&gpu, &mut encoder, &view, &missing_usage, [0, 0, 3, 5], &[])
            .is_err()
    );
    Ok(())
}
#[test]
#[ignore = "requires a real GPU adapter"]
fn nonzero_viewport_reads_only_requested_depth_region() -> Result<()> {
    let gpu = gpu()?;
    let kernel = HiZKernel::new(&gpu)?;
    let view = kernel.create_view(&gpu, [3, 5], 2)?;
    let mut full = vec![1.0; 5 * 7];
    let mut region = Vec::new();
    for y in 0..5 {
        for x in 0..3 {
            let value = ((x + y * 3) as f32) / 20.0;
            full[((y + 1) * 5 + (x + 1)) as usize] = value;
            region.push(value);
        }
    }
    let depth = depth_texture(&gpu, [5, 7], &full);
    let queries = [
        VisibilityQuery::new([0, 0, 3, 5], 0.5, false),
        VisibilityQuery::new([2, 4, 3, 5], 0.5, false),
    ];
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let output = kernel.encode(&gpu, &mut encoder, &view, &depth, [1, 1, 3, 5], &queries)?;
    gpu.queue.submit([encoder.finish()]);
    gpu.wait_idle()?;
    let expected = reference_pyramid([3, 5], &region)?;
    for (level, &dimensions) in view.mip_dimensions().iter().enumerate() {
        assert_eq!(
            read_mip(&gpu, view.texture(), level as u32, dimensions)?,
            expected[level]
        );
    }
    let bytes = read_buffer(&gpu, &output.buffer, 8)?;
    for (i, query) in queries.iter().enumerate() {
        let actual = u32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into()?);
        assert_eq!(actual, reference_visibility([3, 5], &expected, *query)?);
    }
    Ok(())
}
