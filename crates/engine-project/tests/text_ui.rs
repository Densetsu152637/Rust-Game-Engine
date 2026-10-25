//! Shaped text in the game UI: the `engine-text` backend, `font_face` assets,
//! rich labels (links, inline images), text input on TrueType fonts, backend
//! selection and world-space text.
use engine_project::{
    ActionFrame, ComponentCatalog, CustomValue, Project, Runtime,
    content::{
        FontAtlas, FontAtlasMode, FontConfig, FontFamilySettings, FontGlyph, Texture,
        load_font_config, load_font_face,
    },
    runtime::DrawSpace,
    ui_tree::*,
};
use engine_text::{
    AtlasConfig, FallbackConfig, FontLibrary, GlyphAtlas, LayoutOptions, RichText, TextStyle,
    build_quads, transform_quads,
};
use glam::{Mat4, Vec2, Vec3};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, sync::Arc};

const UBUNTU: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;
const EMOJI: &[u8] = epaint_default_fonts::NOTO_EMOJI_REGULAR;

fn backend() -> EngineTextBackend {
    let mut backend = EngineTextBackend::new(AtlasConfig::default()).unwrap();
    backend
        .register_font(
            UBUNTU.to_vec(),
            "ui",
            FontFamilySettings {
                size: Some(20.0),
                atlas: FontAtlasMode::Coverage,
            },
        )
        .unwrap();
    backend
}

fn request<'a>(text: &'a str, kind: UiTextKind, width: Option<f32>) -> UiTextRequest<'a> {
    UiTextRequest {
        text,
        kind,
        font: "ui",
        scale: 1.0,
        color: [1.0, 1.0, 1.0, 1.0],
        max_width: width,
    }
}

fn plain<'a>(text: &'a str) -> UiTextRequest<'a> {
    request(text, UiTextKind::Plain, None)
}

fn px(v: f32) -> UiLength {
    UiLength::Pixels(v)
}
fn at(node: UiNode, x: f32, y: f32, w: f32, h: f32) -> UiNode {
    node.with_size(px(w), px(h)).with_style(|s| {
        s.offset = [x, y];
        s.padding = 0.0;
        s.gap = 0.0;
    })
}
fn root(nodes: Vec<UiNode>) -> UiRoot {
    UiRoot {
        tree: UiTree { nodes },
        ..Default::default()
    }
}
fn press(action: &str, pointer: [f32; 2]) -> ActionFrame {
    ActionFrame {
        pressed: [action.into()].into(),
        pointer,
        ..Default::default()
    }
}
fn no_textures() -> BTreeMap<String, Arc<Texture>> {
    BTreeMap::new()
}

// ---- measure / layout parity with engine-text ----

#[test]
fn backend_measure_and_layout_match_engine_text_directly() {
    let engine = backend();
    let text = "The quick brown fox jumps over the lazy dog";
    let req = request(text, UiTextKind::Plain, Some(150.0));

    let mut library = FontLibrary::new();
    library.load_font_as(UBUNTU.to_vec(), "ui").unwrap();
    let layout = library
        .layout(
            &RichText::plain(text),
            &TextStyle::new("ui", 20.0),
            &LayoutOptions {
                max_width: Some(150.0),
                ..LayoutOptions::default()
            },
        )
        .unwrap();
    let mut atlas = GlyphAtlas::new(AtlasConfig::default()).unwrap();
    let direct = build_quads(&layout, &library, &mut atlas, Vec2::ZERO).unwrap();

    let metrics = engine.measure(&req).unwrap();
    assert_eq!(metrics.lines as usize, layout.lines.len());
    assert!(layout.lines.len() > 1, "the sentence must wrap at 150 px");
    let widest = layout.lines.iter().map(|l| l.width).fold(0.0, f32::max);
    assert_eq!(metrics.width, widest);
    assert_eq!(
        metrics.height,
        layout.lines.iter().map(|l| l.height).sum::<f32>()
    );
    assert_eq!(metrics.line_height, layout.lines[0].height);

    let laid = engine.layout(&req).unwrap();
    assert_eq!(laid.metrics, metrics, "measure and layout agree");
    assert_eq!(laid.glyphs.len(), direct.len());
    for (ours, theirs) in laid.glyphs.iter().zip(&direct) {
        assert_eq!(ours.rect.x, theirs.rect.x);
        assert_eq!(ours.rect.y, theirs.rect.y);
        assert_eq!(ours.rect.width, theirs.rect.width);
        assert_eq!(ours.rect.height, theirs.rect.height);
        assert_eq!(
            ours.uv,
            [theirs.uv.u0, theirs.uv.v0, theirs.uv.u1, theirs.uv.v1]
        );
    }
    // Caret stops cover every grapheme plus the end.
    assert_eq!(laid.carets.len(), text.chars().count() + 1);
    assert_eq!(laid.carets.last().unwrap().index as usize, text.len());
    // Min-content is the widest word, never wider than the text itself.
    let unwrapped = engine.measure(&plain(text)).unwrap();
    assert!(unwrapped.min_width > 0.0 && unwrapped.min_width < unwrapped.width / 3.0);
}

#[test]
fn font_scale_multiplies_the_family_pixel_size() {
    let engine = backend();
    let one = engine.measure(&plain("Scale")).unwrap();
    let two = engine
        .measure(&UiTextRequest {
            scale: 2.0,
            ..plain("Scale")
        })
        .unwrap();
    assert!(
        (two.width / one.width - 2.0).abs() < 0.05,
        "{one:?} {two:?}"
    );
    assert_eq!(engine.pixel_size("ui", 2.0), Some(40.0));
}

