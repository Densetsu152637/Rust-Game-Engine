#![allow(clippy::too_many_lines)]
//! Actual GPU pixels for compiled graphs versus the hand-written built-in shader.
//! Run with `cargo test -p engine-material-graph --test gpu_material_graph -- --ignored --test-threads=1`.

use anyhow::Result;
use engine_graph::{GraphDocument, GraphEdge, GraphNode, PortRef};
use engine_material_graph::compile_material;
use engine_render_pipeline::{
    ENGINE_VERTEX_LAYOUT, EngineVertex, GpuMesh, GpuTexture, LIT_WGSL, MaterialTarget,
    compose_material,
};
use engine_render_wgpu::{GpuContext, WgpuOptions};
use glam::Mat4;
use serde_json::json;
use std::collections::BTreeMap;
use wgpu::util::DeviceExt;

fn graph() -> GraphDocument {
    let mut doc = GraphDocument::new("material");
    for (id, kind) in [
        ("tex", "texture_color"),
        ("tint", "tint"),
        ("rgb", "multiply_vec3"),
        ("alpha", "multiply_float"),
        ("out", "material_output"),
    ] {
        doc.nodes.push(GraphNode {
            id: id.into(),
            node_type: kind.to_owned(),
            ports: vec![],
            params: BTreeMap::new(),
            position: None,
        });
    }
    for (from, port, to, input) in [
        ("tex", "color", "rgb", "a"),
        ("tint", "color", "rgb", "b"),
        ("tex", "alpha", "alpha", "a"),
        ("tint", "alpha", "alpha", "b"),
        ("rgb", "value", "out", "base_color"),
        ("alpha", "value", "out", "alpha"),
    ] {
        doc.edges.push(GraphEdge {
            from: PortRef::new(from, port),
            to: PortRef::new(to, input),
        });
    }
    doc
}

fn override_graph(physical: bool) -> GraphDocument {
    let mut doc = GraphDocument::new("material");
    let mut nodes = vec![
        (
            "red",
            "vec3",
            BTreeMap::from([("value".to_owned(), json!([0.8, 0.1, 0.2]))]),
        ),
        (
            "opacity",
            "scalar",
            BTreeMap::from([("value".to_owned(), json!(0.35))]),
        ),
    ];
    if physical {
        nodes.extend([
            (
                "metal",
                "scalar",
                BTreeMap::from([("value".to_owned(), json!(0.85))]),
            ),
            (
                "rough",
                "scalar",
                BTreeMap::from([("value".to_owned(), json!(0.15))]),
            ),
        ]);
    }
    nodes.push(("out", "material_output", BTreeMap::new()));
    doc.nodes = nodes
        .into_iter()
        .map(|(id, kind, params)| GraphNode {
            id: id.into(),
            node_type: kind.to_owned(),
            ports: vec![],
            params,
            position: None,
        })
        .collect();
    let mut edges = vec![("red", "base_color"), ("opacity", "alpha")];
    if physical {
        edges.extend([("metal", "metallic"), ("rough", "roughness")]);
    }
    doc.edges = edges
        .into_iter()
        .map(|(from, to)| GraphEdge {
            from: PortRef::new(from, "value"),
            to: PortRef::new("out", to),
        })
        .collect();
    doc
}

fn reference_override(physical: bool) -> String {
    let mut source = LIT_WGSL
        .replace(
            "surface.albedo = color.rgb;",
            "surface.albedo = vec3<f32>(0.8, 0.1, 0.2);",
        )
        .replace(
            "select(shaded, color.rgb, material.options.x > 0.5)",
            "select(shaded, vec3<f32>(0.8, 0.1, 0.2), material.options.x > 0.5)",
        )
        .replace(
            "return vec4<f32>(result, color.a);",
            "return vec4<f32>(result, 0.35);",
        );
    if physical {
        source = source
            .replace(
                "surface.metallic = material.options.y * select(1.0, mr.b, maps.z > 0.5);",
                "surface.metallic = 0.85;",
            )
            .replace(
                "surface.roughness = material.options.z * select(1.0, mr.g, maps.z > 0.5);",
                "surface.roughness = 0.15;",
            );
    }
    source
}

fn quad(gpu: &GpuContext, vertex_color: [f32; 4]) -> Result<std::sync::Arc<GpuMesh>> {
    let v = |x, y, u, vv| EngineVertex {
        position: [x, y, 0.0],
        uv: [u, vv],
        color: vertex_color,
        normal: [0.0, 0.0, 1.0],
    };
    GpuMesh::from_engine_vertices(
        &gpu.device,
        "material graph gpu quad",
        &[
            v(-1.0, -1.0, 0.0, 1.0),
            v(1.0, -1.0, 1.0, 1.0),
            v(1.0, 1.0, 1.0, 0.0),
            v(-1.0, 1.0, 0.0, 0.0),
        ],
        &[0, 1, 2, 0, 2, 3],
    )
}

