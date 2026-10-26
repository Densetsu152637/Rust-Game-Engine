//! Deterministic uniform spatial hash: a sorted entry array, never hash-map iteration order.
use glam::Vec2;

pub struct SpatialHash {
    cell: f32,
    entries: Vec<(i32, i32, u32)>,
}
impl SpatialHash {
    pub fn build(cell: f32, points: impl Iterator<Item = Vec2>) -> Self {
        let cell = cell.max(1e-3);
        let mut entries: Vec<_> = points
            .enumerate()
            .map(|(i, p)| {
                let (x, y) = Self::cell_of(cell, p);
                (x, y, i as u32)
            })
            .collect();
        entries.sort_unstable();
        Self { cell, entries }
    }
    fn cell_of(cell: f32, p: Vec2) -> (i32, i32) {
        ((p.x / cell).floor() as i32, (p.y / cell).floor() as i32)
    }
    /// Calls `visit(index)` for each entry in the cells overlapping the box `p +- reach`, in
    /// ascending (cell, index) order.
    pub fn for_each_near(&self, p: Vec2, reach: f32, mut visit: impl FnMut(u32)) {
        let (x0, y0) = Self::cell_of(self.cell, p - Vec2::splat(reach));
        let (x1, y1) = Self::cell_of(self.cell, p + Vec2::splat(reach));
        for x in x0..=x1 {
            for y in y0..=y1 {
                let start = self.entries.partition_point(|e| (e.0, e.1) < (x, y));
                for e in &self.entries[start..] {
                    if (e.0, e.1) != (x, y) {
                        break;
                    }
                    visit(e.2);
                }
            }
        }
    }
}
