//! Level of detail by distance bands with hysteresis: mesh LODs, then a billboard impostor,
//! then culled.
use crate::field::CellKey;
use crate::types::FoliageType;
use std::collections::HashMap;

/// What to draw for one instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LodLevel {
    /// Mesh level `n` (0 = full detail).
    Mesh(u8),
    /// The camera-facing impostor.
    Impostor,
    /// Not drawn.
    Culled,
}

impl LodLevel {
    /// Compact code for [`LodTracker`] storage: mesh level `n` is `n`, impostor 254, culled 255.
    pub fn to_code(self) -> u8 {
        match self {
            Self::Mesh(n) => n,
            Self::Impostor => 254,
            Self::Culled => 255,
        }
    }
    /// Inverse of [`LodLevel::to_code`]; `UNKNOWN_CODE` (and anything else unused) is `None`.
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            UNKNOWN_CODE => None,
            254 => Some(Self::Impostor),
            255 => Some(Self::Culled),
            n if n < 8 => Some(Self::Mesh(n)),
            _ => None,
        }
    }
}

/// Stored for instances that have no previous level.
pub const UNKNOWN_CODE: u8 = 253;

/// Distance bands of one type.
///
/// Levels in order: mesh `0..mesh_levels`, then the impostor (when present), then culled.
/// `edges[i]` is the distance between level `i` and level `i + 1` (the last edge is the cull
/// distance). With hysteresis `h` a level is left outward past `edge * (1 + h)` and entered
/// again inward only below `edge * (1 - h)`, so an instance hovering near an edge does not
/// flicker.
#[derive(Clone, Debug, PartialEq)]
pub struct LodBands {
    edges: Vec<f32>,
    mesh_levels: u8,
    has_impostor: bool,
    hysteresis: f32,
}

impl LodBands {
    /// The bands of a validated type. `default_cull` applies when the type has no
    /// `cull_distance`; `scale` multiplies every distance (a global quality bias);
    /// `hysteresis` is a fraction in 0..=0.5.
    pub fn for_type(t: &FoliageType, default_cull: f32, scale: f32, hysteresis: f32) -> Self {
        let mut edges: Vec<f32> = t.lod_distances.iter().map(|d| d * scale).collect();
        let has_impostor = t.impostor.is_some() && t.impostor_distance.is_some();
        if has_impostor {
            edges.push(t.impostor_distance.unwrap_or(0.0) * scale);
        }
        let last = edges.last().copied().unwrap_or(0.0);
        let cull = t.cull_distance.unwrap_or(default_cull) * scale;
        edges.push(cull.max(last * 1.001 + 1e-3));
        Self {
            edges,
            mesh_levels: t.mesh_levels() as u8,
            has_impostor,
            hysteresis: hysteresis.clamp(0.0, 0.5),
        }
    }

    /// Distance beyond which the instance is culled.
    pub fn cull_distance(&self) -> f32 {
        *self.edges.last().expect("at least the cull edge")
    }
    /// The band edges (mesh handovers, the impostor edge when present, then the cull edge).
    pub fn edges(&self) -> &[f32] {
        &self.edges
    }
    pub fn mesh_levels(&self) -> u8 {
        self.mesh_levels
    }
    pub fn has_impostor(&self) -> bool {
        self.has_impostor
    }
    pub fn hysteresis(&self) -> f32 {
        self.hysteresis
    }

    fn level_at(&self, index: usize) -> LodLevel {
        let meshes = self.mesh_levels as usize;
        if index < meshes {
            LodLevel::Mesh(index as u8)
        } else if self.has_impostor && index == meshes {
            LodLevel::Impostor
        } else {
            LodLevel::Culled
        }
    }
    fn index_of(&self, level: LodLevel) -> usize {
        match level {
            LodLevel::Mesh(n) => (n as usize).min(self.mesh_levels as usize - 1),
            LodLevel::Impostor => self.mesh_levels as usize,
            LodLevel::Culled => self.edges.len(),
        }
    }