fn uniform_layout(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn texture_layout(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn draw_sprite(gpu: &GpuContext, source: &str, vertex_color: [f32; 4]) -> Result<[u8; 4]> {
    const SIZE: u32 = 32;
    let device = &gpu.device;
    let composed = compose_material(source, MaterialTarget::Sprite)?;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("material graph sprite"),
        source: wgpu::ShaderSource::Wgsl(composed.into()),
    });
    let view_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sprite view"),
        entries: &[uniform_layout(0, wgpu::ShaderStages::VERTEX)],
    });
    let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sprite images"),
        entries: &[
            texture_layout(0),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            texture_layout(2),
            texture_layout(3),
        ],
    });
    let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sprite material"),
        entries: &[
            uniform_layout(0, wgpu::ShaderStages::FRAGMENT),
            uniform_layout(2, wgpu::ShaderStages::FRAGMENT),
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("sprite graph layout"),
        bind_group_layouts: &[
            Some(&view_layout),
            Some(&image_layout),
            Some(&material_layout),
        ],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sprite graph pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(ENGINE_VERTEX_LAYOUT)],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    let clip = Mat4::IDENTITY.to_cols_array();
    let material = [[0.5f32, 0.75, 1.0, 0.8], [1.0, 0.3, 0.6, 0.0]];
    let maps = [0.0f32, 1.0, 0.0, 0.0];
    let buffer = |label, bytes: &[u8]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        })
    };
    let view_buffer = buffer("sprite clip", bytemuck::cast_slice(&clip));
    let material_buffer = buffer("sprite material", bytemuck::cast_slice(&material));
    let maps_buffer = buffer("sprite maps", bytemuck::cast_slice(&maps));
    let view_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sprite view bind"),
        layout: &view_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: view_buffer.as_entire_binding(),
        }],
    });
    let image = GpuTexture::rgba8(
        device,
        &gpu.queue,
        "sprite color",
        [1, 1],
        &[128, 64, 32, 128],
    )?;
    let data_image = GpuTexture::rgba8(
        device,
        &gpu.queue,
        "sprite data",
        [1, 1],
        &[255, 255, 255, 255],
    )?;
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let image_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sprite images bind"),
        layout: &image_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&image.view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&data_image.linear_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&data_image.linear_view),
            },
        ],
    });
    let material_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sprite material bind"),
        layout: &material_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: material_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: maps_buffer.as_entire_binding(),
            },
        ],
    });
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sprite graph output"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output.create_view(&Default::default());
    let mesh = quad(gpu, vertex_color)?;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("sprite graph"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sprite graph pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &view_group, &[]);
        pass.set_bind_group(1, &image_group, &[]);
        pass.set_bind_group(2, &material_group, &[]);
        mesh.draw(&mut pass, 0..1);
    }
    gpu.queue.submit([encoder.finish()]);
    let pixels = engine_render_wgpu::read_texture_rgba(gpu, &output, SIZE, SIZE)?;
    Ok(pixels[((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize..][..4].try_into()?)
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn sprite_graph_matches_handwritten_material_pixels_including_alpha() -> Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let source = compile_material(&graph()).unwrap().source;
    let graph_pixel = draw_sprite(&gpu, &source, [1.0; 4])?;
    let reference_pixel = draw_sprite(&gpu, LIT_WGSL, [1.0; 4])?;
    for (actual, expected) in graph_pixel.into_iter().zip(reference_pixel) {
        assert!(
            actual.abs_diff(expected) <= 1,
            "{graph_pixel:?} versus {reference_pixel:?}"
        );
    }
    assert!(
        (graph_pixel[3] as i16 - 102).abs() <= 2,
        "alpha: {graph_pixel:?}"
    );
    Ok(())
}

fn draw_mesh(
    gpu: &GpuContext,
    source: &str,
    unlit: bool,
    vertex_color: [f32; 4],
) -> Result<[f32; 4]> {
    use engine_render_pipeline::{
        Light, LightList, MaterialParams, NoHost, OutputTarget, SceneFrame, SceneMesh,
        ScenePipeline, ViewData, ViewId, Viewport,
    };
    use glam::Vec3;

    let mut pipeline = ScenePipeline::new(gpu.clone())?;
    let mesh = quad(gpu, vertex_color)?;
    let image = GpuTexture::rgba8(
        &gpu.device,
        &gpu.queue,
        "mesh graph color",
        [1, 1],
        &[128, 64, 32, 128],
    )?;
    let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mesh graph output"),
        size: wgpu::Extent3d {
            width: 32,
            height: 32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target = OutputTarget {
        view: output.create_view(&Default::default()),
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        size: [32, 32],
        viewport: Viewport::full([32, 32]),
    };
    let projection =
        glam::camera::rh::proj::directx::perspective(std::f32::consts::FRAC_PI_3, 1.0, 0.1, 100.0);
    let view = ViewData::new(ViewId::MAIN, Mat4::IDENTITY, projection, [32, 32], [32, 32])?;
    let draw = SceneMesh {
        id: "material graph mesh",
        mesh,
        world_from_local: Mat4::from_translation(Vec3::new(0.0, 0.0, -2.0)),
        shader: Some(source),
        params: MaterialParams {
            tint: [0.5, 0.75, 1.0, 0.8],
            unlit,
            metallic: 0.05,
            roughness: 0.8,
            ..Default::default()
        },
        base_color: Some(image),
        metallic_roughness: None,
        normal_map: None,
    };
    let lights = LightList {
        lights: vec![Light::directional(Vec3::NEG_Z, Vec3::ONE, 10.0)],
    };
    pipeline.render_view(
        &SceneFrame {
            view: &view,
            meshes: &[draw],
            lights: &lights,
            world: None,
        },
        &target,
        &mut NoHost,
    )?;
    Ok(pipeline.read_scene_hdr(ViewId::MAIN)?.pixel(16, 16))
}

fn assert_hdr_close(actual: [f32; 4], expected: [f32; 4]) {
    for (a, b) in actual.into_iter().zip(expected) {
        assert!((a - b).abs() <= 0.005, "{actual:?} versus {expected:?}");
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn mesh_default_graph_matches_handwritten_material_hdr_pixels() -> Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let graph_source = compile_material(&graph()).unwrap().source;
    for unlit in [true, false] {
        assert_hdr_close(
            draw_mesh(&gpu, &graph_source, unlit, [1.0; 4])?,
            draw_mesh(&gpu, LIT_WGSL, unlit, [1.0; 4])?,
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn mesh_graph_override_controls_unlit_color_and_lit_physical_response() -> Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let base_source = compile_material(&override_graph(false)).unwrap().source;
    let physical_source = compile_material(&override_graph(true)).unwrap().source;

    let unlit = draw_mesh(&gpu, &base_source, true, [0.7, 0.3, 0.9, 0.6])?;
    assert_hdr_close(
        unlit,
        draw_mesh(&gpu, &reference_override(false), true, [0.7, 0.3, 0.9, 0.6])?,
    );
    let builtin = draw_mesh(&gpu, LIT_WGSL, true, [0.7, 0.3, 0.9, 0.6])?;
    assert!(
        (unlit[0] - builtin[0]).abs() > 0.1,
        "graph base color ignored: {unlit:?} versus {builtin:?}"
    );
    assert!(
        (unlit[3] - 0.35).abs() <= 0.01,
        "graph alpha ignored: {unlit:?}"
    );

    let lit = draw_mesh(&gpu, &physical_source, false, [0.7, 0.3, 0.9, 0.6])?;
    assert_hdr_close(
        lit,
        draw_mesh(&gpu, &reference_override(true), false, [0.7, 0.3, 0.9, 0.6])?,
    );
    let base_only = draw_mesh(&gpu, &base_source, false, [0.7, 0.3, 0.9, 0.6])?;
    assert!(
        lit[..3]
            .iter()
            .zip(&base_only[..3])
            .any(|(a, b)| (a - b).abs() > 0.01),
        "metallic and roughness overrides had no visible effect: {lit:?} versus {base_only:?}"
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn sprite_graph_override_changes_color_and_alpha_against_handwritten_reference() -> Result<()> {
    let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
    let graph_source = compile_material(&override_graph(false)).unwrap().source;
    let graph_pixel = draw_sprite(&gpu, &graph_source, [0.7, 0.3, 0.9, 0.6])?;
    let reference_pixel = draw_sprite(&gpu, &reference_override(false), [0.7, 0.3, 0.9, 0.6])?;
    let builtin_pixel = draw_sprite(&gpu, LIT_WGSL, [0.7, 0.3, 0.9, 0.6])?;
    for (actual, expected) in graph_pixel.into_iter().zip(reference_pixel) {
        assert!(
            actual.abs_diff(expected) <= 1,
            "{graph_pixel:?} versus {reference_pixel:?}"
        );
    }
    assert!(
        graph_pixel[0].abs_diff(builtin_pixel[0]) > 20,
        "graph override was ignored: {graph_pixel:?} versus {builtin_pixel:?}"
    );
    assert!(
        (graph_pixel[3] as i16 - 89).abs() <= 2,
        "graph alpha: {graph_pixel:?}"
    );
    Ok(())
}
