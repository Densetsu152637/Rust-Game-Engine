use engine_tilemap::collision::{
    collision_boxes, collision_outlines, merge_cells, signed_area, solid_cells,
};
use engine_tilemap::geometry::{chunk_geometry, position_to_cell};
use engine_tilemap::orient::{FLIP_X, FLIP_Y, TRANSPOSE};
use engine_tilemap::*;
use std::collections::BTreeSet;

fn tile(id: TileId, collision: CollisionKind) -> TileDef {
    TileDef {
        id,
        name: format!("t{id}"),
        source: [(id % 8) * 16, (id / 8) * 16, 16, 16],
        collision,
        properties: Default::default(),
    }
}
/// 64 full-collision tiles (ids 1..=64) so presets and plain tiles fit.
fn tileset_with(rules: Vec<RuleTile>) -> Tileset {
    let mut doc = TilesetDocument::new("test", "atlas", [16, 16]);
    doc.tiles = (1..=64).map(|id| tile(id, CollisionKind::Full)).collect();
    doc.rule_tiles = rules;
    Tileset::from_document(doc).unwrap()
}
fn map() -> Tilemap {
    Tilemap::new(CellRect::new([-64, -64], [64, 64])).unwrap()
}

#[test]
fn blob47_has_47_masks_and_exact_picks() {
    let masks = blob47_masks();
    assert_eq!(masks.len(), 47);
    assert_eq!(&masks[..5], &[0, 1, 4, 5, 7]);
    assert_eq!(*masks.last().unwrap(), 255);
    let ts = tileset_with(vec![RuleTile::blob47(100, "wall", 1)]);
    let mut map = map();
    let layer = map.layer_mut("ground").unwrap();
    layer
        .fill_rect(CellRect::new([0, 0], [3, 3]), 100, 0, Some(&ts))
        .unwrap();
    // Centre is fully surrounded: mask 255 is the last of 47 -> tile 1 + 46.
    assert_eq!(layer_shown(layer, [1, 1]), 47);
    // Bottom-left corner touches N, NE and E: mask 7 -> index 4 -> tile 5.
    assert_eq!(layer_shown(layer, [0, 0]), 5);
    // Every cell equals the independent mask computation.
    for y in 0..3 {
        for x in 0..3 {
            let mut mask = 0u8;
            for (bit, dx, dy) in [
                (1, 0, 1),
                (2, 1, 1),
                (4, 1, 0),
                (8, 1, -1),
                (16, 0, -1),
                (32, -1, -1),
                (64, -1, 0),
                (128, -1, 1),
            ] {
                if (0..3).contains(&(x + dx)) && (0..3).contains(&(y + dy)) {
                    mask |= bit;
                }
            }
            let index = masks.iter().position(|m| *m == blob_reduce(mask)).unwrap();
            assert_eq!(
                layer_shown(layer, [x, y]),
                1 + index as u32,
                "cell {x},{y} mask {mask}"
            );
        }
    }
    // An isolated cell is mask 0 -> the first tile.
    layer.set([10, 10], 100, 0, Some(&ts)).unwrap();
    assert_eq!(layer_shown(layer, [10, 10]), 1);
}
fn layer_shown(layer: &TileLayer, cell: CellPos) -> TileId {
    layer.get(cell).shown
}

#[test]
fn four_neighbour_preset_picks_by_cardinal_mask() {
    let ts = tileset_with(vec![RuleTile::four_neighbour16(100, "path", 1)]);
    let mut map = map();
    let layer = map.layer_mut("ground").unwrap();
    for x in 0..3 {
        layer.set([x, 0], 100, 0, Some(&ts)).unwrap();
    }
    // N=1,E=2,S=4,W=8: left end has E only, middle E+W, right end W only.
    assert_eq!(layer.get([0, 0]).shown, 1 + 2);
    assert_eq!(layer.get([1, 0]).shown, 1 + 10);
    assert_eq!(layer.get([2, 0]).shown, 1 + 8);
    // A diagonal neighbour changes nothing for the four-neighbour preset.
    layer.set([1, 1], 100, 0, Some(&ts)).unwrap();
    assert_eq!(layer.get([1, 0]).shown, 1 + 11);
    assert_eq!(layer.get([0, 0]).shown, 1 + 2);
}