    /// The level for `distance` (metres) without memory: the plain bands.
    pub fn select_plain(&self, distance: f32) -> LodLevel {
        let index = self.edges.iter().take_while(|e| distance > **e).count();
        self.level_at(index)
    }

    /// The level for `distance` given the instance's `previous` level (hysteresis applies
    /// when it is known).
    pub fn select(&self, distance: f32, previous: Option<LodLevel>) -> LodLevel {
        let Some(previous) = previous else {
            return self.select_plain(distance);
        };
        let h = self.hysteresis;
        let top = self.edges.len();
        let mut index = self.index_of(previous).min(top);
        while index < top && distance > self.edges[index] * (1.0 + h) {
            index += 1;
        }
        while index > 0 && distance < self.edges[index - 1] * (1.0 - h) {
            index -= 1;
        }
        self.level_at(index)
    }
}

/// Remembers each instance's last level so hysteresis works frame to frame. Levels are kept
/// per cell and reset when the cell's revision (or size) changes.
#[derive(Clone, Debug, Default)]
pub struct LodTracker {
    cells: HashMap<CellKey, (u64, Vec<u8>)>,
}

impl LodTracker {
    pub fn new() -> Self {
        Self::default()
    }
    /// The level codes of a cell's instances (same order as the cell), all [`UNKNOWN_CODE`]
    /// after a reset.
    pub fn levels_mut(&mut self, key: CellKey, revision: u64, count: usize) -> &mut [u8] {
        let entry = self
            .cells
            .entry(key)
            .or_insert_with(|| (revision, vec![UNKNOWN_CODE; count]));
        if entry.0 != revision || entry.1.len() != count {
            *entry = (revision, vec![UNKNOWN_CODE; count]);
        }
        &mut entry.1
    }
    /// Forgets cells for which `keep` is false.
    pub fn retain(&mut self, mut keep: impl FnMut(CellKey) -> bool) {
        self.cells.retain(|k, _| keep(*k));
    }
    pub fn clear(&mut self) {
        self.cells.clear();
    }
    pub fn len(&self) -> usize {
        self.cells.len()
    }
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Impostor;

    fn tree() -> FoliageType {
        let mut t = FoliageType::new("tree", "t0");
        t.lod_meshes = vec!["t1".into(), "t2".into()];
        t.lod_distances = vec![20.0, 60.0];
        t.impostor = Some(Impostor {
            texture: "card".into(),
            size: [4.0, 6.0],
        });
        t.impostor_distance = Some(150.0);
        t.cull_distance = Some(400.0);
        t
    }

    #[test]
    fn plain_bands_cover_meshes_impostor_and_cull() {
        let b = LodBands::for_type(&tree(), 1000.0, 1.0, 0.1);
        assert_eq!(b.edges(), &[20.0, 60.0, 150.0, 400.0]);
        for (d, level) in [
            (0.0, LodLevel::Mesh(0)),
            (20.0, LodLevel::Mesh(0)),
            (20.1, LodLevel::Mesh(1)),
            (60.1, LodLevel::Mesh(2)),
            (149.0, LodLevel::Mesh(2)),
            (150.1, LodLevel::Impostor),
            (399.0, LodLevel::Impostor),
            (400.1, LodLevel::Culled),
        ] {
            assert_eq!(b.select_plain(d), level, "distance {d}");
        }
    }

    #[test]
    fn types_without_an_impostor_cull_after_the_last_mesh_and_use_the_default_cull() {
        let mut t = FoliageType::new("grass", "g");
        t.lod_meshes = vec!["g1".into()];
        t.lod_distances = vec![15.0];
        let b = LodBands::for_type(&t, 80.0, 1.0, 0.0);
        assert_eq!(b.select_plain(10.0), LodLevel::Mesh(0));
        assert_eq!(b.select_plain(30.0), LodLevel::Mesh(1));
        assert_eq!(b.select_plain(81.0), LodLevel::Culled);
        assert_eq!(b.cull_distance(), 80.0);
    }

