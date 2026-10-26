//! The cell grid: absolute f64 positions to integer cell keys and box distances.
use serde::{Deserialize, Serialize};
use std::fmt;

/// Largest accepted cell size in metres.
pub const MAX_CELL_SIZE: f64 = 1.0e9;

/// Integer coordinates of a cell. The cell with key `k` spans `[k * size, (k + 1) * size)` on
/// every gridded axis. In a [`GridPlane::Xz`] grid the `y` key is always `0`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct CellKey(pub [i32; 3]);

impl fmt::Display for CellKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{},{}", self.0[0], self.0[1], self.0[2])
    }
}

/// Which axes are divided into cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridPlane {
    /// Horizontal grid: cells are infinite columns along `y` (key `y` is `0`).
    #[default]
    Xz,
    /// Full three dimensional grid.
    Xyz,
}

/// A uniform cell grid anchored at the absolute origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    cell_size: f64,
    plane: GridPlane,
}

/// Why a grid could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridError(pub String);

impl fmt::Display for GridError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for GridError {}

impl Grid {
    /// A grid with `cell_size` metres per cell edge; the size must be finite and in `(0, 1e9]`.
    pub fn new(cell_size: f64, plane: GridPlane) -> Result<Self, GridError> {
        if !(cell_size.is_finite() && cell_size > 0.0 && cell_size <= MAX_CELL_SIZE) {
            return Err(GridError(format!(
                "cell size must be finite and in (0, {MAX_CELL_SIZE}]"
            )));
        }
        Ok(Self { cell_size, plane })
    }
    pub fn cell_size(&self) -> f64 {
        self.cell_size
    }
    pub fn plane(&self) -> GridPlane {
        self.plane
    }
    /// The cell that contains `position` (absolute metres). Non-finite input maps to cell 0 and
    /// keys saturate at the `i32` range.
    pub fn key_of(&self, position: [f64; 3]) -> CellKey {
        let axis = |v: f64| {
            if v.is_finite() {
                (v / self.cell_size)
                    .floor()
                    .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
            } else {
                0
            }
        };
        let y = if self.plane == GridPlane::Xz {
            0
        } else {
            axis(position[1])
        };
        CellKey([axis(position[0]), y, axis(position[2])])
    }
    /// Absolute minimum corner of the cell. In an `Xz` grid the `y` component is `0`.
    pub fn origin(&self, key: CellKey) -> [f64; 3] {
        let y = if self.plane == GridPlane::Xz {
            0.0
        } else {
            f64::from(key.0[1]) * self.cell_size
        };
        [
            f64::from(key.0[0]) * self.cell_size,
            y,
            f64::from(key.0[2]) * self.cell_size,
        ]
    }
    /// Distance from `position` to the cell's box (zero inside). In an `Xz` grid the vertical
    /// axis is ignored.
    pub fn distance(&self, position: [f64; 3], key: CellKey) -> f64 {
        let min = self.origin(key);
        let mut sum = 0.0;
        for axis in 0..3 {
            if axis == 1 && self.plane == GridPlane::Xz {
                continue;
            }
            let lo = min[axis];
            let hi = lo + self.cell_size;
            let d = if position[axis] < lo {
                lo - position[axis]
            } else if position[axis] > hi {
                position[axis] - hi
            } else {
                0.0
            };
            sum += d * d;
        }
        sum.sqrt()
    }
    /// Position relative to the cell's [`origin`](Self::origin), in f64.
    pub fn to_local(&self, key: CellKey, position: [f64; 3]) -> [f64; 3] {
        let o = self.origin(key);
        std::array::from_fn(|i| position[i] - o[i])
    }
    /// Assigns each position to its cell. Returns `(index, key)` pairs in input order.
    pub fn assign(&self, positions: &[[f64; 3]]) -> Vec<CellKey> {
        positions.iter().map(|p| self.key_of(*p)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid::new(100.0, GridPlane::Xz).unwrap()
    }

    #[test]
    fn keys_floor_including_negative_and_far_positions() {
        let g = grid();
        assert_eq!(g.key_of([0.0, 5.0, 99.999]), CellKey([0, 0, 0]));
        assert_eq!(g.key_of([100.0, 0.0, -0.001]), CellKey([1, 0, -1]));
        assert_eq!(g.key_of([-100.0, 9e9, -100.5]), CellKey([-1, 0, -2]));
        // More than 10 km out the f64 key stays exact.
        assert_eq!(
            g.key_of([12_345.678, 0.0, -23_456.789]),
            CellKey([123, 0, -235])
        );
        assert_eq!(
            g.origin(CellKey([123, 0, -235])),
            [12_300.0, 0.0, -23_500.0]
        );
    }

    #[test]
    fn xyz_grid_divides_height() {
        let g = Grid::new(10.0, GridPlane::Xyz).unwrap();
        assert_eq!(g.key_of([1.0, 25.0, 1.0]), CellKey([0, 2, 0]));
        assert_eq!(g.origin(CellKey([0, 2, 0])), [0.0, 20.0, 0.0]);
    }

    #[test]
    fn distance_is_zero_inside_and_euclidean_to_the_box() {
        let g = grid();
        let k = CellKey([0, 0, 0]);
        assert_eq!(g.distance([50.0, 1e6, 50.0], k), 0.0);
        assert_eq!(g.distance([-30.0, 0.0, 50.0], k), 30.0);
        assert!((g.distance([-30.0, 0.0, 140.0], k) - 50.0).abs() < 1e-12);
        let g3 = Grid::new(100.0, GridPlane::Xyz).unwrap();
        assert!((g3.distance([50.0, 140.0, 50.0], k) - 40.0).abs() < 1e-12);
    }

    #[test]
    fn precision_survives_far_from_the_origin() {
        let g = grid();
        let far = 50_000_000.25;
        let key = g.key_of([far, 0.0, 0.0]);
        assert_eq!(key, CellKey([500_000, 0, 0]));
        assert!((g.to_local(key, [far, 0.0, 0.0])[0] - 0.25).abs() < 1e-9);
    }

    #[test]
    fn invalid_sizes_are_rejected_and_bad_input_is_total() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 2e9] {
            assert!(Grid::new(bad, GridPlane::Xz).is_err(), "{bad}");
        }
        assert_eq!(
            grid().key_of([f64::NAN, 0.0, f64::INFINITY]),
            CellKey([0, 0, 0])
        );
        assert_eq!(grid().key_of([1e300, 0.0, 0.0]).0[0], i32::MAX);
    }
}
