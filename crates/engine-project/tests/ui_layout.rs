//! Layout (taffy flex/grid/absolute), draw extraction, authoring schema and a
//! 1024-node timing measurement for the retained game UI.
use engine_project::{
    ComponentCatalog, CustomValue,
    content::{FontAtlas, FontGlyph, Texture},
    runtime::Draw,
    ui::Rect,
    ui_tree::*,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

fn node(id: &str, widget: UiWidget) -> UiNode {
    UiNode::new(id, widget).with_style(|s| s.padding = 0.0)
}
fn boxed(id: &str, parent: Option<&str>, width: UiLength, height: UiLength) -> UiNode {
    let mut n = node(id, UiWidget::Container).with_size(width, height);
    n.parent = parent.map(str::to_owned);
    n
}
fn px(v: f32) -> UiLength {
    UiLength::Pixels(v)
}
fn rect_of(tree: &UiTree, boxes: &[UiBox], id: &str) -> Rect {
    boxes
        .iter()
        .find(|b| tree.nodes[b.node].id == id && b.item.is_none())
        .unwrap_or_else(|| panic!("{id} not laid out"))
        .rect
}
fn close(a: Rect, b: [f32; 4]) {
    let got = [a.x, a.y, a.width, a.height];
    assert!(
        got.iter().zip(b).all(|(g, e)| (g - e).abs() < 1e-3),
        "{got:?} != {b:?}"
    );
}

#[test]
fn flex_grow_shrink_and_wrap_produce_css_rectangles() {
    // grow: 150 px of free space split 1:2.
    let mut row = boxed("row", None, px(300.0), px(100.0));
    row.style.layout = UiLayout::Row;
    row.style.gap = 0.0;
    let mut a = boxed("a", Some("row"), px(100.0), px(20.0));
    a.style.grow = Some(1.0);
    let mut b = boxed("b", Some("row"), px(50.0), px(20.0));
    b.style.grow = Some(2.0);
    let tree = UiTree {
        nodes: vec![row, a, b],
    };
    let boxes = tree.layout([400, 300]).unwrap();
    close(rect_of(&tree, &boxes, "a"), [0.0, 0.0, 150.0, 20.0]);
    close(rect_of(&tree, &boxes, "b"), [150.0, 0.0, 150.0, 20.0]);

    // shrink: 100 px overflow removed equally from shrinkable 150 px items;
    // legacy pixel items (shrink absent) keep their size and overflow.
    let mut row = boxed("row", None, px(200.0), px(100.0));
    row.style.layout = UiLayout::Row;
    row.style.gap = 0.0;
    let mut a = boxed("a", Some("row"), px(150.0), px(20.0));
    a.style.shrink = Some(1.0);
    let mut b = boxed("b", Some("row"), px(150.0), px(20.0));
    b.style.shrink = Some(1.0);
    let mut tree = UiTree {
        nodes: vec![row, a, b],
    };
    let boxes = tree.layout([400, 300]).unwrap();
    close(rect_of(&tree, &boxes, "a"), [0.0, 0.0, 100.0, 20.0]);
    close(rect_of(&tree, &boxes, "b"), [100.0, 0.0, 100.0, 20.0]);
    tree.nodes[1].style.shrink = None;
    tree.nodes[2].style.shrink = None;
    let boxes = tree.layout([400, 300]).unwrap();
    close(rect_of(&tree, &boxes, "b"), [150.0, 0.0, 150.0, 20.0]);

    // wrap: two 100 px items per 250 px line, 10 px gaps, lines packed at start.
    let mut row = boxed("row", None, px(250.0), px(200.0));
    row.style.layout = UiLayout::Row;
    row.style.wrap = UiWrap::Wrap;
    row.style.gap = 10.0;
    row.style.align_content = Some(UiJustify::Start);
    let mut nodes = vec![row];
    for id in ["w0", "w1", "w2", "w3"] {
        nodes.push(boxed(id, Some("row"), px(100.0), px(40.0)));
    }
    let tree = UiTree { nodes };
    let boxes = tree.layout([400, 300]).unwrap();
    close(rect_of(&tree, &boxes, "w1"), [110.0, 0.0, 100.0, 40.0]);
    close(rect_of(&tree, &boxes, "w2"), [0.0, 50.0, 100.0, 40.0]);
    close(rect_of(&tree, &boxes, "w3"), [110.0, 50.0, 100.0, 40.0]);
}

#[test]
fn justify_and_align_distribute_flex_items() {
    let mut row = boxed("row", None, px(300.0), px(100.0));
    row.style.layout = UiLayout::Row;
    row.style.gap = 0.0;
    row.style.justify = Some(UiJustify::SpaceBetween);
    row.style.align_items = Some(UiAlign::Center);
    let tree = UiTree {
        nodes: vec![
            row,
            boxed("a", Some("row"), px(50.0), px(20.0)),
            boxed("b", Some("row"), px(50.0), px(40.0)),
            boxed("c", Some("row"), px(50.0), px(20.0)),
        ],
    };
    let boxes = tree.layout([400, 300]).unwrap();
    close(rect_of(&tree, &boxes, "a"), [0.0, 40.0, 50.0, 20.0]);
    close(rect_of(&tree, &boxes, "b"), [125.0, 30.0, 50.0, 40.0]);
    close(rect_of(&tree, &boxes, "c"), [250.0, 40.0, 50.0, 20.0]);
}

#[test]
fn grid_fr_tracks_minmax_and_spans() {
    let mut grid = UiNode::grid(
        "grid",
        vec![
            UiTrack::Px { value: 100.0 },
            UiTrack::Fr { value: 1.0 },
            UiTrack::Fr { value: 2.0 },
        ],
        vec![UiTrack::Px { value: 50.0 }, UiTrack::Fr { value: 1.0 }],
    )
    .with_size(px(300.0), px(200.0))
    .with_style(|s| {
        s.padding = 0.0;
        s.gap = 0.0;
    });
    grid.style.clip = false;
    let cell = |id: &str, column: UiGridPlacement, row: UiGridPlacement| {
        boxed(id, Some("grid"), UiLength::Fill, UiLength::Fill).with_style(|s| {
            s.grid_column = column;
            s.grid_row = row;
        })
    };
    let at = |start: i16| UiGridPlacement {
        start: Some(start),
        ..Default::default()
    };
    let tree = UiTree {
        nodes: vec![
            grid,
            cell("a", at(1), at(1)),
            cell(
                "b",
                UiGridPlacement {
                    start: Some(2),
                    span: Some(2),
                    end: None,
                },
                at(1),
            ),
            cell(
                "c",
                UiGridPlacement {
                    start: Some(1),
                    end: Some(-1),
                    span: None,
                },
                at(2),
            ),
            cell("d", at(3), at(2)),
        ],
    };
    let boxes = tree.layout([400, 300]).unwrap();
    close(rect_of(&tree, &boxes, "a"), [0.0, 0.0, 100.0, 50.0]);
    close(rect_of(&tree, &boxes, "b"), [100.0, 0.0, 200.0, 50.0]);
    close(rect_of(&tree, &boxes, "c"), [0.0, 50.0, 300.0, 150.0]);
    close(
        rect_of(&tree, &boxes, "d"),
        [100.0 + 200.0 / 3.0, 50.0, 400.0 / 3.0, 150.0],
    );

    let grid = UiNode::grid(
        "grid",
        vec![
            UiTrack::Minmax {
                min: UiTrackSize::Px { value: 50.0 },
                max: UiTrackSize::Fr { value: 1.0 },
            },
            UiTrack::Px { value: 100.0 },
        ],
        vec![],
    )
    .with_size(px(300.0), px(40.0))
    .with_style(|s| {
        s.padding = 0.0;
        s.gap = 0.0;
    });
    let tree = UiTree {
        nodes: vec![
            grid,
            boxed("x", Some("grid"), UiLength::Fill, px(10.0)),
            boxed("y", Some("grid"), UiLength::Fill, px(10.0)),
        ],
    };
    let boxes = tree.layout([400, 300]).unwrap();
    close(rect_of(&tree, &boxes, "x"), [0.0, 0.0, 200.0, 10.0]);
    close(rect_of(&tree, &boxes, "y"), [200.0, 0.0, 100.0, 10.0]);
}

#[test]
fn absolute_position_margin_aspect_ratio_and_max_size() {
    let mut panel = boxed("panel", None, px(200.0), px(200.0));
    panel.style.layout = UiLayout::Column;
    panel.style.padding = 10.0;
    let mut a = boxed("a", Some("panel"), px(50.0), px(20.0));
    a.style.margin = UiEdges {
        left: 5.0,
        top: 7.0,
        ..Default::default()
    };
    let mut pinned = boxed("pinned", Some("panel"), px(30.0), px(30.0));
    pinned.style.position = UiPosition::Absolute;
    pinned.style.inset = UiInset {
        right: Some(10.0),
        bottom: Some(20.0),
        ..Default::default()
    };
    let mut overlay = boxed("overlay", None, px(300.0), px(300.0));
    overlay.style.padding = 10.0;
    overlay.style.offset = [200.0, 0.0];
    let mut ratio = boxed("ratio", Some("overlay"), px(80.0), UiLength::Auto);
    ratio.style.aspect_ratio = Some(2.0);
    let mut capped = boxed("capped", Some("overlay"), UiLength::Fill, px(10.0));
    capped.style.max_width = px(50.0);
    capped.style.margin.left = 5.0;
    let tree = UiTree {
        nodes: vec![panel, a, pinned, overlay, ratio, capped],
    };
    let boxes = tree.layout([600, 400]).unwrap();
    close(rect_of(&tree, &boxes, "a"), [15.0, 17.0, 50.0, 20.0]);
    close(rect_of(&tree, &boxes, "pinned"), [160.0, 150.0, 30.0, 30.0]);
    // Offsets translate the subtree after layout.
    close(rect_of(&tree, &boxes, "ratio"), [210.0, 10.0, 80.0, 40.0]);
    close(rect_of(&tree, &boxes, "capped"), [215.0, 10.0, 50.0, 10.0]);
}

#[test]
fn version_one_percent_fill_and_offset_json_keeps_legacy_rectangles() {
    let authored = json!({"tree":{"nodes":[
        {"id":"panel","widget":{"kind":"container"},"style":{"width":{"unit":"fill"},"height":{"unit":"fill"},"layout":"column","padding":10,"gap":10}},
        {"id":"half","parent":"panel","style":{"width":{"unit":"percent","value":0.5},"height":{"unit":"pixels","value":30}}},
        {"id":"rest","parent":"panel","style":{"width":{"unit":"fill"},"height":{"unit":"fill"}}},
        {"id":"over","parent":"panel","style":{"offset":[5,5]}},
        {"id":"corner","style":{"width":{"unit":"percent","value":0.25},"offset":[8,4],"padding":20}}
    ]}});
    let catalog = ComponentCatalog::new(vec![]).unwrap();
    catalog
        .validate(
            "engine.UiRoot",
            &CustomValue {
                version: 1,
                data: authored.clone(),
            },
        )
        .unwrap();
    let root: UiRoot = serde_json::from_value(authored).unwrap();
    let tree = &root.tree;
    let boxes = tree.layout([400, 300]).unwrap();
    close(rect_of(tree, &boxes, "panel"), [0.0, 0.0, 400.0, 300.0]);
    close(rect_of(tree, &boxes, "half"), [10.0, 10.0, 190.0, 30.0]);
    close(rect_of(tree, &boxes, "rest"), [10.0, 50.0, 380.0, 202.0]);
    close(rect_of(tree, &boxes, "over"), [15.0, 267.0, 120.0, 28.0]);
    // Padding 20 is clamped to half of the 28 px height, as legacy layout did.
    let corner = boxes
        .iter()
        .find(|b| tree.nodes[b.node].id == "corner")
        .unwrap();
    close(corner.rect, [8.0, 4.0, 100.0, 28.0]);
    close(corner.content, [28.0, 18.0, 60.0, 0.0]);
    // Default-valued fields are omitted when serialized.
    let encoded = serde_json::to_value(&root).unwrap();
    assert_eq!(
        encoded["tree"]["nodes"][2]["style"],
        json!({"width":{"unit":"fill"},"height":{"unit":"fill"}})
    );
}

#[test]
fn content_sized_text_is_measured_through_the_text_backend() {
    let (fonts, textures) = assets();
    let backend = BitmapTextBackend::new(&fonts, &textures);
    let mut row = boxed("row", None, px(23.0), UiLength::Auto);
    row.style.layout = UiLayout::Column;
    let mut label = node(
        "label",
        UiWidget::Label {
            text: "AA AA".into(),
        },
    )
    .with_size(UiLength::Auto, UiLength::Auto)
    .child_of("row");
    label.style.font = "font".into();
    label.style.padding = 1.0;
    let tree = UiTree {
        nodes: vec![row, label],
    };
    // 23 px wraps "AA AA" (25 px) into two 8 px lines, plus 1 px padding.
    let boxes = tree.layout_with([200, 200], &backend).unwrap();
    close(rect_of(&tree, &boxes, "label"), [0.0, 0.0, 23.0, 18.0]);
    close(rect_of(&tree, &boxes, "row"), [0.0, 0.0, 23.0, 18.0]);
    let metrics = backend
        .measure(&UiTextRequest {
            text: "<b>AA</b> &amp;",
            kind: UiTextKind::Markup,
            font: "font",
            scale: 2.0,
            color: [1.0; 4],
            max_width: None,
        })
        .unwrap();
    assert_eq!(strip_markup("<b>AA</b> &amp;"), "AA &");
    assert_eq!(
        (metrics.width, metrics.height, metrics.lines),
        (40.0, 16.0, 1)
    );
}

#[test]
fn virtual_list_lays_out_only_the_visible_range() {
    assert_eq!(virtual_range(1000, 20.0, 105.0, 100.0, 1), 4..12);
    assert_eq!(virtual_range(1000, 20.0, 1.0e6, 100.0, 0), 995..1000);
    assert_eq!(virtual_range(0, 20.0, 0.0, 100.0, 1), 0..0);
    assert_eq!(virtual_range(100_000, 1.0, 0.0, 4096.0, 0).len(), 256);
    let mut list = UiNode::virtual_list("list", 1000, 20.0, "row").with_size(px(200.0), px(100.0));
    list.style.padding = 0.0;
    if let UiWidget::VirtualList { scroll, .. } = &mut list.widget {
        *scroll = 105.0;
    }
    let row = node(
        "row",
        UiWidget::Label {
            text: String::new(),
        },
    )
    .with_size(UiLength::Fill, UiLength::Fill)
    .child_of("list");
    let cell = node("cell", UiWidget::Container)
        .with_size(px(10.0), px(10.0))
        .child_of("row");
    let tree = UiTree {
        nodes: vec![list, row, cell],
    };
    let boxes = tree.layout([400, 300]).unwrap();
    let replicas: Vec<_> = boxes
        .iter()
        .filter(|b| tree.nodes[b.node].id == "row")
        .collect();
    assert_eq!(
        replicas.iter().map(|b| b.item.unwrap()).collect::<Vec<_>>(),
        (4..12).collect::<Vec<_>>()
    );
    close(replicas[1].rect, [0.0, -5.0, 200.0, 20.0]);
    assert_eq!(
        boxes
            .iter()
            .filter(|b| tree.nodes[b.node].id == "cell")
            .count(),
        8
    );
    assert_eq!(boxes[0].scroll, [0.0, 105.0]);
    assert_eq!(boxes[0].scroll_max, [0.0, 19900.0]);
}

fn assets() -> (BTreeMap<String, FontAtlas>, BTreeMap<String, Arc<Texture>>) {
    let glyph = |source: [u32; 4], advance: f32| FontGlyph {
        source,
        advance,
        bearing: [0.0; 2],
    };
    (
        BTreeMap::from([(
            "font".into(),
            FontAtlas {
                texture: "atlas".into(),
                line_height: 8.0,
                fallback: 65,
                glyphs: BTreeMap::from([
                    (65, glyph([0, 0, 4, 8], 5.0)),
                    (42, glyph([0, 0, 2, 2], 3.0)),
                    (32, glyph([0, 0, 0, 0], 5.0)),
                ]),
            },
        )]),
        BTreeMap::from([
            (
                "atlas".into(),
                Arc::new(Texture {
                    width: 4,
                    height: 8,
                    pixels: vec![255; 128],
                }),
            ),
            (
                "img".into(),
                Arc::new(Texture {
                    width: 4,
                    height: 8,
                    pixels: vec![255; 128],
                }),
            ),
        ]),
    )
}

fn draws(root: &UiRoot, state: &UiRenderState) -> Vec<Draw> {
    let (fonts, textures) = assets();
    root.extract_draws_with(
        "hud",
        [400, 300],
        state,
        &BitmapTextBackend::new(&fonts, &textures),
        &textures,
    )
    .unwrap()
}
fn single(widget: UiWidget, width: f32, height: f32) -> UiRoot {
    UiRoot {
        tree: UiTree {
            nodes: vec![
                node("w", widget)
                    .with_size(px(width), px(height))
                    .with_style(|s| s.font = "font".into()),
            ],
        },
        ..Default::default()
    }
}
fn verts(draw: &Draw) -> usize {
    draw.mesh.vertices.len()
}

#[test]
fn image_and_nine_slice_draws_fit_and_slice_textures() {
    let image = |fit| UiWidget::Image {
        texture: "img".into(),
        uv: [0.0, 0.0, 1.0, 1.0],
        tint: [1.0; 4],
        fit,
    };
    let out = draws(
        &single(image(UiImageFit::Stretch), 100.0, 50.0),
        &UiRenderState::default(),
    );
    assert_eq!(out.len(), 2, "background + image");
    assert!(out[1].texture.is_some());
    assert_eq!(out[1].mesh.vertices[3].position, [0.0, 0.0, 0.0]);
    let contain = draws(
        &single(image(UiImageFit::Contain), 100.0, 50.0),
        &UiRenderState::default(),
    );
    // 4x8 source fitted into 100x50: 25x50 centred.
    assert_eq!(contain[1].mesh.vertices[3].position, [37.5, 0.0, 0.0]);
    assert_eq!(contain[1].mesh.vertices[1].position, [62.5, 50.0, 0.0]);
    let cover = draws(
        &single(image(UiImageFit::Cover), 100.0, 50.0),
        &UiRenderState::default(),
    );
    // Cover crops the source to its middle quarter vertically.
    assert_eq!(cover[1].mesh.vertices[3].uv, [0.0, 0.375]);
    assert_eq!(cover[1].mesh.vertices[1].uv, [1.0, 0.625]);
    let nine = |center| UiWidget::NineSlice {
        texture: "img".into(),
        slices: [1.0, 2.0, 1.0, 2.0],
        border: Some([10.0, 10.0, 10.0, 10.0]),
        center,
        tint: [1.0; 4],
    };
    let out = draws(&single(nine(true), 100.0, 50.0), &UiRenderState::default());
    assert_eq!(out.len(), 1, "nine-slice replaces the background quad");
    assert_eq!(verts(&out[0]), 36);
    assert_eq!(out[0].mesh.vertices[4].position, [10.0, 10.0, 0.0]);
    assert_eq!(out[0].mesh.vertices[4].uv, [0.25, 0.25]);
    assert_eq!(
        verts(&draws(&single(nine(false), 100.0, 50.0), &UiRenderState::default())[0]),
        32
    );
    let missing = single(
        UiWidget::Image {
            texture: "nope".into(),
            uv: [0.0, 0.0, 1.0, 1.0],
            tint: [1.0; 4],
            fit: UiImageFit::Stretch,
        },
        10.0,
        10.0,
    );
    let (fonts, textures) = assets();
    let error = missing
        .extract_draws("hud", [400, 300], None, &fonts, &textures)
        .unwrap_err();
    assert_eq!(error.diagnostics[0].code, "render.texture.unknown");
}

#[test]
fn text_input_draws_selection_caret_and_password_mask() {
    let input = |text: &str, password: bool| UiWidget::TextInput {
        text: text.into(),
        placeholder: "AAAA".into(),
        max_length: 64,
        password,
        multiline: false,
        caret: 1.min(text.len() as u32),
        anchor: 3.min(text.len() as u32),
        action: String::new(),
    };
    let focused = UiRenderState {
        focus: Some("w".into()),
        ..Default::default()
    };
    let out = draws(&single(input("AAAA", false), 100.0, 20.0), &focused);
    assert_eq!(out.len(), 3, "chrome, text, caret");
    // background + 2 selected glyph quads + 4 focus edges
    assert_eq!(verts(&out[0]), 4 + 8 + 16);
    assert_eq!(verts(&out[1]), 16);
    assert_eq!(verts(&out[2]), 4);
    assert_eq!(
        out[2].mesh.vertices[0].position[0], 5.0,
        "caret after first glyph"
    );
    let masked = draws(
        &single(input("AbC", true), 100.0, 20.0),
        &UiRenderState::default(),
    );
    // Three '*' glyphs (2x2 source) instead of the value, no caret when unfocused.
    assert_eq!(masked.len(), 2);
    let glyphs = &masked[1].mesh.vertices;
    assert_eq!(glyphs.len(), 12);
    assert_eq!(glyphs[1].position, [2.0, 2.0, 0.0]);
    assert_eq!(glyphs[7].position, [3.0, 0.0, 0.0], "mask advance is 3 px");
    // Empty values show the placeholder at half alpha.
    let placeholder = draws(
        &single(input("", false), 100.0, 20.0),
        &UiRenderState::default(),
    );
    assert_eq!(placeholder[1].mesh.vertices[0].color[3], 0.5);
    let preedit = UiRenderState {
        focus: Some("w".into()),
        preedit: Some((
            "w".into(),
            UiPreedit {
                text: "AA".into(),
                cursor: None,
            },
        )),
        ..Default::default()
    };
    let composing = draws(&single(input("AAAA", false), 100.0, 20.0), &preedit);
    assert_eq!(verts(&composing[1]), 24, "value plus preedit glyphs");
    // caret + two underline quads
    assert_eq!(verts(&composing[2]), 12);
}

#[test]
fn widget_draw_counts_layers_and_clips() {
    let none = UiRenderState::default();
    let button = draws(
        &single(
            UiWidget::Button {
                text: "AA".into(),
                action: String::new(),
            },
            50.0,
            20.0,
        ),
        &none,
    );
    assert_eq!(button.len(), 2);
    assert_eq!(button[1].clip, Some([0.0, 0.0, 50.0, 20.0]));
    let rich = draws(
        &single(
            UiWidget::RichLabel {
                markup: "<i>A</i>A".into(),
            },
            50.0,
            20.0,
        ),
        &none,
    );
    assert_eq!(verts(&rich[1]), 8, "markup tags are not drawn");

    let dropdown = UiWidget::Dropdown {
        options: vec!["A".into(), "AA".into(), "AAA".into()],
        selected: Some(1),
        placeholder: String::new(),
        max_visible: 8,
        action: String::new(),
    };
    let closed = draws(&single(dropdown.clone(), 100.0, 20.0), &none);
    assert_eq!(closed.len(), 2);
    assert_eq!(verts(&closed[0]), 8, "background + arrow");
    let open = draws(
        &single(dropdown, 100.0, 20.0),
        &UiRenderState {
            popup: Some(("w".into(), 2, 0)),
            ..Default::default()
        },
    );
    assert_eq!(open.len(), 4);
    let popup: Vec<_> = open.iter().filter(|d| d.layer == UI_POPUP_LAYER).collect();
    assert_eq!(popup.len(), 2);
    // popup background + highlighted row + 4 outline edges
    assert_eq!(verts(popup[0]), 24);
    assert_eq!(popup[0].clip, Some([0.0, 20.0, 100.0, 60.0]));
    assert_eq!(verts(popup[1]), 4 * 6, "one glyph per option character");

    let tabs = draws(
        &single(
            UiWidget::Tabs {
                labels: vec!["A".into(), "AA".into(), "A".into()],
                selected: 1,
                header_height: 20.0,
                action: String::new(),
            },
            300.0,
            100.0,
        ),
        &none,
    );
    assert_eq!(tabs.len(), 2);
    assert_eq!(verts(&tabs[0]), 8, "background + selected header cell");
    assert_eq!(tabs[0].mesh.vertices[4].position[0], 100.0);
    assert_eq!(verts(&tabs[1]), 16);
}

#[test]
fn scroll_view_list_dialog_and_tooltip_draws() {
    let mut view =
        UiNode::scroll_view("view", UiScrollAxes::Vertical).with_size(px(100.0), px(100.0));
    view.style.padding = 0.0;
    view.style.gap = 0.0;
    view.style.layout = UiLayout::Column;
    let mut nodes = vec![view];
    for i in 0..4 {
        nodes.push(boxed(
            &format!("i{i}"),
            Some("view"),
            UiLength::Fill,
            px(50.0),
        ));
    }
    let root = UiRoot {
        tree: UiTree { nodes },
        ..Default::default()
    };
    let out = draws(&root, &UiRenderState::default());
    assert_eq!(
        out.len(),
        1 + 2,
        "items scrolled out of the clip are culled"
    );
    // background + scrollbar track + thumb; thumb is half the track
    assert_eq!(verts(&out[0]), 12);
    assert_eq!(out[0].mesh.vertices[8].position, [92.0, 0.0, 0.0]);
    assert_eq!(out[0].mesh.vertices[10].position, [100.0, 50.0, 0.0]);
    // Items are clipped to the view minus the scrollbar gutter.
    assert!(out.iter().skip(1).all(|d| d.clip.unwrap()[2] == 92.0));

    let mut list = UiNode::list("list").with_size(px(100.0), px(100.0));
    list.style.padding = 0.0;
    if let UiWidget::List { selected, .. } = &mut list.widget {
        *selected = Some(1);
    }
    let root = UiRoot {
        tree: UiTree {
            nodes: vec![
                list,
                boxed("a", Some("list"), UiLength::Fill, px(20.0)),
                boxed("b", Some("list"), UiLength::Fill, px(20.0)),
            ],
        },
        ..Default::default()
    };
    let out = draws(&root, &UiRenderState::default());
    assert_eq!(out.len(), 4, "list, a, b, b selection outline");
    assert_eq!(verts(&out[3]), 16);

    let mut dialog =
        UiNode::dialog("dlg", "AA", vec!["A".into(), "AA".into()]).with_size(px(200.0), px(100.0));
    dialog.style.font = "font".into();
    if let UiWidget::ModalDialog { open, .. } = &mut dialog.widget {
        *open = true;
    }
    let root = UiRoot {
        tree: UiTree {
            nodes: vec![boxed("behind", None, px(50.0), px(50.0)), dialog],
        },
        ..Default::default()
    };
    let state = UiRenderState {
        dialog_choice: Some(("dlg".into(), 1)),
        ..Default::default()
    };
    let out = draws(&root, &state);
    assert_eq!(out[0].layer, 0);
    let overlay: Vec<_> = out.iter().filter(|d| d.layer == UI_DIALOG_LAYER).collect();
    // backdrop, dialog chrome, dialog text, highlighted button outline
    assert_eq!(overlay.len(), 4);
    assert_eq!(overlay[0].clip, Some([0.0, 0.0, 400.0, 300.0]));
    assert_eq!(
        overlay[1].mesh.vertices[0].position,
        [100.0, 100.0, 0.0],
        "centred"
    );
    // background + title bar + 2 buttons
    assert_eq!(verts(overlay[1]), 16);
    assert_eq!(verts(overlay[2]), 4 * 5, "title + button labels");
    assert_eq!(verts(overlay[3]), 16);
    let mut sorted = out.clone();
    sorted.sort_by(|a, b| (a.layer, &a.id).cmp(&(b.layer, &b.id)));
    assert_eq!(
        sorted.iter().map(|d| &d.id).collect::<Vec<_>>(),
        out.iter().map(|d| &d.id).collect::<Vec<_>>(),
        "emission order equals paint order"
    );

    let tip = draws(
        &single(UiWidget::Container, 50.0, 50.0),
        &UiRenderState {
            tooltip: Some(("w".into(), "AAA".into(), [390.0, 10.0])),
            ..Default::default()
        },
    );
    let tooltip: Vec<_> = tip.iter().filter(|d| d.layer == UI_POPUP_LAYER).collect();
    assert_eq!(tooltip.len(), 2);
    // 15x8 text + 4 px padding, clamped inside the 400 px viewport.
    assert_eq!(tooltip[0].clip, Some([377.0, 26.0, 23.0, 16.0]));
}

#[test]
fn descriptor_accepts_authored_widgets_and_rejects_invalid_shapes() {
    let catalog = ComponentCatalog::new(vec![]).unwrap();
    let check = |nodes: serde_json::Value| {
        catalog.validate(
            "engine.UiRoot",
            &CustomValue {
                version: 1,
                data: json!({"version":1,"layer":2,"tree":{"nodes":nodes}}),
            },
        )
    };
    let sample = json!([
        {"id":"root","style":{"layout":"grid","gridColumns":[{"kind":"px","value":100},{"kind":"fr","value":1},{"kind":"minmax","min":{"kind":"px","value":40},"max":{"kind":"fr","value":2}}],"gridRows":[{"kind":"auto"}],"paddingEdges":{"left":4,"top":2,"right":4,"bottom":2},"rowGap":6}},
        {"id":"title","parent":"root","widget":{"kind":"rich_label","markup":"<b>Hi</b>"},"style":{"gridColumn":{"start":1,"span":2},"alignSelf":"center"},"classes":["title","h1"]},
        {"id":"icon","parent":"root","widget":{"kind":"image","texture":"icons","uv":[0,0,0.5,0.5],"fit":"contain"},"style":{"aspectRatio":1,"width":{"unit":"auto"}}},
        {"id":"frame","parent":"root","widget":{"kind":"nine_slice","texture":"panel","slices":[4,4,4,4],"center":false}},
        {"id":"name","parent":"root","widget":{"kind":"text_input","text":"Ada","placeholder":"Name","maxLength":16,"caret":3,"anchor":0},"nav":{"down":"lang","tabIndex":1},"tooltip":{"text":"Your name","delay":0.25}},
        {"id":"lang","parent":"root","widget":{"kind":"dropdown","options":["en","fr"],"selected":1,"maxVisible":4,"action":"lang"}},
        {"id":"scroll","parent":"root","widget":{"kind":"scroll_view","offset":[0,20],"axes":"both"},"style":{"layout":"column","minHeight":{"unit":"pixels","value":40}}},
        {"id":"items","parent":"scroll","widget":{"kind":"list","selected":0}},
        {"id":"item","parent":"items","widget":{"kind":"label","text":"One"},"style":{"grow":1,"shrink":0,"basis":{"unit":"pixels","value":20}}},
        {"id":"rows","parent":"root","widget":{"kind":"virtual_list","itemCount":500,"itemExtent":24,"template":"row","scroll":48,"overscan":2}},
        {"id":"row","parent":"rows","widget":{"kind":"button","text":"Row","action":"pick"}},
        {"id":"tabs","parent":"root","widget":{"kind":"tabs","labels":["A","B"],"selected":1,"headerHeight":24}},
        {"id":"pa","parent":"tabs"},{"id":"pb","parent":"tabs","style":{"wrap":"wrap","justify":"space_between","layout":"row_reverse"}},
        {"id":"quit","parent":"root","widget":{"kind":"modal_dialog","title":"Quit?","open":true,"buttons":["Yes","No"]},"style":{"position":"absolute","inset":{"left":0,"top":0}}},
        {"id":"vol","parent":"quit","widget":{"kind":"slider","value":0.5,"min":0,"max":1,"step":0.1},"nav":{"focusable":false}}
    ]);
    check(sample.clone()).unwrap();
    let root: UiRoot = serde_json::from_value(json!({"tree":{"nodes":sample}})).unwrap();
    let roundtrip: UiRoot = serde_json::from_value(serde_json::to_value(&root).unwrap()).unwrap();
    assert_eq!(roundtrip, root);
    root.tree.layout([800, 600]).unwrap();
    let invalid = [
        json!([{"id":"a","widget":{"kind":"label","text":"x","bogus":1}}]),
        json!([{"id":"a","style":{"minWidth":{"unit":"fill"}}}]),
        json!([{"id":"a","style":{"gridColumns":[{"kind":"minmax","min":{"kind":"fr","value":1},"max":{"kind":"auto"}}]}}]),
        json!([{"id":"a","widget":{"kind":"tabs","labels":["A"],"selected":1}}]),
        json!([{"id":"a","widget":{"kind":"virtual_list","itemCount":3,"itemExtent":10,"template":"b"}},{"id":"c","parent":"a"}]),
        json!([{"id":"a","nav":{"left":"missing"}}]),
        json!([{"id":"a","widget":{"kind":"dropdown","options":["x"],"selected":1}}]),
        json!([{"id":"a","widget":{"kind":"dropdown","options":(0..65).map(|i| i.to_string()).collect::<Vec<_>>()}}]),
        json!([{"id":"a","widget":{"kind":"text_input","text":"abc","caret":4}}]),
        json!([{"id":"a","widget":{"kind":"text_input","text":"abcdef","maxLength":3}}]),
        json!([{"id":"a","widget":{"kind":"text_input","text":"a\nb"}}]),
        json!([{"id":"a","widget":{"kind":"image","texture":"t","uv":[0.5,0,0.75,1]}}]),
        json!([{"id":"a","widget":{"kind":"modal_dialog","buttons":["1","2","3","4","5"]}}]),
        json!([{"id":"a","classes":["bad class"]}]),
        json!([{"id":"a","tooltip":{"text":"x","delay":20}}]),
        json!([{"id":"a","style":{"gridRow":{"start":0}}}]),
        json!([{"id":"a","style":{"layout":"flex"}}]),
    ];
    for nodes in invalid {
        assert!(check(nodes.clone()).is_err(), "accepted {nodes}");
    }
}

#[test]
fn layout_and_extract_of_1024_nodes_is_measured() {
    let (fonts, textures) = assets();
    let backend = BitmapTextBackend::new(&fonts, &textures);
    let mut nodes =
        vec![UiNode::container("root", UiLayout::Column).with_size(UiLength::Fill, UiLength::Fill)];
    for r in 0..31 {
        let row = format!("r{r}");
        nodes.push(
            UiNode::container(&row, UiLayout::Row)
                .with_size(UiLength::Fill, px(20.0))
                .child_of("root"),
        );
        for c in 0..32 {
            nodes.push(
                UiNode::button(format!("b{r}_{c}"), "AA", "")
                    .with_size(UiLength::Fill, UiLength::Fill)
                    .with_style(|s| s.font = "font".into())
                    .child_of(&row),
            );
        }
    }
    let root = UiRoot {
        tree: UiTree { nodes },
        ..Default::default()
    };
    assert_eq!(root.tree.nodes.len(), MAX_UI_NODES);
    let serialized = serde_json::to_vec(&root).unwrap().len();
    let state = UiRenderState::default();
    let iterations = 10;
    let start = std::time::Instant::now();
    let mut boxes = 0;
    for _ in 0..iterations {
        boxes = root.tree.layout_with([1920, 1080], &backend).unwrap().len();
    }
    let layout = start.elapsed() / iterations;
    let start = std::time::Instant::now();
    let mut draws = 0;
    for _ in 0..iterations {
        // Extraction lays the tree out again, then builds meshes.
        draws = root
            .extract_draws_with("hud", [1920, 1080], &state, &backend, &textures)
            .unwrap()
            .len();
    }
    let extract = start.elapsed() / iterations;
    println!(
        "ui 1024 nodes ({} build): layout {:.3} ms, layout+extract {:.3} ms, {boxes} boxes, {draws} draws, {serialized} bytes JSON",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        layout.as_secs_f64() * 1000.0,
        extract.as_secs_f64() * 1000.0,
    );
    assert_eq!(boxes, 1024);
    assert_eq!(draws, 32 + 992 * 2);
}