#[test]
fn painting_updates_affected_neighbours_only() {
    let ts = tileset_with(vec![RuleTile::four_neighbour16(100, "path", 1)]);
    let mut map = map();
    let layer = map.layer_mut("ground").unwrap();
    let report = layer.set([0, 0], 100, 0, Some(&ts)).unwrap();
    assert_eq!((report.changed, report.refreshed), (1, 1));
    layer.set([20, 20], 100, 0, Some(&ts)).unwrap();
    let far = layer.chunk_revision([1, 1]).unwrap();
    let near = layer.chunk_revision([0, 0]).unwrap();
    let report = layer.set([1, 0], 100, 0, Some(&ts)).unwrap();
    // The new cell and its west neighbour change display; nothing else is re-stamped.
    assert_eq!((report.changed, report.refreshed), (1, 2));
    assert_eq!(report.dirty_chunks, vec![[0, 0]]);
    assert_eq!(layer.chunk_revision([1, 1]), Some(far));
    assert!(layer.chunk_revision([0, 0]).unwrap() > near);
    // A neighbour across a chunk edge dirties both chunks.
    let report = layer.set([15, 5], 100, 0, Some(&ts)).unwrap();
    assert_eq!(report.dirty_chunks, vec![[0, 0]]);
    let report = layer.set([16, 5], 100, 0, Some(&ts)).unwrap();
    assert_eq!(report.dirty_chunks, vec![[0, 0], [1, 0]]);
    assert_eq!(report.refreshed, 2);
    // Repainting the same tile is a no-op.
    let report = layer.set([16, 5], 100, 0, Some(&ts)).unwrap();
    assert_eq!(report, PaintReport::default());
    // Erasing reverts the neighbour.
    layer.erase([16, 5], Some(&ts)).unwrap();
    assert_eq!(layer.get([15, 5]).shown, 1);
}

#[test]
fn rotated_and_mirrored_rules_report_orientation() {
    let mut cap = RuleTile::new(100, "cap", 1);
    // Cap art points north: only the north neighbour is this tile.
    cap.rules.push(Rule {
        neighbours: [
            Neighbour::Any,
            Neighbour::This,
            Neighbour::Any,
            Neighbour::NotThis,
            Neighbour::NotThis,
            Neighbour::Any,
            Neighbour::NotThis,
            Neighbour::Any,
        ],
        output: 2,
        transform: RuleTransform::Rotated,
    });
    let mut edge = RuleTile::new(101, "edge", 1);
    // Edge art has a neighbour on the west only; mirroring covers the east.
    edge.rules.push(Rule {
        neighbours: [
            Neighbour::Any,
            Neighbour::NotThis,
            Neighbour::Any,
            Neighbour::This,
            Neighbour::NotThis,
            Neighbour::Any,
            Neighbour::NotThis,
            Neighbour::Any,
        ],
        output: 3,
        transform: RuleTransform::MirrorX,
    });
    let ts = tileset_with(vec![cap, edge]);
    let mut map = map();
    let layer = map.layer_mut("ground").unwrap();
    // Cap at (0,0) with neighbours at north / east / south / west in turn.
    for (neighbour, flags) in [
        ([0, 1], 0),
        ([1, 0], TRANSPOSE | FLIP_X),
        ([0, -1], FLIP_X | FLIP_Y),
        ([-1, 0], TRANSPOSE | FLIP_Y),
    ] {
        layer
            .fill_rect(CellRect::new([-3, -3], [4, 4]), 0, 0, Some(&ts))
            .unwrap();
        layer.set([0, 0], 100, 0, Some(&ts)).unwrap();
        // The neighbour is a different tile, so only the cap's own match is under test.
        layer.set(neighbour, 100, 0, Some(&ts)).unwrap();
        let cell = layer.get([0, 0]);
        assert_eq!((cell.shown, cell.shown_flags), (2, flags), "{neighbour:?}");
    }
    layer
        .fill_rect(CellRect::new([-3, -3], [4, 4]), 0, 0, Some(&ts))
        .unwrap();
    layer.set([0, 0], 101, 0, Some(&ts)).unwrap();
    layer.set([1, 0], 101, 0, Some(&ts)).unwrap();
    let cell = layer.get([0, 0]);
    assert_eq!((cell.shown, cell.shown_flags), (3, FLIP_X));
    layer.erase([1, 0], Some(&ts)).unwrap();
    layer.set([-1, 0], 101, 0, Some(&ts)).unwrap();
    let cell = layer.get([0, 0]);
    assert_eq!((cell.shown, cell.shown_flags), (3, 0));
}

