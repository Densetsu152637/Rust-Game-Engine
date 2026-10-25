//! These tests intentionally fail if no functioning hardware/software GPU is
//! available. They are ignored in ordinary headless CI; run with --ignored.
use engine_desktop::render_pipeline::{FeatureContext, FeatureDescriptor, PipelineFeature, Stage};
use engine_desktop::*;
use engine_project::{
    Camera2D,
    content::{FontAtlas, FontGlyph, Texture, sprite_quad, text_mesh},
    runtime::{BlendMode, Draw, DrawMotion, DrawSpace, DrawState, RenderFrame},
};
use std::{collections::BTreeMap, sync::Arc};

fn camera() -> Camera2D {
    Camera2D {
        logical_size: [64, 64],
        pixels_per_unit: 1.0,
        ..Default::default()
    }
}
fn pixel(pixels: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
    pixels[(y * width + x) * 4..(y * width + x + 1) * 4]
        .try_into()
        .unwrap()
}

fn save_evidence(name: &str, pixels: &[u8], width: u32, height: u32) -> anyhow::Result<()> {
    if let Some(directory) = std::env::var_os("ENGINE_GPU_EVIDENCE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory)?;
        image::save_buffer(
            directory.join(name),
            pixels,
            width,
            height,
            image::ColorType::Rgba8,
        )?;
    }
    Ok(())
}

#[test]
#[ignore = "requires a real native wgpu adapter; run cargo test -p engine-desktop --test gpu_pixels -- --ignored"]
fn textured_sprite_font_cache_order_clip_resize_and_limits() -> anyhow::Result<()> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    eprintln!("GPU pixel test adapter: {:?}", gpu.adapter.get_info());
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(
        gpu,
        target.info,
        RenderLimits {
            max_draws: 2,
            ..Default::default()
        },
    )?;
    let atlas = Arc::new(Texture {
        width: 2,
        height: 2,
        pixels: vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ],
    });
    let sprite = Arc::new(sprite_quad(
        [0.0, 0.0, 0.5],
        [32.0, 32.0],
        [0.5, 0.5],
        [0, 0, 2, 2],
        &atlas,
        [1.0; 4],
    )?);
    let mut frame = RenderFrame {
        camera: camera(),
        draws: vec![Draw {
            id: "sprite".into(),
            mesh: sprite,
            texture: Some(atlas.clone()),
            layer: 0,
            space: DrawSpace::World,
            clip: None,
            state: DrawState::default(),
            motion: None,
            offset: [0.0; 3],
        }],
    };
    let first = renderer.render(&frame, &target.view, None)?;
    let pixels = target.read_rgba(&renderer.gpu)?;
    assert_eq!(pixel(&pixels, 64, 20, 20), [255, 0, 0, 255]);
    assert_eq!(pixel(&pixels, 64, 44, 20), [0, 255, 0, 255]);
    assert_eq!(pixel(&pixels, 64, 20, 44), [0, 0, 255, 255]);
    assert_eq!(pixel(&pixels, 64, 44, 44), [255, 255, 255, 255]);
    assert_eq!(pixel(&pixels, 64, 2, 2), [0, 0, 0, 255]);
    assert_eq!((first.mesh_uploads, first.texture_uploads), (1, 1));
    let unchanged = renderer.render(&frame, &target.view, None)?;
    assert_eq!((unchanged.mesh_uploads, unchanged.texture_uploads), (0, 0));

    // Real UTF-8 atlas text, with UI-down coordinates and clipping. Both glyphs
    // sample the atlas's white bottom-right cell and tint it yellow.
    let font = FontAtlas {
        texture: "atlas".into(),
        line_height: 2.0,
        fallback: '?' as u32,
        glyphs: BTreeMap::from([
            (
                '?' as u32,
                FontGlyph {
                    source: [1, 1, 1, 1],
                    advance: 1.0,
                    bearing: [0.0; 2],
                },
            ),
            (
                'Ω' as u32,
                FontGlyph {
                    source: [1, 1, 1, 1],
                    advance: 1.0,
                    bearing: [0.0; 2],
                },
            ),
        ]),
    };
    let text = Arc::new(text_mesh(
        &font,
        &atlas,
        "Ω?",
        [4.0, 4.0],
        0.0,
        8.0,
        [1.0, 1.0, 0.0, 1.0],
    )?);
    frame.draws.push(Draw {
        id: "text".into(),
        mesh: text,
        texture: Some(atlas.clone()),
        layer: 10,
        space: DrawSpace::Ui,
        clip: Some([4.0, 4.0, 10.0, 8.0]),
        state: DrawState::default(),
        motion: None,
        offset: [0.0; 3],
    });
    let updated = renderer.render(&frame, &target.view, None)?;
    let pixels = target.read_rgba(&renderer.gpu)?;
    assert_eq!(pixel(&pixels, 64, 5, 5), [255, 255, 0, 255]);
    assert_eq!(pixel(&pixels, 64, 13, 5), [255, 255, 0, 255]);
    assert_eq!(pixel(&pixels, 64, 14, 5), [0, 0, 0, 255]);
    assert_eq!((updated.mesh_uploads, updated.texture_uploads), (1, 0));
    assert_eq!(updated.cached_textures, 1);
    save_evidence("atlas-font.png", &pixels, 64, 64)?;

    // Layer+ID ordering is stable even if producer iteration reverses.
    frame.draws.reverse();
    renderer.render(&frame, &target.view, None)?;
    assert_eq!(pixels, target.read_rgba(&renderer.gpu)?);
    let mut excess = frame.draws[0].clone();
    excess.id = "excess".into();
    frame.draws.push(excess);
    assert!(renderer.render(&frame, &target.view, None).is_err());
    frame.draws.pop();
    let original = frame.draws[0].id.clone();
    frame.draws[0].id = frame.draws[1].id.clone();
    assert!(renderer.render(&frame, &target.view, None).is_err());
    frame.draws[0].id = original;
    let invalid = Arc::new(Texture {
        width: 2,
        height: 2,
        pixels: vec![0; 3],
    });
    frame.draws[0].texture = Some(invalid);
    assert!(renderer.render(&frame, &target.view, None).is_err());
    frame.draws[0].texture = Some(atlas);

    renderer.resize(128, 96)?;
    let resized = OffscreenTarget::new(&renderer.gpu, 128, 96)?;
    renderer.render(&frame, &resized.view, None)?;
    let pixels = resized.read_rgba(&renderer.gpu)?;
    assert_eq!(pixel(&pixels, 128, 0, 0), [0, 0, 0, 255]);
    assert_eq!(pixel(&pixels, 128, 32 + 20, 16 + 20), [255, 0, 0, 255]);
    frame.draws.clear();
    let empty = renderer.render(&frame, &resized.view, None)?;
    assert_eq!((empty.cached_meshes, empty.cached_textures), (0, 0));
    renderer.gpu.wait_idle()?;
    Ok(())
}