#[test]
fn hostile_requests_return_errors() {
    let engine = backend();
    let long = "x".repeat(70_000);
    assert_eq!(
        engine.measure(&plain(&long)).unwrap_err().diagnostics[0].code,
        "ui.text.invalid"
    );
    let bad_scale = UiTextRequest {
        scale: f32::NAN,
        ..plain("a")
    };
    assert!(engine.layout(&bad_scale).is_err());
    let huge = UiTextRequest {
        scale: 1000.0,
        ..plain("a")
    };
    assert_eq!(
        engine.layout(&huge).unwrap_err().diagnostics[0].code,
        "text.input.size_out_of_range"
    );
    assert_eq!(
        engine
            .measure(&UiTextRequest {
                font: "missing",
                ..plain("a")
            })
            .unwrap_err()
            .diagnostics[0]
            .code,
        "render.font.unknown"
    );
}

// ---- rich text ----

#[test]
fn markup_sets_colour_size_weight_and_underline() {
    let engine = backend();
    let plain_width = engine.measure(&plain("xx")).unwrap().width;
    let bold = engine
        .measure(&request("[b]xx[/b]", UiTextKind::Markup, None))
        .unwrap();
    assert!(bold.width > plain_width, "synthetic bold widens advances");
    let big = engine
        .measure(&request("[size=40]x[/size]", UiTextKind::Markup, None))
        .unwrap();
    assert!(big.line_height > engine.measure(&plain("x")).unwrap().line_height * 1.5);

    let laid = engine
        .layout(&request(
            "[color=#ff0000]R[/color]G[u]U[/u]",
            UiTextKind::Markup,
            None,
        ))
        .unwrap();
    let colours: Vec<[f32; 4]> = laid.glyphs.iter().map(|g| g.color).collect();
    assert!(colours.contains(&[1.0, 0.0, 0.0, 1.0]), "{colours:?}");
    assert!(
        colours.contains(&[1.0; 4]),
        "base colour passes through exactly"
    );
    assert!(
        laid.glyphs
            .iter()
            .any(|g| g.uv[0] == g.uv[2] && g.uv[1] == g.uv[3]),
        "the underline is a solid quad from the atlas white block"
    );
    // Literal unknown tags stay text; malformed known tags are errors.
    let literal = engine
        .measure(&request("[note] hi", UiTextKind::Markup, None))
        .unwrap();
    assert!(literal.width > engine.measure(&plain(" hi")).unwrap().width);
    let error = engine
        .measure(&request("[color=zz]x[/color]", UiTextKind::Markup, None))
        .unwrap_err();
    assert_eq!(error.diagnostics[0].code, "text.markup.bad_value");
    assert!(validate_markup("[b]ok[/b]").is_ok());
    assert!(validate_markup("[b]open").is_err());
}

fn link_label(markup: &str) -> UiRoot {
    let mut label = at(UiNode::rich_label("doc", markup), 0.0, 0.0, 400.0, 40.0);
    label.style.font = "ui".into();
    root(vec![label])
}

#[test]
fn rich_label_link_click_emits_a_link_event() {
    let engine = backend();
    let ctx = UiTickContext {
        viewport: [400, 300],
        dt: 0.0,
        text: &engine,
    };
    let markup = "Read [link=help]the guide[/link] now";
    let mut roots = [("hud".to_string(), link_label(markup))];
    let laid = engine
        .layout(&request(markup, UiTextKind::Markup, Some(400.0)))
        .unwrap();
    assert_eq!(laid.links.len(), 1);
    assert_eq!(laid.links[0].id, "help");
    let r = laid.links[0].rect;
    let inside = [r.x + r.width / 2.0, r.y + r.height / 2.0];
    let mut ui = GameUi::default();

    let events = ui
        .tick_with(&mut roots, &press("ui_click", inside), &ctx)
        .unwrap();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].name, "ui.link_activated");
    assert_eq!(events[0].node, "doc");
    assert_eq!(events[0].value, json!("help"));

    // A click on unlinked text of the same label does nothing.
    let events = ui
        .tick_with(
            &mut roots,
            &press("ui_click", [r.x + r.width + 40.0, 10.0]),
            &ctx,
        )
        .unwrap();
    assert!(events.is_empty(), "{events:?}");

    // The focused label activates its first link with ui_confirm.
    let events = ui
        .tick_with(&mut roots, &press("ui_confirm", [900.0, 900.0]), &ctx)
        .unwrap();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].value, json!("help"));

    // Without links a rich label is not a pointer target.
    let mut plain_roots = [("hud".to_string(), link_label("no [b]links[/b] here"))];
    let mut other = GameUi::default();
    let events = other
        .tick_with(&mut plain_roots, &press("ui_click", [5.0, 5.0]), &ctx)
        .unwrap();
    assert!(events.is_empty() && other.focus().is_none());
}

#[test]
fn inline_images_draw_with_the_referenced_texture() {
    let engine = backend();
    let icon = Arc::new(Texture {
        width: 2,
        height: 2,
        pixels: vec![255; 16],
    });
    let textures = BTreeMap::from([("icon".to_string(), icon.clone())]);
    let ui = link_label("Score [img=icon width=16 height=12] ok");
    let draws = ui
        .extract_draws_with(
            "hud",
            [400, 300],
            &UiRenderState::default(),
            &engine,
            &textures,
        )
        .unwrap();
    let image = draws
        .iter()
        .find(|d| d.texture.as_ref().is_some_and(|t| Arc::ptr_eq(t, &icon)))
        .expect("a draw samples the inline image texture");
    assert_eq!(image.mesh.vertices.len(), 4);
    let v = &image.mesh.vertices;
    assert_eq!(v[1].position[0] - v[0].position[0], 16.0);
    assert_eq!(v[0].position[1] - v[3].position[1], 12.0);
    assert!(
        draws
            .iter()
            .any(|d| d.texture.as_ref().is_some_and(|t| !Arc::ptr_eq(t, &icon))),
        "glyphs use the atlas page"
    );
    let error = ui
        .extract_draws_with(
            "hud",
            [400, 300],
            &UiRenderState::default(),
            &engine,
            &no_textures(),
        )
        .unwrap_err();
    assert_eq!(error.diagnostics[0].code, "render.texture.unknown");
}