#[test]
fn first_match_wins_and_connects_count_as_this() {
    let mut rule = RuleTile::new(100, "first", 1);
    let any = [Neighbour::Any; 8];
    rule.rules.push(Rule {
        neighbours: any,
        output: 7,
        transform: RuleTransform::None,
    });
    rule.rules.push(Rule {
        neighbours: any,
        output: 8,
        transform: RuleTransform::None,
    });
    let mut linked = RuleTile::new(101, "linked", 1);
    let mut east = [Neighbour::Any; 8];
    east[4] = Neighbour::This;
    linked.rules.push(Rule {
        neighbours: east,
        output: 9,
        transform: RuleTransform::None,
    });
    linked.connects = vec![5];
    let ts = tileset_with(vec![rule, linked]);
    let mut map = map();
    let layer = map.layer_mut("ground").unwrap();
    layer.set([0, 0], 100, 0, Some(&ts)).unwrap();
    assert_eq!(layer.get([0, 0]).shown, 7);
    layer.set([5, 5], 101, 0, Some(&ts)).unwrap();
    assert_eq!(layer.get([5, 5]).shown, 1);
    // Tile 5 is a plain tile that the linked rule tile connects to.
    layer.set([6, 5], 5, 0, Some(&ts)).unwrap();
    assert_eq!(layer.get([5, 5]).shown, 9);
    // Unknown tiles are rejected before any change.
    assert_eq!(
        layer.set([9, 9], 999, 0, Some(&ts)).unwrap_err().code,
        "tilemap.tile"
    );
}

#[test]
fn rect_and_flood_fill_are_bounded() {
    let mut map = Tilemap::new(CellRect::new([0, 0], [20, 20])).unwrap();
    let layer = map.layer_mut("ground").unwrap();
    let report = layer
        .fill_rect(CellRect::new([2, 2], [6, 5]), 3, 0, None)
        .unwrap();
    assert_eq!(report.changed, 12);
    assert_eq!(layer.filled_cells(), 12);
    // Walls enclose an 8 x 8 room; flooding fills exactly its interior.
    for i in 8..18 {
        for (x, y) in [(i, 8), (i, 17), (8, i), (17, i)] {
            layer.set([x, y], 1, 0, None).unwrap();
        }
    }
    let report = layer.flood_fill([10, 10], 2, 0, None, None).unwrap();
    assert_eq!(report.changed, 8 * 8);
    assert_eq!(layer.tile_at([9, 9]), 2);
    assert_eq!(layer.tile_at([8, 8]), 1);
    assert_eq!(layer.tile_at([18, 18]), 0);
    // Flooding the outside is limited by the map bounds and the limit rectangle.
    let before = layer.filled_cells();
    let report = layer
        .flood_fill([0, 19], 4, 0, Some(CellRect::new([0, 18], [3, 20])), None)
        .unwrap();
    assert_eq!(report.changed, 6);
    assert_eq!(layer.filled_cells(), before + 6);
    // Errors: outside bounds, outside limit, oversize rectangle.
    assert_eq!(
        layer.set([20, 0], 1, 0, None).unwrap_err().code,
        "tilemap.bounds"
    );
    assert_eq!(
        layer
            .flood_fill([10, 10], 5, 0, Some(CellRect::new([0, 0], [3, 3])), None)
            .unwrap_err()
            .code,
        "tilemap.fill"
    );
    assert_eq!(
        layer
            .fill_rect(CellRect::new([0, 0], [21, 5]), 1, 0, None)
            .unwrap_err()
            .code,
        "tilemap.fill"
    );
    // A flood region over the 65536-cell cap fails and leaves the layer untouched.
    let mut big = Tilemap::new(CellRect::new([0, 0], [300, 300])).unwrap();
    let layer = big.layer_mut("ground").unwrap();
    let err = layer.flood_fill([0, 0], 1, 0, None, None).unwrap_err();
    assert_eq!(err.code, "tilemap.fill");
    assert_eq!(layer.filled_cells(), 0);
    // Erase by rectangle.
    let mut m = Tilemap::new(CellRect::new([0, 0], [20, 20])).unwrap();
    let layer = m.layer_mut("ground").unwrap();
    layer
        .fill_rect(CellRect::new([0, 0], [10, 10]), 1, 0, None)
        .unwrap();
    layer
        .fill_rect(CellRect::new([0, 0], [5, 10]), 0, 0, None)
        .unwrap();
    assert_eq!(layer.filled_cells(), 50);
}

