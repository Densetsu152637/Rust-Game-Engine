//! TrueType UI text through the real wgpu renderer. Ignored in ordinary headless
//! CI because it needs a native adapter; run with
//! `cargo test -p engine-desktop --test ttf_text -- --ignored --test-threads=1`.
use engine_desktop::*;
use engine_project::{
    Camera2D,
    content::{FontConfig, FontFace, parse_font_face},
    runtime::RenderFrame,
    ui_tree::{EngineTextBackend, UiLength, UiNode, UiRenderState, UiRoot, UiTree},
};
use std::{collections::BTreeMap, sync::Arc};

const WIDTH: usize = 160;
const HEIGHT: usize = 64;

fn backend() -> EngineTextBackend {
    let face: Arc<FontFace> =
        Arc::new(parse_font_face(epaint_default_fonts::UBUNTU_LIGHT.to_vec()).unwrap());
    EngineTextBackend::from_content(
        &BTreeMap::from([("ui".to_string(), face)]),
        &FontConfig::default(),
    )
    .unwrap()
}

fn label_root(text: &str) -> UiRoot {
    let mut label =
        UiNode::label("title", text).with_size(UiLength::Pixels(150.0), UiLength::Pixels(40.0));
    label.style.font = "ui".into();
    label.style.font_scale = 1.5;
    label.style.foreground = [1.0, 0.5, 0.0, 1.0];
    label.style.padding = 0.0;
    label.style.background = [0.0; 4];
    UiRoot {
        tree: UiTree { nodes: vec![label] },
        ..Default::default()
    }
}

fn frame(engine: &EngineTextBackend, text: &str) -> RenderFrame {
    engine.begin_frame();
    let draws = label_root(text)
        .extract_draws_with(
            "hud",
            [WIDTH as u32, HEIGHT as u32],
            &UiRenderState::default(),
            engine,
            &BTreeMap::new(),
        )
        .unwrap();
    RenderFrame {
        camera: Camera2D {
            logical_size: [WIDTH as u32, HEIGHT as u32],
            pixels_per_unit: 1.0,
            ..Default::default()
        },
        draws,
    }
}

fn lit(pixels: &[u8]) -> Vec<(usize, usize, [u8; 4])> {
    (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .filter_map(|(x, y)| {
            let p: [u8; 4] = pixels[(y * WIDTH + x) * 4..(y * WIDTH + x) * 4 + 4]
                .try_into()
                .unwrap();
            (p[0] > 24).then_some((x, y, p))
        })
        .collect()
}

#[test]
#[ignore = "requires a real native wgpu adapter; run cargo test -p engine-desktop --test ttf_text -- --ignored --test-threads=1"]
fn a_ttf_label_renders_orange_glyph_coverage_and_uploads_its_atlas_once() -> anyhow::Result<()> {
    let gpu = pollster::block_on(GpuContext::headless())?;
    eprintln!("TTF text GPU test adapter: {:?}", gpu.adapter.get_info());
    let target = OffscreenTarget::new(&gpu, WIDTH as u32, HEIGHT as u32)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let engine = backend();

    let first = renderer.render(&frame(&engine, "Hello GPU"), &target.view, None)?;
    let pixels = target.read_rgba(&renderer.gpu)?;
    assert_eq!(first.texture_uploads, 1, "one atlas page uploaded");
    let lit_pixels = lit(&pixels);
    assert!(
        (150..4000).contains(&lit_pixels.len()),
        "glyph coverage pixel count {}",
        lit_pixels.len()
    );
    // Coverage tints the white atlas with the orange foreground (0.5 linear encodes to about 188 in sRGB).
    let (_, _, brightest) = lit_pixels
        .iter()
        .max_by_key(|(_, _, p)| p[0])
        .copied()
        .unwrap();
    assert!(
        brightest[0] > 230 && (175..200).contains(&brightest[1]) && brightest[2] < 20,
        "{brightest:?}"
    );
    assert!(
        lit_pixels.iter().all(|(_, _, p)| p[2] < 20 && p[1] <= p[0]),
        "only orange-on-black"
    );
    // Text stays inside the label's 150 x 40 box at the top-left.
    assert!(lit_pixels.iter().all(|(x, y, _)| *x < 150 && *y < 40));
    assert!(
        lit_pixels.iter().any(|(x, _, _)| *x < 12),
        "text starts at the left edge"
    );
    if let Some(directory) = std::env::var_os("ENGINE_GPU_EVIDENCE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory)?;
        image::save_buffer(
            directory.join("ttf-label.png"),
            &pixels,
            WIDTH as u32,
            HEIGHT as u32,
            image::ColorType::Rgba8,
        )?;
    }

    // The same text again re-extracts new meshes but reuses the page texture.
    let again = renderer.render(&frame(&engine, "Hello GPU"), &target.view, None)?;
    assert_eq!(
        again.texture_uploads, 0,
        "unchanged atlas is not uploaded again"
    );
    assert_eq!(pixels, target.read_rgba(&renderer.gpu)?);

    // New glyphs dirty the page: exactly one new upload.
    let changed = renderer.render(&frame(&engine, "Hello GPU xyz"), &target.view, None)?;
    assert_eq!(changed.texture_uploads, 1);
    assert!(lit(&target.read_rgba(&renderer.gpu)?).len() > lit_pixels.len());
    renderer.gpu.wait_idle()?;
    Ok(())
}