#[test]
#[ignore = "requires a real native wgpu adapter"]
fn an_interpolated_sprite_renders_the_same_pixels_as_one_baked_at_the_blended_position()
-> anyhow::Result<()> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let atlas = Arc::new(Texture {
        width: 2,
        height: 2,
        pixels: vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ],
    });
    // ppu 1, pixel snap on: a sprite at x = 4.4 draws at the pixel grid's 4.0.
    let sprite = |x: f32| -> anyhow::Result<Draw> {
        let motion = DrawMotion {
            position: [x, 0.0, 0.0],
            pixel_snap: true,
        };
        Ok(Draw {
            id: "sprite:a".into(),
            mesh: Arc::new(sprite_quad(
                motion.baked_position(1.0),
                [16.0, 16.0],
                [0.5, 0.5],
                [0, 0, 2, 2],
                &atlas,
                [1.0; 4],
            )?),
            texture: Some(atlas.clone()),
            layer: 0,
            space: DrawSpace::World,
            clip: None,
            state: DrawState::default(),
            motion: Some(motion),
            offset: [0.0; 3],
        })
    };
    let frame = |draw| RenderFrame {
        camera: camera(),
        draws: vec![draw],
    };
    let previous = frame(sprite(0.0)?);
    let current = frame(sprite(8.8)?);
    let blended = interpolate_frame(&previous, &current, 0.5);
    renderer.render(&blended, &target.view, None)?;
    let interpolated = target.read_rgba(&renderer.gpu)?;
    // The reference sprite sits exactly where the blend snaps to, no offset.
    let mut reference = frame(sprite(4.0)?);
    reference.draws[0].motion = None;
    renderer.render(&reference, &target.view, None)?;
    let baked = target.read_rgba(&renderer.gpu)?;
    assert_eq!(interpolated, baked);
    // And it really moved: not where the current tick's mesh was baked (x = 9).
    renderer.render(&current, &target.view, None)?;
    assert_ne!(interpolated, target.read_rgba(&renderer.gpu)?);
    save_evidence("interpolated-sprite.png", &interpolated, 64, 64)?;
    // alpha = 1 is the current frame, pixel for pixel.
    let at_one = interpolate_frame(&previous, &current, 1.0);
    renderer.render(&at_one, &target.view, None)?;
    let one = target.read_rgba(&renderer.gpu)?;
    renderer.render(&current, &target.view, None)?;
    assert_eq!(one, target.read_rgba(&renderer.gpu)?);
    renderer.gpu.wait_idle()?;
    Ok(())
}

/// A game overlay feature (the former native render pass): its own
/// pipeline drawn into the output target after the engine draws.
struct CustomTriangle {
    pipeline: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
}

