//! Native resource GPU checks only; these do not establish material/scene IBL.
//! Run explicitly on a real adapter with --ignored --test-threads=1 --nocapture.
use anyhow::Result;
use engine_render_pipeline::{GpuContext, indirect::*, readback::read_rgba16f};
use engine_render_wgpu::WgpuOptions;

fn environment() -> EnvironmentData {
    let mips = (0..3)
        .map(|mip| {
            let edge = 4 >> mip;
            std::array::from_fn(|face| {
                let pixels = (0..edge)
                    .flat_map(|y| {
                        (0..edge).map(move |x| {
                            [
                                face as f32 + 1.0,
                                mip as f32 + 0.125 * x as f32,
                                2.0 + 0.5 * y as f32,
                            ]
                        })
                    })
                    .collect();
                RadianceImage::new([edge; 2], pixels).unwrap()
            })
        })
        .collect();
    EnvironmentData::new(DiffusePolynomial::new([[0.0; 3]; 9]).unwrap(), mips).unwrap()
}

fn gpu() -> Result<GpuContext> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    eprintln!("indirect resource adapter: {:?}", gpu.adapter.get_info());
    check_capabilities(&gpu)?;
    Ok(gpu)
}

fn read_face(
    gpu: &GpuContext,
    texture: &wgpu::Texture,
    mip: u32,
    face: u32,
) -> Result<Vec<[f32; 4]>> {
    let size = texture.width() >> mip;
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("indirect mip readback staging texture"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: mip,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: face,
            },
            aspect: wgpu::TextureAspect::All,
        },
        target.as_image_copy(),
        target.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    Ok(read_rgba16f(gpu, &target)?.pixels)
}

#[test]
#[ignore = "requires a real GPU; native uploads and cube sampling, not material integration"]
fn every_cube_face_mip_and_lightmap_is_uploaded_without_srgb_or_exposure() -> Result<()> {
    let gpu = gpu()?;
    let source = environment();
    let mut map_pixels = vec![[4.0, 0.25, 32.0]; 6];
    map_pixels[5] = [1.9999, 0.9999, 65_504.0];
    let map = RadianceImage::new([3, 2], map_pixels)?;
    let lut = BrdfLut::prepare(8, 128)?;
    let resources = GpuIndirectResources::prepare(
        &gpu,
        std::slice::from_ref(&source),
        std::slice::from_ref(&map),
        &lut,
    )?;
    for (mip, faces) in source.mips().iter().enumerate() {
        for (face, image) in faces.iter().enumerate() {
            let actual = read_face(
                &gpu,
                &resources.environments[0].texture,
                mip as u32,
                face as u32,
            )?;
            assert_eq!(actual.len(), image.pixels().len());
            for (actual, expected) in actual.iter().zip(image.pixels()) {
                assert_eq!(&actual[..3], expected, "mip {mip}, face {face}");
                assert_eq!(actual[3], 1.0);
            }
        }
    }
    let actual = read_rgba16f(&gpu, &resources.lightmaps[0].texture)?;
    assert_eq!(actual.size, [3, 2]);
    for pixel in &actual.pixels[..5] {
        assert_eq!(*pixel, [4.0, 0.25, 32.0, 1.0]);
    }
    assert_eq!(actual.pixels[5], [2.0, 1.0, 65_504.0, 1.0]);
    sample_cube_view(&gpu, &resources)?;
    // Preparing/dropping a different set does not resize or mutate a shared set.
    let replacement = GpuIndirectResources::prepare(
        &gpu,
        &[],
        &[RadianceImage::new([1, 1], vec![[0.0; 3]])?],
        &lut,
    )?;
    drop(replacement);
    assert_eq!(
        read_face(&gpu, &resources.environments[0].texture, 2, 5)?[0],
        [6.0, 2.0, 2.0, 1.0]
    );
    gpu.check_errors()
}

fn sample_cube_view(gpu: &GpuContext, resources: &GpuIndirectResources) -> Result<()> {
    // Sample actual Cube bindings at axis centers with explicit LOD. This checks
    // view construction/face routing in addition to the copy-based mip oracle.
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("indirect cube view oracle"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
@group(0) @binding(0) var environment: texture_cube<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@compute @workgroup_size(1) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let directions = array<vec3<f32>, 6>(vec3<f32>(1,0,0), vec3<f32>(-1,0,0),
        vec3<f32>(0,1,0), vec3<f32>(0,-1,0), vec3<f32>(0,0,1), vec3<f32>(0,0,-1));
        let off_axis = array<vec3<f32>, 6>(vec3<f32>(1,0.5,-0.5), vec3<f32>(-1,0.5,0.5),
        vec3<f32>(0.5,1,-0.5), vec3<f32>(0.5,-1,0.5), vec3<f32>(0.5,0.5,1), vec3<f32>(-0.5,0.5,-1));
    let row = id.x / 6u;
    let direction = select(directions[id.x % 6u], off_axis[id.x % 6u], row == 3u);
    let lod = select(f32(row), select(0.0, 0.5, row == 4u), row >= 3u);
    output[id.x] = textureSampleLevel(environment, linear_sampler, direction, lod);
}
"#
                .into(),
            ),
        });
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("indirect cube view oracle"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
    let bytes = 30 * 16;
    let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sampled indirect cube results"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sampled indirect cube readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("indirect cube view oracle"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&resources.environments[0].view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&resources.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(30, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, bytes);
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    gpu.wait_idle()?;
    rx.recv()??;
    let mapped = readback
        .slice(..)
        .get_mapped_range()
        .map_err(|e| anyhow::anyhow!("cube sample readback: {e:?}"))?;
    for (index, pixel) in mapped.chunks_exact(16).enumerate() {
        let actual: [f32; 4] = std::array::from_fn(|c| {
            f32::from_le_bytes(pixel[c * 4..c * 4 + 4].try_into().unwrap())
        });
        let mip = index / 6;
        let edge = (4 >> mip.min(2)) as f32;
        let [g, b] = match mip {
            3 => [0.3125, 2.25], // UV (.75,.25) at mip zero.
            4 => [0.625, 2.5],   // Equal blend of center samples at mips 0 and 1.
            _ => [
                mip as f32 + 0.125 * (edge - 1.0) * 0.5,
                2.0 + 0.5 * (edge - 1.0) * 0.5,
            ],
        };
        let expected = [index as f32 % 6.0 + 1.0, g, b, 1.0];
        for c in 0..4 {
            assert!(
                (actual[c] - expected[c]).abs() < 0.002,
                "cube sample {index}: {actual:?} {expected:?}"
            );
        }
    }
    drop(mapped);
    readback.unmap();
    Ok(())
}