fn set_of(cells: &[CellPos]) -> BTreeSet<CellPos> {
    cells.iter().copied().collect()
}
fn rect_cells(rects: &[CellRect]) -> BTreeSet<CellPos> {
    let mut all = BTreeSet::new();
    for r in rects {
        for y in r.min[1]..r.max[1] {
            for x in r.min[0]..r.max[0] {
                assert!(all.insert([x, y]), "rectangles overlap at {x},{y}");
            }
        }
    }
    all
}

#[test]
fn greedy_merge_counts_areas_and_union() {
    // Solid 4 x 3 block: one rectangle of area 12.
    let block: BTreeSet<_> = (0..4).flat_map(|x| (0..3).map(move |y| [x, y])).collect();
    let rects = merge_cells(&block);
    assert_eq!(rects, vec![CellRect::new([0, 0], [4, 3])]);
    // L shape: row of 3 on the bottom plus a column of 2 above its left end -> 2 rectangles.
    let l = set_of(&[[0, 0], [1, 0], [2, 0], [0, 1], [0, 2]]);
    let rects = merge_cells(&l);
    assert_eq!(rects.len(), 2);
    assert_eq!(rects.iter().map(CellRect::area).sum::<i64>(), 5);
    assert_eq!(rect_cells(&rects), l);
    // Diagonal and checkerboard cells never merge.
    let checker = set_of(&[[0, 0], [2, 0], [1, 1], [3, 1]]);
    assert_eq!(merge_cells(&checker).len(), 4);
    // Ring (3x3 with a hole): bottom row, left side, right side and top middle.
    let ring: BTreeSet<_> = (0..3)
        .flat_map(|x| (0..3).map(move |y| [x, y]))
        .filter(|c| *c != [1, 1])
        .collect();
    let rects = merge_cells(&ring);
    assert_eq!(rects.len(), 4);
    assert_eq!(rect_cells(&rects), ring);
    // Pseudo-random 40x40 field: disjoint rectangles whose union is the input.
    let mut seed = 0x2545_f491_u64;
    let mut random = BTreeSet::new();
    for y in 0..40 {
        for x in 0..40 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            if (seed >> 33) % 100 < 60 {
                random.insert([x, y]);
            }
        }
    }
    let rects = merge_cells(&random);
    assert_eq!(rect_cells(&rects), random);
    assert!(
        rects.len() < random.len() / 2,
        "{} rects for {} cells",
        rects.len(),
        random.len()
    );
}

#[test]
fn layer_collision_boxes_merge_full_tiles_and_orient_custom_shapes() {
    let mut doc = TilesetDocument::new("c", "atlas", [16, 16]);
    doc.tiles = vec![
        tile(1, CollisionKind::Full),
        tile(2, CollisionKind::None),
        tile(
            3,
            CollisionKind::Rect {
                min: [0.0, 0.0],
                max: [1.0, 0.25],
            },
        ),
        tile(
            4,
            CollisionKind::Polygon {
                points: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 0.5]],
            },
        ),
    ];
    let ts = Tileset::from_document(doc).unwrap();
    let mut map = map();
    let layer = map.layer_mut("ground").unwrap();
    layer
        .fill_rect(CellRect::new([0, 0], [4, 2]), 1, 0, Some(&ts))
        .unwrap();
    layer.set([10, 0], 2, 0, Some(&ts)).unwrap();
    layer.set([10, 1], 3, 0, Some(&ts)).unwrap();
    layer.set([10, 2], 3, FLIP_Y, Some(&ts)).unwrap();
    layer.set([10, 3], 4, 0, Some(&ts)).unwrap();
    assert_eq!(solid_cells(layer, &ts).len(), 8);
    let boxes = collision_boxes(layer, &ts);
    // One merged 4x2 rectangle + three custom boxes.
    assert_eq!(boxes.len(), 4);
    assert_eq!(
        (boxes[0].min, boxes[0].max, boxes[0].area()),
        ([0.0, 0.0], [4.0, 2.0], 8.0)
    );
    // Chunk iteration is row-major from the bottom: (10,1), (10,2), (10,3).
    assert_eq!((boxes[1].min, boxes[1].max), ([10.0, 1.0], [11.0, 1.25]));
    assert_eq!((boxes[2].min, boxes[2].max), ([10.0, 2.75], [11.0, 3.0]));
    assert_eq!((boxes[3].min, boxes[3].max), ([10.0, 3.0], [11.0, 3.5]));
}

