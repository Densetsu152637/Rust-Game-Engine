//! Collision generation: greedy rectangle merging of solid cells and outline polygons.
use crate::map::{CellPos, CellRect, TileLayer};
use crate::tileset::{CollisionKind, Tileset};
use std::collections::{BTreeMap, BTreeSet};

/// Axis-aligned box in cell units (y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    /// Bottom-left corner.
    pub min: [f32; 2],
    /// Top-right corner.
    pub max: [f32; 2],
}
impl Aabb {
    /// Width and height.
    pub fn size(&self) -> [f32; 2] {
        [self.max[0] - self.min[0], self.max[1] - self.min[1]]
    }
    /// Area.
    pub fn area(&self) -> f32 {
        let s = self.size();
        s[0] * s[1]
    }
}

/// Greedily merges `cells` into rectangles: scanning rows bottom-up and left-to-right, each
/// rectangle extends right as far as the row is solid, then up while whole rows stay solid.
/// The rectangles are disjoint and their union is exactly `cells`.
pub fn merge_cells(cells: &BTreeSet<CellPos>) -> Vec<CellRect> {
    let mut remaining: BTreeSet<(i32, i32)> = cells.iter().map(|c| (c[1], c[0])).collect();
    let mut rects = Vec::new();
    while let Some(&(y, x)) = remaining.iter().next() {
        let mut x1 = x + 1;
        while remaining.contains(&(y, x1)) {
            x1 += 1;
        }
        let mut y1 = y + 1;
        while (x..x1).all(|xx| remaining.contains(&(y1, xx))) {
            y1 += 1;
        }
        for yy in y..y1 {
            for xx in x..x1 {
                remaining.remove(&(yy, xx));
            }
        }
        rects.push(CellRect::new([x, y], [x1, y1]));
    }
    rects
}

/// Cells of `layer` whose displayed tile has [`CollisionKind::Full`] collision.
pub fn solid_cells(layer: &TileLayer, tileset: &Tileset) -> BTreeSet<CellPos> {
    layer
        .cells()
        .into_iter()
        .filter(|(_, cell)| {
            tileset
                .concrete(cell.shown)
                .is_some_and(|t| t.collision == CollisionKind::Full)
        })
        .map(|(pos, _)| pos)
        .collect()
}

/// Collision boxes for a layer in cell units: merged rectangles of full cells, followed by one
/// box per custom rectangle tile and the bounding box of each custom polygon tile (oriented by
/// the cell's display flags).
pub fn collision_boxes(layer: &TileLayer, tileset: &Tileset) -> Vec<Aabb> {
    let mut boxes: Vec<Aabb> = merge_cells(&solid_cells(layer, tileset))
        .into_iter()
        .map(|r| Aabb {
            min: [r.min[0] as f32, r.min[1] as f32],
            max: [r.max[0] as f32, r.max[1] as f32],
        })
        .collect();
    for (pos, cell) in layer.cells() {
        let Some(def) = tileset.concrete(cell.shown) else {
            continue;
        };
        let Some(points) = def.collision.oriented_points(cell.shown_flags) else {
            continue;
        };
        let mut low = [f32::INFINITY; 2];
        let mut high = [f32::NEG_INFINITY; 2];
        for p in points {
            for axis in 0..2 {
                low[axis] = low[axis].min(p[axis]);
                high[axis] = high[axis].max(p[axis]);
            }
        }
        boxes.push(Aabb {
            min: [pos[0] as f32 + low[0], pos[1] as f32 + low[1]],
            max: [pos[0] as f32 + high[0], pos[1] as f32 + high[1]],
        });
    }
    boxes
}

/// Outline polygons in cell units: closed loops around groups of full cells (outer loops run
/// counter-clockwise, holes clockwise, collinear points removed) plus each custom polygon or
/// rectangle tile as its own counter-clockwise polygon.
pub fn collision_outlines(layer: &TileLayer, tileset: &Tileset) -> Vec<Vec<[f32; 2]>> {
    let solid = solid_cells(layer, tileset);
    let mut outlines = trace_outlines(&solid);
    for (pos, cell) in layer.cells() {
        let Some(def) = tileset.concrete(cell.shown) else {
            continue;
        };
        if let Some(mut points) = def.collision.oriented_points(cell.shown_flags) {
            for p in &mut points {
                *p = [pos[0] as f32 + p[0], pos[1] as f32 + p[1]];
            }
            if signed_area(&points) < 0.0 {
                points.reverse();
            }
            outlines.push(points);
        }
    }
    outlines
}

/// Signed area (positive for counter-clockwise).
pub fn signed_area(points: &[[f32; 2]]) -> f32 {
    let mut sum = 0.0;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        sum += a[0] * b[1] - b[0] * a[1];
    }
    sum / 2.0
}

fn trace_outlines(solid: &BTreeSet<CellPos>) -> Vec<Vec<[f32; 2]>> {
    type V = (i32, i32);
    let mut edges: BTreeMap<V, Vec<V>> = BTreeMap::new();
    for &[x, y] in solid {
        let open = |dx: i32, dy: i32| !solid.contains(&[x + dx, y + dy]);
        if open(0, -1) {
            edges.entry((x, y)).or_default().push((x + 1, y));
        }
        if open(1, 0) {
            edges.entry((x + 1, y)).or_default().push((x + 1, y + 1));
        }
        if open(0, 1) {
            edges.entry((x + 1, y + 1)).or_default().push((x, y + 1));
        }
        if open(-1, 0) {
            edges.entry((x, y + 1)).or_default().push((x, y));
        }
    }
    let mut loops = Vec::new();
    while let Some((&start, targets)) = edges.iter().next() {
        let first = targets[0];
        let mut path = vec![start];
        let (mut from, mut to) = (start, first);
        remove_edge(&mut edges, from, to);
        loop {
            if to == start {
                break;
            }
            path.push(to);
            let incoming = (to.0 - from.0, to.1 - from.1);
            let candidates = edges.get(&to).cloned().unwrap_or_default();
            // Prefer a left turn, then straight, then a right turn: touching corners stay separate.
            let next = [
                (-incoming.1, incoming.0),
                incoming,
                (incoming.1, -incoming.0),
            ]
            .into_iter()
            .find_map(|d| {
                candidates
                    .iter()
                    .find(|c| (c.0 - to.0, c.1 - to.1) == d)
                    .copied()
            });
            let Some(next) = next else { break };
            remove_edge(&mut edges, to, next);
            from = to;
            to = next;
        }
        loops.push(simplify(&path));
    }
    loops
}
fn remove_edge(
    edges: &mut BTreeMap<(i32, i32), Vec<(i32, i32)>>,
    from: (i32, i32),
    to: (i32, i32),
) {
    if let Some(list) = edges.get_mut(&from) {
        if let Some(i) = list.iter().position(|t| *t == to) {
            list.remove(i);
        }
        if list.is_empty() {
            edges.remove(&from);
        }
    }
}
fn simplify(path: &[(i32, i32)]) -> Vec<[f32; 2]> {
    let n = path.len();
    let mut out = Vec::new();
    for i in 0..n {
        let (a, b, c) = (path[(i + n - 1) % n], path[i], path[(i + 1) % n]);
        let cross = (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0);
        if cross != 0 {
            out.push([b.0 as f32, b.1 as f32]);
        }
    }
    out
}