// ---- atlas pages ----

#[test]
fn atlas_textures_are_recreated_only_when_dirty() {
    let engine = backend();
    assert_eq!(engine.stats().texture_uploads, 0);
    let first = engine.layout(&plain("Hello")).unwrap();
    assert_eq!(engine.stats().texture_uploads, 1);
    assert_eq!(first.pages.len(), 1);

    // Same text, different request (colour) and a cache hit: no new texture.
    let again = engine.layout(&plain("Hello")).unwrap();
    let recoloured = engine
        .layout(&UiTextRequest {
            color: [0.5, 0.5, 0.5, 1.0],
            ..plain("Hello")
        })
        .unwrap();
    assert_eq!(engine.stats().texture_uploads, 1);
    assert!(Arc::ptr_eq(&first.pages[0], &again.pages[0]));
    assert!(Arc::ptr_eq(&first.pages[0], &recoloured.pages[0]));

    // Glyphs already on the page: still no upload.
    engine.layout(&plain("oleH")).unwrap();
    assert_eq!(engine.stats().texture_uploads, 1);

    // New glyphs dirty the page: exactly one new snapshot.
    let more = engine.layout(&plain("World!")).unwrap();
    assert_eq!(engine.stats().texture_uploads, 2);
    assert!(!Arc::ptr_eq(&first.pages[0], &more.pages[0]));
    let stable = engine.layout(&plain("World!")).unwrap();
    assert_eq!(engine.stats().texture_uploads, 2);
    assert!(Arc::ptr_eq(&more.pages[0], &stable.pages[0]));
    // Old layouts keep working against the page that contains their glyphs.
    assert!(first.glyphs.iter().all(|g| g.page == 0));
    // Measuring never rasterises.
    let before = engine.stats();
    engine
        .measure(&plain("completely new glyphs: QXZJ"))
        .unwrap();
    assert_eq!(engine.stats().cached_glyphs, before.cached_glyphs);
    assert_eq!(engine.stats().texture_uploads, before.texture_uploads);
}

#[test]
fn atlas_pages_are_recycled_across_frames_without_corrupting_old_layouts() {
    let mut engine = EngineTextBackend::new(AtlasConfig {
        page_size: 64,
        max_pages: 1,
        ..AtlasConfig::default()
    })
    .unwrap();
    engine
        .register_font(UBUNTU.to_vec(), "ui", FontFamilySettings::default())
        .unwrap();
    let first = engine.layout(&plain("abc")).unwrap();
    let fill = |engine: &EngineTextBackend, texts: &[&str]| {
        texts
            .iter()
            .find_map(|text| engine.layout(&plain(text)).err())
    };
    // Frame 0: fill the only page; everything on it is in use, so it cannot be recycled.
    let full = fill(
        &engine,
        &["DEFGHIJ", "KLMNOPQ", "RSTUVWX", "YZ01234", "56789&%"],
    );
    assert_eq!(full.unwrap().diagnostics[0].code, "text.atlas.full");
    assert_eq!(engine.stats().evictions, 0);
    // Frame 1: the page is idle, so the first request recycles it.
    engine.begin_frame();
    let second = engine.layout(&plain("klm")).unwrap();
    assert_eq!(engine.stats().evictions, 1, "{:?}", engine.stats());
    assert!(!Arc::ptr_eq(&first.pages[0], &second.pages[0]));
    // Glyphs placed in the current frame are never evicted: the page fills up instead.
    let full = fill(
        &engine,
        &["DEFGHIJ", "KLMNOPQ", "RSTUVWX", "YZ01234", "56789&%"],
    );
    assert_eq!(full.unwrap().diagnostics[0].code, "text.atlas.full");
    // The earlier layout still holds its own, unchanged snapshot.
    assert_eq!(first.pages[0].pixels.len(), 64 * 64 * 4);
    assert!(first.pages[0].pixels.chunks(4).any(|p| p[3] > 0));
}

// ---- text input on a TrueType font ----

fn ttf_input(multiline: bool, width: f32, height: f32) -> UiNode {
    let mut node =
        at(UiNode::text_input("name", "", 200), 0.0, 0.0, width, height).with_action("name");
    node.style.font = "ui".into();
    if let UiWidget::TextInput { multiline: m, .. } = &mut node.widget {
        *m = multiline;
    }
    node
}
fn input_text(roots: &[(String, UiRoot)]) -> (String, u32) {
    let tree = &roots[0].1.tree;
    match &tree.nodes[tree.find("name").unwrap()].widget {
        UiWidget::TextInput { text, caret, .. } => (text.clone(), *caret),
        _ => unreachable!(),
    }
}