// Independent f64 light-direction integration of the actual shader clamps.
fn reference(nv: f64, roughness: f64, resolution: u32) -> [f64; 2] {
    let a2 = roughness.max(0.045).powi(4);
    let shader_nv = nv.max(0.0001);
    let vx = (1.0 - nv * nv).sqrt();
    let gv = 2.0 * shader_nv / (shader_nv + (a2 + (1.0 - a2) * shader_nv * shader_nv).sqrt());
    let mut result = [0.0; 2];
    // Independent LIGHT-direction quadrature, with explicit change-of-variable
    // Jacobians. Cluster z around the mirror ray's z=nv, and azimuth around pi,
    // so low-roughness grazing lobes cannot fall between a uniform phi grid.
    // Symmetry about y=0 lets [0,pi] represent the full azimuth with factor 2.
    for side in 0..2 {
        let span = if side == 0 { nv } else { 1.0 - nv };
        for y in 0..resolution {
            let t = (y as f64 + 0.5) / f64::from(resolution);
            let nl = if side == 0 {
                nv - span * t * t
            } else {
                nv + span * t * t
            };
            let dz = 2.0 * span * t;
            let radial = (1.0 - nl * nl).max(0.0).sqrt();
            for x in 0..resolution {
                let u = (x as f64 + 0.5) / f64::from(resolution);
                let phi = std::f64::consts::PI * (1.0 - u.powi(4));
                let dphi = 4.0 * std::f64::consts::PI * u.powi(3);
                let hx = vx + radial * phi.cos();
                let hy = radial * phi.sin();
                let hz = nv + nl;
                let inverse = (hx * hx + hy * hy + hz * hz).sqrt().max(0.000001).recip();
                let nh = (hz * inverse).max(0.0);
                let vh = ((vx * hx + nv * hz) * inverse).max(0.0);
                let denom = nh * nh * (a2 - 1.0) + 1.0;
                let d = a2 / (std::f64::consts::PI * denom * denom).max(0.0000001);
                let gl = 2.0 * nl / (nl + (a2 + (1.0 - a2) * nl * nl).sqrt()).max(0.000001);
                let weight =
                    d * gv * gl * nl / (4.0 * shader_nv * nl).max(0.000001) * 2.0 * dz * dphi;
                let fc = (1.0 - vh).clamp(0.0, 1.0).powi(5);
                result[0] += weight * (1.0 - fc);
                result[1] += weight * fc;
            }
        }
    }
    result.map(|v| v / f64::from(resolution).powi(2))
}

#[test]
#[ignore = "requires a real GPU; reads the uploaded BRDF LUT against CPU quadrature"]
fn uploaded_brdf_lut_matches_independent_cpu_integral() -> Result<()> {
    let gpu = gpu()?;
    let lut = BrdfLut::prepare(15, 4096)?;
    let resources = GpuIndirectResources::prepare(&gpu, &[], &[], &lut)?;
    let actual = read_rgba16f(&gpu, &resources.brdf.texture)?;
    assert_eq!(actual.size, [15, 15]);
    for (pixel, expected) in actual.pixels.iter().zip(lut.values()) {
        assert!((pixel[0] - expected[0]).abs() < 0.002);
        assert!((pixel[1] - expected[1]).abs() < 0.002);
        assert_eq!((pixel[2], pixel[3]), (0.0, 1.0));
    }
    // With size 15, row 0 exercises the shader's .045 floor, row 1 is .1.
    // The first x column is grazing; all use the actual uploaded half texture.
    for (x, y) in [(0, 0), (12, 0), (0, 1), (12, 1), (3, 8), (7, 12), (14, 14)] {
        let nv = (x as f64 + 0.5) / 15.0;
        let roughness = (y as f64 + 0.5) / 15.0;
        let coarse = reference(nv, roughness, 512);
        let fine = reference(nv, roughness, 1024);
        let pixel = actual.pixel(x, y);
        for c in 0..2 {
            assert!(
                (fine[c] - coarse[c]).abs() < 0.001 + 0.01 * fine[c].abs(),
                "unresolved GPU oracle ({x},{y}): {coarse:?} -> {fine:?}"
            );
            // 0.001 additionally covers half upload quantization.
            assert!(
                (f64::from(pixel[c]) - fine[c]).abs() < 0.003 + 0.04 * fine[c].abs(),
                "regularized BRDF ({x},{y}): {pixel:?} {fine:?}"
            );
        }
    }
    gpu.check_errors()
}
