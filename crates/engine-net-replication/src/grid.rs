//! Uniform spatial hash grid used for interest management. Deterministic (ordered maps).
use std::collections::BTreeMap;

pub(crate) type Cell = (i32, i32, i32);

/// Cap on cells enumerated per query before falling back to a scan of every occupied cell.
const MAX_QUERY_CELLS: i64 = 4_096;

pub(crate) struct SpatialGrid {
    cell_size: f32,
    cells: BTreeMap<Cell, Vec<u32>>,
}

impl SpatialGrid {
    pub(crate) fn new(cell_size: f32) -> Self {
        Self {
            cell_size,
            cells: BTreeMap::new(),
        }
    }

    pub(crate) fn cell_of(&self, p: [f32; 3]) -> Cell {
        let c = |v: f32| (v / self.cell_size).floor() as i32;
        (c(p[0]), c(p[1]), c(p[2]))
    }

    pub(crate) fn insert(&mut self, index: u32, cell: Cell) {
        self.cells.entry(cell).or_default().push(index);
    }

    pub(crate) fn remove(&mut self, index: u32, cell: Cell) {
        if let Some(list) = self.cells.get_mut(&cell) {
            if let Some(pos) = list.iter().position(|i| *i == index) {
                list.swap_remove(pos);
            }
            if list.is_empty() {
                self.cells.remove(&cell);
            }
        }
    }

    /// Appends the entity indices of every cell overlapping the sphere's bounding box, sorted
    /// ascending. Callers filter by exact distance.
    pub(crate) fn query(&self, center: [f32; 3], radius: f32, out: &mut Vec<u32>) {
        let lo = self.cell_of([center[0] - radius, center[1] - radius, center[2] - radius]);
        let hi = self.cell_of([center[0] + radius, center[1] + radius, center[2] + radius]);
        let span = |a: i32, b: i32| i64::from(b) - i64::from(a) + 1;
        let cells = span(lo.0, hi.0)
            .saturating_mul(span(lo.1, hi.1))
            .saturating_mul(span(lo.2, hi.2));
        let start = out.len();
        if cells > MAX_QUERY_CELLS || cells as usize > self.cells.len() {
            for (cell, list) in &self.cells {
                if (lo.0..=hi.0).contains(&cell.0)
                    && (lo.1..=hi.1).contains(&cell.1)
                    && (lo.2..=hi.2).contains(&cell.2)
                {
                    out.extend_from_slice(list);
                }
            }
        } else {
            for x in lo.0..=hi.0 {
                for y in lo.1..=hi.1 {
                    for z in lo.2..=hi.2 {
                        if let Some(list) = self.cells.get(&(x, y, z)) {
                            out.extend_from_slice(list);
                        }
                    }
                }
            }
        }
        out[start..].sort_unstable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_returns_overlapping_cells_sorted() {
        let mut g = SpatialGrid::new(10.0);
        for (i, x) in [0.0f32, 5.0, 15.0, 95.0, -25.0].iter().enumerate() {
            let c = g.cell_of([*x, 0.0, 0.0]);
            g.insert(i as u32, c);
        }
        let mut out = Vec::new();
        g.query([0.0, 0.0, 0.0], 12.0, &mut out);
        assert_eq!(out, vec![0, 1, 2]);
        out.clear();
        g.query([0.0, 0.0, 0.0], 1.0e6, &mut out);
        assert_eq!(out, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn remove_drops_empty_cells() {
        let mut g = SpatialGrid::new(10.0);
        let c = g.cell_of([1.0, 1.0, 1.0]);
        g.insert(7, c);
        g.remove(7, c);
        let mut out = Vec::new();
        g.query([1.0, 1.0, 1.0], 5.0, &mut out);
        assert!(out.is_empty());
    }
}