#[test]
fn ttf_text_input_inserts_composes_and_commits() {
    let engine = backend();
    let ctx = UiTickContext {
        viewport: [400, 300],
        dt: 0.0,
        text: &engine,
    };
    let mut roots = [("hud".to_string(), root(vec![ttf_input(false, 200.0, 40.0)]))];
    let mut ui = GameUi::default();
    ui.tick_with(&mut roots, &press("ui_click", [5.0, 5.0]), &ctx)
        .unwrap();
    assert_eq!(ui.text_input_focus().unwrap().node, "name");
    ui.push_text_event(UiTextEvent::Insert("h\u{e9}llo".into()))
        .unwrap();
    let events = ui
        .tick_with(&mut roots, &ActionFrame::default(), &ctx)
        .unwrap();
    assert_eq!(events[0].value, json!("h\u{e9}llo"));
    assert_eq!(input_text(&roots), ("h\u{e9}llo".into(), 5));

    // IME composition: not part of the value, drawn underlined at the caret.
    ui.push_text_event(UiTextEvent::Preedit(UiPreedit {
        text: "wo".into(),
        cursor: Some([2, 2]),
    }))
    .unwrap();
    ui.tick_with(&mut roots, &ActionFrame::default(), &ctx)
        .unwrap();
    assert_eq!(ui.preedit().unwrap().text, "wo");
    assert_eq!(input_text(&roots).0, "h\u{e9}llo");
    let area = ui
        .ime_area()
        .expect("focused input reports the caret rectangle");
    let state = ui.render_state("hud");
    let draws = roots[0]
        .1
        .extract_draws_with("hud", [400, 300], &state, &engine, &no_textures())
        .unwrap();
    let top = draws.iter().find(|d| d.id.ends_with(":3")).unwrap();
    let rects: Vec<(f32, f32, f32, f32)> = top
        .mesh
        .vertices
        .chunks(4)
        .map(|q| {
            (
                q[0].position[0],
                q[0].position[1],
                q[2].position[0] - q[0].position[0],
                q[2].position[1] - q[0].position[1],
            )
        })
        .collect();
    let underline = rects
        .iter()
        .find(|r| r.3 == 1.0 && r.2 > 1.0)
        .expect("an underline quad is drawn for the preedit");
    // The underline sits at the bottom of the composed text, to the left of the caret.
    assert!(underline.0 < area.x && underline.1 > area.y);
    assert!(
        rects.iter().any(|r| r.2 == 2.0 && r.3 == area.height),
        "caret quad"
    );

    ui.push_text_event(UiTextEvent::Commit("rld".into()))
        .unwrap();
    ui.tick_with(&mut roots, &ActionFrame::default(), &ctx)
        .unwrap();
    assert!(ui.preedit().is_none());
    assert_eq!(input_text(&roots), ("h\u{e9}llorld".into(), 8));
}

#[test]
fn ime_area_is_inside_the_input_rectangle() {
    let engine = backend();
    let ctx = UiTickContext {
        viewport: [400, 300],
        dt: 0.0,
        text: &engine,
    };
    let mut node = ttf_input(false, 160.0, 36.0);
    node.style.offset = [30.0, 50.0];
    let mut roots = [("hud".to_string(), root(vec![node]))];
    let mut ui = GameUi::default();
    assert!(ui.ime_area().is_none());
    ui.tick_with(&mut roots, &press("ui_click", [40.0, 60.0]), &ctx)
        .unwrap();
    for text in [
        "",
        "short",
        "a longer line that scrolls horizontally past the edge",
    ] {
        ui.push_text_event(UiTextEvent::Insert(text.into()))
            .unwrap();
        ui.tick_with(&mut roots, &ActionFrame::default(), &ctx)
            .unwrap();
        let area = ui.ime_area().unwrap();
        assert!(
            area.x >= 30.0 && area.x + area.width <= 30.0 + 160.0,
            "{area:?}"
        );
        assert!(
            area.y >= 50.0 && area.y + area.height <= 50.0 + 36.0,
            "{area:?}"
        );
        assert!(area.height > 0.0);
    }
    // Unfocusing clears it.
    ui.tick_with(&mut roots, &press("ui_click", [350.0, 250.0]), &ctx)
        .unwrap();
    assert!(ui.text_input_focus().is_none() && ui.ime_area().is_none());
}

#[test]
fn multiline_up_and_down_move_by_wrapped_visual_lines() {
    let engine = backend();
    let ctx = UiTickContext {
        viewport: [400, 300],
        dt: 0.0,
        text: &engine,
    };
    let mut roots = [("hud".to_string(), root(vec![ttf_input(true, 90.0, 120.0)]))];
    let mut ui = GameUi::default();
    ui.tick_with(&mut roots, &press("ui_click", [5.0, 5.0]), &ctx)
        .unwrap();
    // One logical line that wraps into several visual lines at 90 px.
    let text = "alpha bravo charlie delta echo";
    ui.push_text_event(UiTextEvent::Insert(text.into()))
        .unwrap();
    ui.tick_with(&mut roots, &ActionFrame::default(), &ctx)
        .unwrap();
    let end = text.chars().count() as u32;
    assert_eq!(input_text(&roots).1, end);
    let lines = engine
        .measure(&request(text, UiTextKind::Plain, Some(90.0)))
        .unwrap()
        .lines;
    assert!(lines >= 3, "text wraps into {lines} lines");

    let mut caret_after = |ui: &mut GameUi, key: UiEditKey| {
        ui.push_text_event(UiTextEvent::key(key)).unwrap();
        ui.tick_with(&mut roots, &ActionFrame::default(), &ctx)
            .unwrap();
        input_text(&roots).1
    };
    let up = caret_after(&mut ui, UiEditKey::Up);
    assert!(
        up > 0 && up < end,
        "Up lands on the previous visual line, not the text start: {up}"
    );
    let up_again = caret_after(&mut ui, UiEditKey::Up);
    assert!(up_again < up);
    let down = caret_after(&mut ui, UiEditKey::Down);
    assert!(down > up_again && down <= end);
}

#[test]
fn clicking_places_the_caret_with_engine_geometry() {
    let engine = backend();
    let ctx = UiTickContext {
        viewport: [400, 300],
        dt: 0.0,
        text: &engine,
    };
    let mut roots = [("hud".to_string(), root(vec![ttf_input(false, 300.0, 40.0)]))];
    let mut ui = GameUi::default();
    ui.tick_with(&mut roots, &press("ui_click", [5.0, 5.0]), &ctx)
        .unwrap();
    ui.push_text_event(UiTextEvent::Insert("abcdef".into()))
        .unwrap();
    ui.tick_with(&mut roots, &ActionFrame::default(), &ctx)
        .unwrap();
    let carets = engine.layout(&plain("abcdef")).unwrap().carets;
    let target = carets[3].x + 0.5;
    ui.tick_with(&mut roots, &press("ui_click", [target, 10.0]), &ctx)
        .unwrap();
    assert_eq!(input_text(&roots).1, 3);
}

