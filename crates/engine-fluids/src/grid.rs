//! Spatial hash grid with deterministic ordering.
//!
//! Cells have the kernel radius as edge length. Cell coordinates hash into a power-of-two
//! bucket table and particles are counting-sorted by bucket, so the candidate order depends
//! only on positions and particle order (never on hash-map iteration order). Hash collisions
//! only add candidates; callers filter by distance.

use glam::{IVec3, Vec3};

#[derive(Clone, Debug, Default)]
pub(crate) struct NeighborGrid {
    inv_cell: f32,
    mask: u32,
    /// `starts[b]..starts[b + 1]` indexes `order` for bucket `b`.
    starts: Vec<u32>,
    order: Vec<u32>,
    bucket_of: Vec<u32>,
}

impl NeighborGrid {
    pub fn build(&mut self, positions: &[Vec3], cell: f32) {
        self.inv_cell = 1.0 / cell;
        let buckets = (positions.len() * 2).max(64).next_power_of_two();
        self.mask = (buckets - 1) as u32;
        self.starts.clear();
        self.starts.resize(buckets + 1, 0);
        self.bucket_of.clear();
        for &p in positions {
            let b = self.bucket(self.cell_of(p));
            self.bucket_of.push(b);
            self.starts[b as usize + 1] += 1;
        }
        for b in 0..buckets {
            self.starts[b + 1] += self.starts[b];
        }
        self.order.clear();
        self.order.resize(positions.len(), 0);
        let mut cursor = self.starts.clone();
        for (i, &b) in self.bucket_of.iter().enumerate() {
            let slot = &mut cursor[b as usize];
            self.order[*slot as usize] = i as u32;
            *slot += 1;
        }
    }

    #[inline]
    fn cell_of(&self, p: Vec3) -> IVec3 {
        (p * self.inv_cell).floor().as_ivec3()
    }

    #[inline]
    fn bucket(&self, c: IVec3) -> u32 {
        let h = (c.x as u32).wrapping_mul(73_856_093)
            ^ (c.y as u32).wrapping_mul(19_349_663)
            ^ (c.z as u32).wrapping_mul(83_492_791);
        h & self.mask
    }

    /// Call `visit` once for every particle in the 27 cells around `p` (plus hash-collision
    /// candidates), in deterministic order.
    #[inline]
    pub fn for_each_candidate(&self, p: Vec3, mut visit: impl FnMut(u32)) {
        if self.order.is_empty() {
            return;
        }
        let c = self.cell_of(p);
        let mut buckets = [0u32; 27];
        let mut n = 0;
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    buckets[n] = self.bucket(c + IVec3::new(dx, dy, dz));
                    n += 1;
                }
            }
        }
        buckets.sort_unstable();
        let mut previous = u32::MAX;
        for &b in &buckets {
            if b == previous {
                continue;
            }
            previous = b;
            let range = self.starts[b as usize] as usize..self.starts[b as usize + 1] as usize;
            for &j in &self.order[range] {
                visit(j);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_cover_every_neighbor_exactly_once() {
        let mut positions = Vec::new();
        for i in 0..12 {
            for j in 0..12 {
                for k in 0..12 {
                    positions.push(Vec3::new(i as f32, j as f32, k as f32) * 0.05 - 0.1);
                }
            }
        }
        let h = 0.1;
        let mut grid = NeighborGrid::default();
        grid.build(&positions, h);
        for (i, &p) in positions.iter().enumerate().step_by(37) {
            let mut seen = Vec::new();
            grid.for_each_candidate(p, |j| seen.push(j));
            let mut sorted = seen.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), seen.len(), "duplicate candidate for {i}");
            let brute: Vec<u32> = (0..positions.len() as u32)
                .filter(|&j| positions[j as usize].distance_squared(p) < h * h)
                .collect();
            for j in brute {
                assert!(seen.contains(&j), "missing neighbour {j} of {i}");
            }
        }
    }
}