#[test]
#[ignore = "requires a real native wgpu adapter"]
fn depth_and_straight_alpha_are_distinct_draw_states() -> anyhow::Result<()> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let white = Texture {
        width: 1,
        height: 1,
        pixels: vec![255; 4],
    };
    let opaque = DrawState {
        blend: BlendMode::Opaque,
        depth_test: true,
        depth_write: true,
    };
    let draw = |id: &str, layer, z, color, state| -> anyhow::Result<Draw> {
        Ok(Draw {
            id: id.into(),
            layer,
            mesh: Arc::new(sprite_quad(
                [0.0, 0.0, z],
                [64.0, 64.0],
                [0.5, 0.5],
                [0, 0, 1, 1],
                &white,
                color,
            )?),
            texture: None,
            space: DrawSpace::World,
            clip: None,
            state,
            motion: None,
            offset: [0.0; 3],
        })
    };
    let mut frame = RenderFrame {
        camera: camera(),
        draws: vec![
            draw("near", 0, 0.2, [1.0, 0.0, 0.0, 1.0], opaque)?,
            draw("far", 1, 0.8, [0.0, 0.0, 1.0, 1.0], opaque)?,
        ],
    };
    renderer.render(&frame, &target.view, None)?;
    assert_eq!(
        pixel(&target.read_rgba(&renderer.gpu)?, 64, 32, 32),
        [255, 0, 0, 255]
    );
    frame.draws[1].state = DrawState::default();
    renderer.render(&frame, &target.view, None)?;
    assert_eq!(
        pixel(&target.read_rgba(&renderer.gpu)?, 64, 32, 32),
        [0, 0, 255, 255]
    );
    frame.draws[1] = draw("alpha", 1, 0.8, [0.0, 1.0, 0.0, 0.5], DrawState::default())?;
    renderer.render(&frame, &target.view, None)?;
    let color = pixel(&target.read_rgba(&renderer.gpu)?, 64, 32, 32);
    assert!((187..=188).contains(&color[0]) && (187..=188).contains(&color[1]));
    assert_eq!(&color[2..], &[0, 255]);
    frame.draws[0].id = "a".into();
    frame.draws[1].id = "z".into();
    frame.draws[1].layer = 0;
    frame.draws.reverse();
    renderer.render(&frame, &target.view, None)?;
    assert_eq!(
        pixel(&target.read_rgba(&renderer.gpu)?, 64, 32, 32),
        color,
        "equal-layer painter order must follow stable IDs, not input iteration"
    );
    Ok(())
}
impl PipelineFeature for CustomTriangle {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("game.triangle", Stage::Overlay)
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> anyhow::Result<()> {
        let target = ctx.output()?.clone();
        if self
            .pipeline
            .as_ref()
            .is_none_or(|(f, _)| *f != target.format)
        {
            let shader=ctx.gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("external custom pipeline"),source:wgpu::ShaderSource::Wgsl(r#"
                @vertex fn vs(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32> {
                    let positions=array<vec2<f32>,3>(vec2<f32>(-0.8,-0.8),vec2<f32>(0.8,-0.8),vec2<f32>(0.0,0.8));
                    return vec4<f32>(positions[index],0.0,1.0);
                }
                @fragment fn fs()->@location(0) vec4<f32> {return vec4<f32>(1.0,0.0,1.0,1.0);}
            "#.into())});
            let pipeline = ctx
                .gpu
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("custom"),
                    layout: None,
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(target.format.into())],
                    }),
                    multiview_mask: None,
                    cache: None,
                });
            self.pipeline = Some((target.format, pipeline));
        }
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("external custom pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline.as_ref().unwrap().1);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

#[test]
#[ignore = "requires a real native wgpu adapter"]
fn external_overlay_feature_renders_pixels_and_reports_device_errors() -> anyhow::Result<()> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let mut features = PipelineFeatures::default();
    features.register(CustomTriangle { pipeline: None })?;
    assert!(
        features
            .register(CustomTriangle { pipeline: None })
            .is_err(),
        "duplicate feature names are rejected"
    );
    renderer.set_features(features)?;
    renderer.render(
        &RenderFrame {
            camera: camera(),
            draws: vec![],
        },
        &target.view,
        None,
    )?;
    let pixels = target.read_rgba(&renderer.gpu)?;
    assert_eq!(pixel(&pixels, 64, 32, 32), [255, 0, 255, 255]);
    assert_eq!(pixel(&pixels, 64, 0, 0), [0, 0, 0, 255]);
    save_evidence("overlay-feature.png", &pixels, 64, 64)?;
    // Uncaptured errors are reported, sticky, and prevent successful future
    // submission. This deliberately creates an invalid zero-sized texture.
    let _invalid = renderer
        .gpu
        .device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("intentional invalid texture"),
            size: wgpu::Extent3d {
                width: 0,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
    assert!(renderer.gpu.check_errors().is_err());
    assert!(
        renderer
            .render(
                &RenderFrame {
                    camera: camera(),
                    draws: vec![]
                },
                &target.view,
                None
            )
            .is_err()
    );
    Ok(())
}