// ---- selection and configuration ----

fn bitmap_assets() -> (BTreeMap<String, FontAtlas>, BTreeMap<String, Arc<Texture>>) {
    (
        BTreeMap::from([(
            "font".into(),
            FontAtlas {
                texture: "atlas".into(),
                line_height: 8.0,
                fallback: 65,
                glyphs: BTreeMap::from([(
                    65,
                    FontGlyph {
                        source: [0, 0, 4, 8],
                        advance: 5.0,
                        bearing: [0.0; 2],
                    },
                )]),
            },
        )]),
        BTreeMap::from([(
            "atlas".into(),
            Arc::new(Texture {
                width: 4,
                height: 8,
                pixels: vec![255; 128],
            }),
        )]),
    )
}

#[test]
fn selection_rule_prefers_font_faces_and_falls_back_to_bitmap() {
    let (fonts, textures) = bitmap_assets();
    let mut engine = backend();
    let selecting = SelectingTextBackend::new(Some(&engine), &fonts, &textures);
    assert!(selecting.uses_engine("ui"));
    assert!(
        selecting.uses_engine("UI"),
        "family keys are case-insensitive"
    );
    assert!(!selecting.uses_engine("font"));
    assert!(!selecting.uses_engine(""));

    let bitmap = BitmapTextBackend::new(&fonts, &textures);
    let req = UiTextRequest {
        font: "font",
        ..plain("AAA")
    };
    assert_eq!(
        selecting.measure(&req).unwrap(),
        bitmap.measure(&req).unwrap()
    );
    assert_eq!(selecting.layout(&req).unwrap().glyphs.len(), 3);
    assert!(selecting.layout(&req).unwrap().geometry.is_none());
    let shaped = selecting.layout(&plain("AAA")).unwrap();
    assert!(shaped.geometry.is_some());
    assert_ne!(
        selecting.measure(&plain("AAA")).unwrap(),
        bitmap.measure(&req).unwrap()
    );
    let missing = UiTextRequest {
        font: "nowhere",
        ..plain("A")
    };
    assert_eq!(
        selecting.measure(&missing).unwrap_err().diagnostics[0].code,
        "render.font.unknown"
    );

    // Without an engine every key is bitmap; with a default family the empty key is shaped.
    let no_engine = SelectingTextBackend::new(None, &fonts, &textures);
    assert!(!no_engine.uses_engine("ui"));
    assert!(no_engine.measure(&plain("AAA")).is_err());
    engine.set_default_family(Some("ui"));
    let selecting = SelectingTextBackend::new(Some(&engine), &fonts, &textures);
    assert!(selecting.uses_engine(""));
    assert!(
        selecting
            .measure(&UiTextRequest {
                font: "",
                ..plain("AAA")
            })
            .is_ok()
    );
}

#[test]
fn sdf_requests_are_reported_and_served_by_a_coverage_atlas() {
    let mut engine = EngineTextBackend::new(AtlasConfig::default()).unwrap();
    assert_eq!(engine.render_status().state, TextRenderState::Coverage);
    engine
        .register_font(
            UBUNTU.to_vec(),
            "ui",
            FontFamilySettings {
                size: Some(16.0),
                atlas: FontAtlasMode::Sdf,
            },
        )
        .unwrap();
    let status = engine.render_status();
    assert_eq!(status.requested, FontAtlasMode::Sdf);
    assert_eq!(status.state, TextRenderState::SdfFallback);
    assert_eq!(status.sdf_families, vec!["ui".to_string()]);
    assert!(
        status
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("deferred")
    );
    // It still draws, through a coverage atlas at the needed pixel size.
    let laid = engine.layout(&plain("SDF")).unwrap();
    assert_eq!(laid.glyphs.len(), 3);
    let dim = laid.glyphs[0].rect.height;
    assert!(
        dim > 5.0 && dim < 20.0,
        "rasterised at 16 px, not a 48 px SDF base: {dim}"
    );
}

#[test]
fn fallback_chains_are_configurable_at_runtime() {
    let mut engine = backend();
    engine
        .register_font(EMOJI.to_vec(), "emoji", FontFamilySettings::default())
        .unwrap();
    let without = engine.layout(&plain("A\u{1F600}")).unwrap().glyphs.len();
    engine
        .set_fallback(FallbackConfig::new().with_default(["emoji"]))
        .unwrap();
    assert_eq!(engine.fallback().default, vec!["emoji".to_string()]);
    let with = engine.layout(&plain("A\u{1F600}")).unwrap();
    assert!(
        with.glyphs.len() >= 2,
        "emoji glyph comes from the fallback face"
    );
    assert_ne!(
        with.glyphs.last().unwrap().rect,
        engine.layout(&plain("A")).unwrap().glyphs[0].rect
    );
    assert!(without >= 1);
    // Bad configuration is an error, not a silent drop.
    let error = engine
        .set_fallback(FallbackConfig::new().with_default(vec!["x"; 300]))
        .unwrap_err();
    assert_eq!(error.diagnostics[0].code, "text.font.fallback_limit");
    engine.set_language(Some("ja"));
    assert!(engine.layout(&plain("A")).is_ok());
}

// ---- font_face / font_config assets ----

fn write_project(dir: &Path, assets: Value, scene: Value) {
    fs::write(
        dir.join("project.json"),
        json!({"schema":1,"name":"Text Project","startup_scene":"scene.json","assets":assets})
            .to_string(),
    )
    .unwrap();
    fs::write(dir.join("scene.json"), scene.to_string()).unwrap();
}