#[test]
fn outlines_trace_loops_with_holes() {
    let mut doc = TilesetDocument::new("o", "atlas", [16, 16]);
    doc.tiles = vec![tile(1, CollisionKind::Full)];
    let ts = Tileset::from_document(doc).unwrap();
    let mut map = map();
    let layer = map.layer_mut("ground").unwrap();
    layer
        .fill_rect(CellRect::new([0, 0], [2, 2]), 1, 0, Some(&ts))
        .unwrap();
    let loops = collision_outlines(layer, &ts);
    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0].len(), 4);
    assert_eq!(signed_area(&loops[0]), 4.0);
    // L shape: six corners, area 3.
    let mut map2 = self::map();
    let layer = map2.layer_mut("ground").unwrap();
    for c in [[0, 0], [1, 0], [0, 1]] {
        layer.set(c, 1, 0, Some(&ts)).unwrap();
    }
    let loops = collision_outlines(layer, &ts);
    assert_eq!(
        (loops.len(), loops[0].len(), signed_area(&loops[0])),
        (1, 6, 3.0)
    );
    // Ring: outer CCW area 9 and inner CW hole area -1.
    let mut map3 = self::map();
    let layer = map3.layer_mut("ground").unwrap();
    layer
        .fill_rect(CellRect::new([0, 0], [3, 3]), 1, 0, Some(&ts))
        .unwrap();
    layer.erase([1, 1], Some(&ts)).unwrap();
    let mut areas: Vec<f32> = collision_outlines(layer, &ts)
        .iter()
        .map(|l| signed_area(l))
        .collect();
    areas.sort_by(f32::total_cmp);
    assert_eq!(areas, vec![-1.0, 9.0]);
    // Cells touching only at a corner stay two separate loops.
    let mut map4 = self::map();
    let layer = map4.layer_mut("ground").unwrap();
    layer.set([0, 0], 1, 0, Some(&ts)).unwrap();
    layer.set([1, 1], 1, 0, Some(&ts)).unwrap();
    assert_eq!(collision_outlines(layer, &ts).len(), 2);
}

