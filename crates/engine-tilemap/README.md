# engine-tilemap

Pure-Rust 2D tilemap data model and authoring (no ECS, renderer or file-system access).
Dependencies: `serde`, `serde_json`. `#![forbid(unsafe_code)]`.

- `Tileset` / `TilesetDocument`: tiles with atlas rectangles, collision kind (none, full,
  rect, polygon) and properties; `RuleTile` autotiling with `blob47` and
  `four_neighbour16` presets.
- `Tilemap` / `TileLayer` / `TilemapDocument`: layers with z order, visibility and
  opacity; sparse 16x16 chunks with `i32` cell coordinates inside fixed bounds; `set`,
  `erase`, `fill_rect`, bounded `flood_fill`; painting re-resolves only the affected
  neighbours and stamps changed chunks with a revision.
- `collision`: greedy rectangle merging, per-layer collision boxes, outline polygons.
- `geometry`: per-chunk quads (positions, UVs, indices) for a renderer.
- `orient`: the eight rotations/mirrors as three flag bits.

All documents are versioned (`version: 1`), validated and bounded; see
[docs/world-tilemap.md](../../docs/world-tilemap.md) for the formats, limits and how
`engine-project` renders and collides them.

```rust
use engine_tilemap::*;

let mut doc = TilesetDocument::new("demo", "atlas", [16, 16]);
doc.tiles = (1..=16)
    .map(|id| TileDef {
        id,
        name: String::new(),
        source: [(id - 1) % 4 * 16, (id - 1) / 4 * 16, 16, 16],
        collision: CollisionKind::Full,
        properties: Default::default(),
    })
    .collect();
doc.rule_tiles = vec![RuleTile::four_neighbour16(100, "wall", 1)];
let tileset = Tileset::from_document(doc).unwrap();

let mut map = Tilemap::new(CellRect::new([-32, -32], [32, 32])).unwrap();
let ground = map.layer_mut("ground").unwrap();
ground.set([0, 0], 100, 0, Some(&tileset)).unwrap();
ground.set([1, 0], 100, 0, Some(&tileset)).unwrap();
assert_eq!(ground.get([0, 0]).shown, 1 + 2); // east neighbour only
let boxes = collision::collision_boxes(ground, &tileset);
assert_eq!(boxes.len(), 1); // two adjacent solid cells merge into one box
```

Run `cargo test -p engine-tilemap` for the behaviour tests (exact rule-tile picks,
rectangle merge counts and union checks, outlines, formats and limits).