fn text_project(extra_assets: Vec<Value>, scene: Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("ui.ttf"), UBUNTU).unwrap();
    fs::write(dir.path().join("emoji.ttf"), EMOJI).unwrap();
    let mut assets = vec![
        json!({"id":"ui","kind":"font_face","path":"ui.ttf"}),
        json!({"id":"emoji","kind":"font_face","path":"emoji.ttf"}),
    ];
    assets.extend(extra_assets);
    write_project(dir.path(), Value::Array(assets), scene);
    dir
}

fn empty_scene() -> Value {
    json!({"schema":1,"scene_id":"main","entities":[]})
}

#[test]
fn mcp_style_manifest_with_font_faces_loads_and_drives_the_runtime() {
    let dir = text_project(
        vec![json!({"id":"fonts","kind":"font_config","path":"fonts.json"})],
        empty_scene(),
    );
    fs::write(
        dir.path().join("fonts.json"),
        json!({
            "schema": 1, "defaultFamily": "ui", "defaultSize": 14,
            "families": {"ui": {"size": 18, "atlas": "coverage"}},
            "fallback": {"default": ["emoji"], "byLanguage": {"ja": ["emoji"]}, "byScript": {"Arab": ["emoji"]}}
        })
        .to_string(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
    assert_eq!(project.assets["ui"].kind, "font_face");
    let runtime = Runtime::new(project).unwrap();
    let engine = runtime
        .text_engine()
        .expect("font_face assets create the engine");
    assert!(engine.resolves("ui") && engine.resolves("emoji") && engine.resolves(""));
    assert_eq!(engine.pixel_size("ui", 1.0), Some(18.0));
    assert_eq!(engine.pixel_size("emoji", 1.0), Some(14.0), "defaultSize");
    assert_eq!(engine.fallback().default, vec!["emoji".to_string()]);
    assert!(engine.fallback().by_language.contains_key("ja"));
    assert!(runtime.ui_text().uses_engine("ui"));
    assert!(engine.stats().faces >= 2);
}

#[test]
fn font_config_can_group_files_into_a_family() {
    let dir = text_project(
        vec![json!({"id":"fonts","kind":"font_config","path":"fonts.json"})],
        empty_scene(),
    );
    fs::write(
        dir.path().join("fonts.json"),
        json!({"schema":1,"faces":{"emoji":{"family":"ui"}}}).to_string(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
    let runtime = Runtime::new(project).unwrap();
    let engine = runtime.text_engine().unwrap();
    assert!(engine.resolves("ui"));
    assert!(
        !engine.resolves("emoji"),
        "emoji is now a file of the ui family"
    );
    assert_eq!(engine.stats().faces, 2);
}

#[test]
fn font_face_load_errors_are_typed() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("bad.ttf"), b"definitely not a font").unwrap();
    fs::write(dir.path().join("note.txt"), UBUNTU).unwrap();
    fs::write(dir.path().join("good.ttf"), UBUNTU).unwrap();
    let code = |path: &str| {
        load_font_face(&dir.path().join(path))
            .unwrap_err()
            .diagnostics[0]
            .code
            .clone()
    };
    assert_eq!(code("bad.ttf"), "asset.font_face.invalid");
    assert_eq!(code("note.txt"), "asset.font_face.extension");
    assert!(load_font_face(&dir.path().join("missing.ttf")).is_err());
    let face = load_font_face(&dir.path().join("good.ttf")).unwrap();
    assert_eq!(face.faces[0].family, "Ubuntu");
    assert!(!face.faces[0].italic);

    // Through the project: validation and runtime start both reject it.
    let project_dir = text_project(
        vec![json!({"id":"bad","kind":"font_face","path":"bad.ttf"})],
        empty_scene(),
    );
    fs::write(project_dir.path().join("bad.ttf"), b"garbage").unwrap();
    // Project loading validates every declared asset.
    assert_eq!(
        Project::load(project_dir.path().join("project.json"))
            .unwrap_err()
            .diagnostics[0]
            .code,
        "asset.font_face.invalid"
    );
}

#[test]
fn font_config_validation_rejects_bad_documents() {
    let dir = tempfile::tempdir().unwrap();
    let load = |doc: Value| {
        let path = dir.path().join("fonts.json");
        fs::write(&path, doc.to_string()).unwrap();
        load_font_config(&path)
    };
    for bad in [
        json!({"schema": 2}),
        json!({"schema": 1, "defaultSize": 0}),
        json!({"schema": 1, "families": {"ui": {"size": 9999}}}),
        json!({"schema": 1, "unknownField": true}),
        json!({"schema": 1, "fallback": {"byScript": {"Arabic": ["x"]}}}),
        json!({"schema": 1, "faces": {"": {}}}),
    ] {
        assert_eq!(
            load(bad.clone()).unwrap_err().diagnostics[0].code,
            "asset.font_config.invalid",
            "{bad}"
        );
    }
    let config = load(json!({"schema":1,"fallback":{"default":["ghost"]}})).unwrap();
    let faces = ["ui".to_string()].into();
    assert_eq!(
        config.validate_against(&faces).unwrap_err().diagnostics[0].code,
        "asset.font_config.unknown_family"
    );
    let config = load(json!({"schema":1,"faces":{"nope":{}}})).unwrap();
    assert_eq!(
        config.validate_against(&faces).unwrap_err().diagnostics[0].code,
        "asset.font_config.unknown_face"
    );
    assert!(FontConfig::default().validate_against(&faces).is_ok());

    // Two config assets are rejected at runtime start.
    let project_dir = text_project(
        vec![
            json!({"id":"a","kind":"font_config","path":"a.json"}),
            json!({"id":"b","kind":"font_config","path":"b.json"}),
        ],
        empty_scene(),
    );
    for name in ["a.json", "b.json"] {
        fs::write(
            project_dir.path().join(name),
            json!({"schema":1}).to_string(),
        )
        .unwrap();
    }
    let project = Project::load(project_dir.path().join("project.json")).unwrap();
    assert_eq!(
        Runtime::new(project).err().unwrap().diagnostics[0].code,
        "asset.font_config.duplicate"
    );
}

// ---- world-space text ----

fn world_text_scene(data: Value, transform: Value) -> Value {
    json!({"schema":1,"scene_id":"main","entities":[
        {"id":"label","name":"Label","components":{
            "Transform": transform,
            "custom":{"engine.WorldText":{"version":1,"data":data}}}}
    ]})
}

fn world_text_runtime(data: Value, transform: Value) -> (tempfile::TempDir, Runtime) {
    let dir = text_project(vec![], world_text_scene(data, transform));
    let project = Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    runtime.start().unwrap();
    (dir, runtime)
}

/// Quad corners of the first world-text draw, as `[tl, tr, br, bl]` per quad.
fn world_corners(runtime: &Runtime) -> Vec<[[f32; 3]; 4]> {
    let frame = runtime.extract_frame().unwrap();
    let draws: Vec<_> = frame
        .draws
        .iter()
        .filter(|d| d.id.starts_with("worldtext:label"))
        .collect();
    assert!(!draws.is_empty(), "world text produced draws");
    assert!(draws.iter().all(|d| d.space == DrawSpace::World));
    assert!(
        draws
            .iter()
            .all(|d| d.texture.is_some() && d.clip.is_none())
    );
    draws
        .iter()
        .flat_map(|d| {
            d.mesh
                .vertices
                .chunks(4)
                .map(|q| [q[0].position, q[1].position, q[2].position, q[3].position])
        })
        .collect()
}

#[test]
fn world_text_extracts_world_space_draws_with_transformed_corners() {
    let data = json!({"version":1,"text":"Hi","font":"ui","size":2.0,"pixelsPerUnit":32.0,
        "anchor":[0.0,0.0],"color":[1.0,0.5,0.25,1.0]});
    let transform = json!({"position":[10.0,5.0,1.0],"scale":[2.0,2.0,1.0]});
    let (_dir, runtime) = world_text_runtime(data, transform);
    let ours = world_corners(&runtime);
    assert_eq!(ours.len(), 2, "one quad per glyph");

    // Expected corners straight from engine-text: layout px (y down) scaled by
    // 1/ppu with a y flip, then the entity matrix (scale 2, translate 10,5,1).
    let mut library = FontLibrary::new();
    library.load_font_as(UBUNTU.to_vec(), "ui").unwrap();
    let layout = library
        .layout(
            &RichText::plain("Hi"),
            &TextStyle::new("ui", 64.0),
            &LayoutOptions::default(),
        )
        .unwrap();
    let mut atlas = GlyphAtlas::new(AtlasConfig::default()).unwrap();
    let quads = build_quads(&layout, &library, &mut atlas, Vec2::ZERO).unwrap();
    let world = Mat4::from_scale_rotation_translation(
        Vec3::new(2.0, 2.0, 1.0),
        glam::Quat::IDENTITY,
        Vec3::new(10.0, 5.0, 1.0),
    );
    let matrix = world * Mat4::from_scale(Vec3::new(1.0 / 32.0, -1.0 / 32.0, 1.0));
    let expected = transform_quads(&quads, matrix);
    assert_eq!(expected.len(), ours.len());
    for (quad, corners) in expected.iter().zip(&ours) {
        for (a, b) in quad.corners.iter().zip(corners) {
            assert!(
                (a.x - b[0]).abs() < 1e-3 && (a.y - b[1]).abs() < 1e-3 && (a.z - b[2]).abs() < 1e-3,
                "{a:?} vs {b:?}"
            );
        }
    }
    // Text hangs down and to the right of the anchor at the entity position.
    let first = ours[0];
    assert!(first[0][0] >= 10.0 && first[0][1] <= 5.0 + 1e-3);
    assert!(
        first[0][1] > first[3][1],
        "top corner is above the bottom corner"
    );

    // Colour is per-vertex straight alpha.
    let frame = runtime.extract_frame().unwrap();
    let draw = frame
        .draws
        .iter()
        .find(|d| d.id.starts_with("worldtext:"))
        .unwrap();
    let c = draw.mesh.vertices[0].color;
    assert!((c[0] - 1.0).abs() < 0.01 && (c[1] - 0.5).abs() < 0.01 && (c[2] - 0.25).abs() < 0.01);
}

#[test]
fn world_text_anchor_and_billboard_place_the_text_box() {
    let base = json!({"version":1,"text":"Hi","font":"ui","size":2.0,"pixelsPerUnit":32.0});
    let spun = json!({"position":[0.0,0.0,0.0],"rotation":[0.0,0.0,0.70710678,0.70710678]});
    let flat = json!({"position":[0.0,0.0,0.0]});
    let extent = |data: Value, transform: Value| {
        let (_dir, runtime) = world_text_runtime(data, transform);
        let corners = world_corners(&runtime);
        let xs = corners.iter().flatten().map(|c| c[0]);
        let ys = corners.iter().flatten().map(|c| c[1]);
        (
            xs.clone().fold(f32::MAX, f32::min),
            xs.fold(f32::MIN, f32::max),
            ys.clone().fold(f32::MAX, f32::min),
            ys.fold(f32::MIN, f32::max),
        )
    };
    // Anchored at the top-left the text starts at the origin; centred it straddles it.
    let mut top_left = base.clone();
    top_left["anchor"] = json!([0.0, 0.0]);
    let (min_x, _, _, max_y) = extent(top_left, flat.clone());
    assert!(min_x >= -0.01 && max_y <= 0.01);
    let (min_x, max_x, min_y, max_y) = extent(base.clone(), flat.clone());
    assert!(min_x < 0.0 && max_x > 0.0 && min_y < 0.0 && max_y > 0.0);

    // A quarter turn swaps the extents unless the text is a billboard.
    let (a, b, c, d) = extent(base.clone(), spun.clone());
    assert!((b - a) < (d - c), "rotated text is taller than wide");
    let mut billboard = base.clone();
    billboard["billboard"] = json!(true);
    let (a, b, c, d) = extent(billboard, spun);
    assert!(
        (b - a) > (d - c),
        "billboard text ignores the entity rotation"
    );
}

#[test]
fn world_text_wraps_aligns_and_validates() {
    let wide = json!({"version":1,"text":"alpha bravo charlie delta","font":"ui","size":1.0,
        "pixelsPerUnit":32.0,"anchor":[0.0,0.0]});
    let flat = json!({"position":[0.0,0.0,0.0]});
    let (_d, unwrapped) = world_text_runtime(wide.clone(), flat.clone());
    let mut wrapped = wide.clone();
    wrapped["maxWidth"] = json!(4.0);
    let (_d2, wrapped_runtime) = world_text_runtime(wrapped, flat.clone());
    let height = |runtime: &Runtime| {
        let corners = world_corners(runtime);
        let ys = corners.iter().flatten().map(|c| c[1]);
        ys.clone().fold(f32::MIN, f32::max) - ys.fold(f32::MAX, f32::min)
    };
    assert!(
        height(&wrapped_runtime) > height(&unwrapped) * 1.5,
        "max width wraps lines"
    );

    // Right alignment pushes the last glyph to the box edge.
    let mut right = wide.clone();
    right["maxWidth"] = json!(30.0);
    right["align"] = json!("right");
    let (_d3, right_runtime) = world_text_runtime(right, flat);
    let max_x = world_corners(&right_runtime)
        .iter()
        .flatten()
        .map(|c| c[0])
        .fold(f32::MIN, f32::max);
    assert!(max_x > 28.0 && max_x <= 30.0 + 0.1, "{max_x}");

    // Descriptor and typed validation.
    let catalog = ComponentCatalog::new(vec![]).unwrap();
    let value = |data: Value| CustomValue { version: 1, data };
    assert!(catalog.0.contains_key("engine.WorldText"));
    assert!(catalog.validate("engine.WorldText", &value(wide)).is_ok());
    for bad in [
        json!({"size": 0}),
        json!({"color": [2.0, 0.0, 0.0, 1.0]}),
        json!({"pixelsPerUnit": 1000}),
        json!({"size": 100.0, "pixelsPerUnit": 100.0}),
        json!({"maxWidth": -1.0}),
        json!({"anchor": [0.5]}),
        json!({"unknown": 1}),
        json!({"markup": true, "text": "[b]open"}),
        json!({"align": "justify"}),
    ] {
        assert!(
            catalog
                .validate("engine.WorldText", &value(bad.clone()))
                .is_err(),
            "{bad}"
        );
    }
}

#[test]
fn world_text_requires_a_font_face() {
    let dir = tempfile::tempdir().unwrap();
    write_project(
        dir.path(),
        json!([]),
        world_text_scene(
            json!({"version":1,"text":"x","font":"ui"}),
            json!({"position":[0.0,0.0,0.0]}),
        ),
    );
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    assert_eq!(
        runtime.extract_frame().unwrap_err().diagnostics[0].code,
        "render.font.unknown"
    );
}

#[test]
fn world_text_supports_markup_and_reports_sdf_requests() {
    let data = json!({"version":1,"text":"[color=#00ff00]G[/color][b]B[/b]","markup":true,
        "font":"ui","size":1.0,"atlas":"sdf"});
    let (_dir, runtime) = world_text_runtime(data, json!({"position":[0.0,0.0,0.0]}));
    let frame = runtime.extract_frame().unwrap();
    let colours: Vec<[f32; 4]> = frame
        .draws
        .iter()
        .filter(|d| d.id.starts_with("worldtext:"))
        .flat_map(|d| d.mesh.vertices.iter().map(|v| v.color))
        .collect();
    assert!(
        colours.iter().any(|c| c[1] > 0.99 && c[0] < 0.01),
        "{colours:?}"
    );
    let status = runtime.text_engine().unwrap().render_status();
    assert_eq!(status.state, TextRenderState::SdfFallback);
    assert!(status.sdf_requests >= 1);
}

#[test]
fn ui_root_with_shaped_text_extracts_through_the_runtime() {
    let ui = json!({"version":1,"tree":{"nodes":[
        {"id":"label","widget":{"kind":"label","text":"Hello shaped"},
         "style":{"font":"ui","width":{"unit":"pixels","value":200},"height":{"unit":"pixels","value":30}}}
    ]}});
    let scene = json!({"schema":1,"scene_id":"main","entities":[
        {"id":"hud","name":"HUD","components":{"custom":{"engine.UiRoot":{"version":1,"data":ui}}}}
    ]});
    let dir = text_project(vec![], scene);
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    runtime.start().unwrap();
    runtime
        .tick(
            1.0 / 60.0,
            ActionFrame {
                focused: true,
                ..Default::default()
            },
        )
        .unwrap();
    let frame = runtime.extract_frame().unwrap();
    let text = frame
        .draws
        .iter()
        .find(|d| d.space == DrawSpace::Ui && d.id.contains(":2"))
        .expect("the label's glyph draw");
    assert!(text.mesh.vertices.len() >= 4 * "Hello".len());
    let uploads = runtime.text_engine().unwrap().stats().texture_uploads;
    runtime
        .tick(
            1.0 / 60.0,
            ActionFrame {
                focused: true,
                ..Default::default()
            },
        )
        .unwrap();
    let again = runtime.extract_frame().unwrap();
    let again_text = again
        .draws
        .iter()
        .find(|d| d.space == DrawSpace::Ui && d.id.contains(":2"))
        .unwrap();
    assert!(Arc::ptr_eq(
        text.texture.as_ref().unwrap(),
        again_text.texture.as_ref().unwrap()
    ));
    assert_eq!(
        runtime.text_engine().unwrap().stats().texture_uploads,
        uploads
    );
}