    #[test]
    fn hysteresis_delays_switching_in_both_directions() {
        let b = LodBands::for_type(&tree(), 1000.0, 1.0, 0.1);
        // Outward: LOD0 stays until 20 * 1.1 = 22.
        assert_eq!(b.select(21.0, Some(LodLevel::Mesh(0))), LodLevel::Mesh(0));
        assert_eq!(b.select(22.1, Some(LodLevel::Mesh(0))), LodLevel::Mesh(1));
        // Inward: LOD1 stays until 20 * 0.9 = 18.
        assert_eq!(b.select(19.0, Some(LodLevel::Mesh(1))), LodLevel::Mesh(1));
        assert_eq!(b.select(17.9, Some(LodLevel::Mesh(1))), LodLevel::Mesh(0));
        // A distance inside the dead band follows whichever level it came from.
        assert_eq!(b.select(20.5, Some(LodLevel::Mesh(0))), LodLevel::Mesh(0));
        assert_eq!(b.select(20.5, Some(LodLevel::Mesh(1))), LodLevel::Mesh(1));
        // Impostor and cull edges behave the same way.
        assert_eq!(b.select(160.0, Some(LodLevel::Mesh(2))), LodLevel::Mesh(2));
        assert_eq!(b.select(166.0, Some(LodLevel::Mesh(2))), LodLevel::Impostor);
        assert_eq!(
            b.select(430.0, Some(LodLevel::Impostor)),
            LodLevel::Impostor
        );
        assert_eq!(b.select(441.0, Some(LodLevel::Impostor)), LodLevel::Culled);
        assert_eq!(b.select(380.0, Some(LodLevel::Culled)), LodLevel::Culled);
        assert_eq!(b.select(359.0, Some(LodLevel::Culled)), LodLevel::Impostor);
        // Large jumps cross several bands at once.
        assert_eq!(b.select(1000.0, Some(LodLevel::Mesh(0))), LodLevel::Culled);
        assert_eq!(b.select(1.0, Some(LodLevel::Culled)), LodLevel::Mesh(0));
    }

    #[test]
    fn an_oscillating_camera_flips_without_hysteresis_but_not_with_it() {
        let flips = |h: f32| {
            let b = LodBands::for_type(&tree(), 1000.0, 1.0, h);
            let mut level = None;
            let mut changes = 0;
            for step in 0..200 {
                // Distance wobbles +-1 m around the 20 m edge.
                let d = 20.0 + if step % 2 == 0 { 0.8 } else { -0.8 };
                let next = b.select(d, level);
                if level.is_some_and(|l| l != next) {
                    changes += 1;
                }
                level = Some(next);
            }
            changes
        };
        assert!(flips(0.0) >= 150, "without hysteresis it flickers");
        assert_eq!(flips(0.1), 0, "with 10 % hysteresis it is stable");
    }

    #[test]
    fn scale_moves_every_band_and_codes_round_trip() {
        let b = LodBands::for_type(&tree(), 1000.0, 2.0, 0.0);
        assert_eq!(b.edges(), &[40.0, 120.0, 300.0, 800.0]);
        for level in [
            LodLevel::Mesh(0),
            LodLevel::Mesh(3),
            LodLevel::Impostor,
            LodLevel::Culled,
        ] {
            assert_eq!(LodLevel::from_code(level.to_code()), Some(level));
        }
        assert_eq!(LodLevel::from_code(UNKNOWN_CODE), None);
    }

    #[test]
    fn tracker_resets_when_the_cell_revision_changes() {
        let mut tracker = LodTracker::new();
        let key = CellKey { x: 1, z: 2 };
        tracker.levels_mut(key, 5, 3)[1] = 2;
        assert_eq!(
            tracker.levels_mut(key, 5, 3),
            &[UNKNOWN_CODE, 2, UNKNOWN_CODE]
        );
        assert_eq!(tracker.levels_mut(key, 6, 3), &[UNKNOWN_CODE; 3]);
        tracker.retain(|_| false);
        assert!(tracker.is_empty());
    }
}