#[test]
fn documents_round_trip_and_reject_bad_input() {
    let ts = tileset_with(vec![RuleTile::blob47(100, "wall", 1)]);
    let again = Tileset::from_json(ts.to_json().as_bytes()).unwrap();
    assert_eq!(again.document(), ts.document());
    let mut m = map();
    m.add_layer("deco").unwrap().opacity = 0.5;
    m.layer_mut("ground")
        .unwrap()
        .fill_rect(CellRect::new([-20, -3], [20, 4]), 100, 0, Some(&ts))
        .unwrap();
    m.layer_mut("deco")
        .unwrap()
        .set([3, 3], 9, FLIP_X, Some(&ts))
        .unwrap();
    let json = m.to_json();
    let loaded = Tilemap::from_json(json.as_bytes(), Some(&ts)).unwrap();
    assert_eq!(loaded.to_document(), m.to_document());
    // Rule resolution is reproduced on load.
    assert_eq!(
        loaded.layer("ground").unwrap().get([0, 0]).shown,
        m.layer("ground").unwrap().get([0, 0]).shown
    );
    assert_eq!(loaded.layer("deco").unwrap().get([3, 3]).flags, FLIP_X);
    // Rejections.
    let mut doc = m.to_document();
    doc.version = 2;
    assert_eq!(
        Tilemap::from_document(&doc, None).err().unwrap().code,
        "tilemap.invalid"
    );
    let mut doc = m.to_document();
    doc.layers[1].opacity = 2.0;
    assert!(Tilemap::from_document(&doc, None).is_err());
    let mut doc = m.to_document();
    doc.layers[0].chunks[0].tiles.pop();
    assert!(Tilemap::from_document(&doc, None).is_err());
    let mut doc = m.to_document();
    doc.layers[0].chunks[0].x = 1000;
    assert!(
        Tilemap::from_document(&doc, None).is_err(),
        "chunk outside bounds"
    );
    let mut doc = m.to_document();
    doc.layers.push(doc.layers[0].clone());
    assert!(
        Tilemap::from_document(&doc, None).is_err(),
        "duplicate layer name"
    );
    assert!(Tilemap::new(CellRect::new([0, 0], [70_000, 4])).is_err());
    assert!(Tilemap::from_json(br#"{"version":1,"layers":[],"extra":1}"#, None).is_err());
    let mut doc = ts.document().clone();
    doc.tiles.push(tile(3, CollisionKind::None));
    assert!(Tileset::from_document(doc).is_err(), "duplicate tile id");
    let mut doc = ts.document().clone();
    doc.rule_tiles[0].rules[0].output = 100;
    assert!(
        Tileset::from_document(doc).is_err(),
        "rule output must be concrete"
    );
    let mut doc = ts.document().clone();
    doc.tiles[0].collision = CollisionKind::Rect {
        min: [0.5, 0.0],
        max: [0.5, 1.0],
    };
    assert!(Tileset::from_document(doc).is_err(), "empty rect");
    assert!(ts.validate_texture(256, 256).is_ok());
    assert!(ts.validate_texture(8, 8).is_err());
}

#[test]
fn chunk_geometry_bakes_orientation_into_uvs() {
    let ts = tileset_with(vec![]);
    let mut m = map();
    let layer = m.layer_mut("ground").unwrap();
    layer.set([0, 0], 9, 0, Some(&ts)).unwrap(); // source [16,16,16,16]
    layer.set([1, 0], 9, TRANSPOSE | FLIP_X, Some(&ts)).unwrap();
    let g = chunk_geometry(layer, [0, 0], &ts, [2.0, 2.0], [128, 128]).unwrap();
    assert_eq!(g.quads(), 2);
    assert_eq!(g.indices.len(), 12);
    assert_eq!(&g.indices[..6], &[0, 1, 2, 0, 2, 3]);
    assert_eq!(g.positions[0], [0.0, 0.0]);
    assert_eq!(g.positions[2], [2.0, 2.0]);
    // Unrotated: bottom-left shows the atlas rectangle's bottom-left (u 16/128, v 32/128).
    assert_eq!(g.uvs[0], [16.0 / 128.0, 32.0 / 128.0]);
    assert_eq!(g.uvs[2], [32.0 / 128.0, 16.0 / 128.0]);
    // Rotated 90 degrees clockwise: the top-right corner shows the art's top-left.
    assert_eq!(g.uvs[4 + 2], [16.0 / 128.0, 16.0 / 128.0]);
    assert_eq!(g.positions[4], [2.0, 0.0]);
    assert_eq!(position_to_cell([-0.1, 3.9], [2.0, 2.0]), [-1, 1]);
    assert!(chunk_geometry(layer, [0, 0], &ts, [0.0, 1.0], [128, 128]).is_err());
}

#[test]
fn chunk_capacity_and_layers_are_bounded() {
    let mut m = Tilemap::new(CellRect::new([-30000, -30000], [30000, 30000])).unwrap();
    let layer = m.layer_mut("ground").unwrap();
    let mut failed = None;
    'outer: for cy in 0..65 {
        for cx in 0..65 {
            if let Err(e) = layer.set([cx * 16, cy * 16], 1, 0, None) {
                failed = Some((cx, cy, e));
                break 'outer;
            }
        }
    }
    let (cx, cy, error) = failed.expect("4096 chunks is the cap");
    assert_eq!((cx, cy, error.code), (1, 63, "tilemap.capacity"));
    for i in 1..32 {
        m.add_layer(&format!("l{i}")).unwrap();
    }
    assert_eq!(
        m.add_layer("one-too-many").err().unwrap().code,
        "tilemap.layer"
    );
    assert!(m.remove_layer("l3"));
}
