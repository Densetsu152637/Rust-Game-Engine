//! Instance storage in spatial cells: bounded and serializable.
use crate::{Error, Result};
use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Most instances one field stores.
pub const MAX_INSTANCES: usize = 262_144;
/// Smallest and largest cell edge in metres.
pub const MIN_CELL_SIZE: f32 = 1.0;
pub const MAX_CELL_SIZE: f32 = 4096.0;

/// A cell coordinate: floor of `position / cell_size` along X and Z.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct CellKey {
    pub x: i32,
    pub z: i32,
}

/// A revision base unique in this process: every new field starts in its own block of 2^32
/// revisions, so a regenerated field never reuses the revision numbers of the one it replaces
/// (renderers compare cell revisions to decide what is stale).
fn fresh_revision() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) << 32
}

/// One plant.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    /// Index into the type list the field was built with.
    pub type_index: u16,
    /// World position of the origin (on the ground, plus the type's ground offset).
    pub position: [f32; 3],
    /// Unit quaternion `[x, y, z, w]`.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    /// Wind phase in 0..1, fixed at placement so sway differs between neighbours.
    pub phase: f32,
}

impl Instance {
    /// An upright, unit-scale instance at `position`.
    pub fn new(type_index: u16, position: [f32; 3]) -> Self {
        Self {
            type_index,
            position,
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            phase: 0.0,
        }
    }
    pub fn quat(&self) -> Quat {
        Quat::from_xyzw(
            self.rotation[0],
            self.rotation[1],
            self.rotation[2],
            self.rotation[3],
        )
    }
    /// World-from-local matrix (scale, then rotation, then translation).
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(
            Vec3::from(self.scale),
            self.quat(),
            Vec3::from(self.position),
        )
    }
    /// Whether every component is finite, the rotation is a unit quaternion (1e-2), scales are
    /// positive and the phase is in 0..=1.
    pub fn is_valid(&self) -> bool {
        let finite = self
            .position
            .iter()
            .chain(&self.rotation)
            .chain(&self.scale)
            .chain(std::iter::once(&self.phase))
            .all(|v| v.is_finite());
        let length = self.quat().length();
        finite
            && (length - 1.0).abs() < 1e-2
            && self.scale.iter().all(|s| *s > 0.0 && *s <= 10_000.0)
            && (0.0..=1.0).contains(&self.phase)
    }
}

/// The instances of one cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    pub instances: Vec<Instance>,
    /// Field revision at this cell's last change (renderers rebuild only newer cells).
    pub revision: u64,
}

/// Instances grouped in square cells of `cell_size` metres on the XZ plane.
///
/// Every mutation bumps [`FoliageField::revision`] and the revision of each cell it touched.
/// At most [`MAX_INSTANCES`] instances are stored. Iteration order is deterministic
/// (cells sorted by key, instances in insertion order).
#[derive(Clone, Debug, PartialEq)]
pub struct FoliageField {
    cell_size: f32,
    cells: BTreeMap<CellKey, Cell>,
    count: usize,
    revision: u64,
}

/// The serialized form: bounded and validated on the way in.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    cell_size: f32,
    instances: Vec<Instance>,
}

impl Serialize for FoliageField {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        Snapshot {
            cell_size: self.cell_size,
            instances: self.iter().copied().collect(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FoliageField {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let snapshot = Snapshot::deserialize(deserializer)?;
        let mut field = FoliageField::new(snapshot.cell_size).map_err(serde::de::Error::custom)?;
        field
            .extend(snapshot.instances)
            .map_err(serde::de::Error::custom)?;
        Ok(field)
    }
}

impl FoliageField {
    /// An empty field with square cells of `cell_size` metres (1..=4096).
    pub fn new(cell_size: f32) -> Result<Self> {
        if !(cell_size.is_finite() && (MIN_CELL_SIZE..=MAX_CELL_SIZE).contains(&cell_size)) {
            return Err(Error::new(
                "foliage.field.cell_size",
                "cell_size must be within 1..=4096 metres",
            ));
        }
        Ok(Self {
            cell_size,
            cells: BTreeMap::new(),
            count: 0,
            revision: fresh_revision(),
        })
    }
    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }
    /// Instances stored.
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    /// Non-empty cells.
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }
    /// Incremented by every mutation; starts at a value unique among all fields of the process.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// The cell containing world `(x, z)`.
    pub fn key_of(&self, x: f32, z: f32) -> CellKey {
        CellKey {
            x: (x / self.cell_size).floor() as i32,
            z: (z / self.cell_size).floor() as i32,
        }
    }
    /// World `[min, max]` rectangle (x, z) of a cell.
    pub fn cell_rect(&self, key: CellKey) -> [[f32; 2]; 2] {
        let s = self.cell_size;
        [
            [key.x as f32 * s, key.z as f32 * s],
            [(key.x + 1) as f32 * s, (key.z + 1) as f32 * s],
        ]
    }
    /// Non-empty cells in key order.
    pub fn cells(&self) -> impl Iterator<Item = (CellKey, &Cell)> {
        self.cells.iter().map(|(k, c)| (*k, c))
    }
    pub fn cell(&self, key: CellKey) -> Option<&Cell> {
        self.cells.get(&key)
    }
    /// Every instance in cell order.
    pub fn iter(&self) -> impl Iterator<Item = &Instance> {
        self.cells.values().flat_map(|c| c.instances.iter())
    }

    /// Adds one instance.
    pub fn insert(&mut self, instance: Instance) -> Result<()> {
        if !instance.is_valid() {
            return Err(Error::new(
                "foliage.instance.invalid",
                "instances need finite values, a unit rotation, positive scale and a phase in 0..=1",
            ));
        }
        if self.count >= MAX_INSTANCES {
            return Err(Error::new(
                "foliage.limit.instances",
                format!("a field holds at most {MAX_INSTANCES} instances"),
            ));
        }
        self.revision += 1;
        let key = self.key_of(instance.position[0], instance.position[2]);
        let cell = self.cells.entry(key).or_default();
        cell.instances.push(instance);
        cell.revision = self.revision;
        self.count += 1;
        Ok(())
    }
    /// Adds many instances; stops at the first failure (earlier ones stay).
    pub fn extend(&mut self, instances: impl IntoIterator<Item = Instance>) -> Result<()> {
        instances.into_iter().try_for_each(|i| self.insert(i))
    }

    /// Instances within `radius` (horizontal) of `(x, z)`, with their cell and index.
    pub fn query_disc(&self, x: f32, z: f32, radius: f32) -> Vec<(CellKey, usize, &Instance)> {
        let mut out = Vec::new();
        let (lo, hi) = (
            self.key_of(x - radius, z - radius),
            self.key_of(x + radius, z + radius),
        );
        let r2 = radius * radius;
        for cz in lo.z..=hi.z {
            for cx in lo.x..=hi.x {
                let key = CellKey { x: cx, z: cz };
                if let Some(cell) = self.cells.get(&key) {
                    for (i, inst) in cell.instances.iter().enumerate() {
                        let (dx, dz) = (inst.position[0] - x, inst.position[2] - z);
                        if dx * dx + dz * dz <= r2 {
                            out.push((key, i, inst));
                        }
                    }
                }
            }
        }
        out
    }

    /// Whether an instance of `type_index` (any type with `None`) lies within `radius` of
    /// `(x, z)`.
    pub fn any_within(&self, x: f32, z: f32, radius: f32, type_index: Option<u16>) -> bool {
        let (lo, hi) = (
            self.key_of(x - radius, z - radius),
            self.key_of(x + radius, z + radius),
        );
        let r2 = radius * radius;
        (lo.z..=hi.z).any(|cz| {
            (lo.x..=hi.x).any(|cx| {
                self.cells
                    .get(&CellKey { x: cx, z: cz })
                    .is_some_and(|cell| {
                        cell.instances.iter().any(|i| {
                            type_index.is_none_or(|t| i.type_index == t) && {
                                let (dx, dz) = (i.position[0] - x, i.position[2] - z);
                                dx * dx + dz * dz < r2
                            }
                        })
                    })
            })
        })
    }

    /// Removes the instances for which `remove` returns `true`, optionally only inside the
    /// world rectangle `within` (`[[min x, min z], [max x, max z]]`); returns how many went.
    pub fn remove_where(
        &mut self,
        within: Option<[[f32; 2]; 2]>,
        mut remove: impl FnMut(&Instance) -> bool,
    ) -> usize {
        let keys: Vec<CellKey> = match within {
            Some([lo, hi]) => {
                let (a, b) = (self.key_of(lo[0], lo[1]), self.key_of(hi[0], hi[1]));
                self.cells
                    .range(
                        CellKey {
                            x: a.x,
                            z: i32::MIN,
                        }..=CellKey {
                            x: b.x,
                            z: i32::MAX,
                        },
                    )
                    .map(|(k, _)| *k)
                    .filter(|k| k.z >= a.z && k.z <= b.z)
                    .collect()
            }
            None => self.cells.keys().copied().collect(),
        };
        let mut removed = 0;
        let next = self.revision + 1;
        for key in keys {
            let cell = self.cells.get_mut(&key).expect("key from the map");
            let before = cell.instances.len();
            cell.instances.retain(|i| !remove(i));
            let gone = before - cell.instances.len();
            if gone > 0 {
                cell.revision = next;
                removed += gone;
            }
            if cell.instances.is_empty() {
                self.cells.remove(&key);
            }
        }
        if removed > 0 {
            self.revision = next;
            self.count -= removed;
        }
        removed
    }

    /// Applies `edit` to every instance; it returns `false` to delete the instance. Cells whose
    /// content changed get the new revision. Instances that moved to another cell are
    /// re-filed. Returns `(kept, removed)`.
    pub fn map_instances(&mut self, mut edit: impl FnMut(&mut Instance) -> bool) -> (usize, usize) {
        let old = std::mem::take(&mut self.cells);
        let before = self.count;
        self.count = 0;
        let next = self.revision + 1;
        let mut changed_cells: Vec<CellKey> = Vec::new();
        let mut moved: Vec<Instance> = Vec::new();
        for (key, cell) in old {
            let mut kept = Vec::with_capacity(cell.instances.len());
            let mut dirty = false;
            for mut inst in cell.instances {
                let original = inst;
                if !edit(&mut inst) {
                    dirty = true;
                    continue;
                }
                if inst != original {
                    dirty = true;
                }
                if self.key_of(inst.position[0], inst.position[2]) == key {
                    kept.push(inst);
                } else {
                    moved.push(inst);
                }
            }
            if dirty {
                changed_cells.push(key);
            }
            if !kept.is_empty() {
                self.count += kept.len();
                self.cells.insert(
                    key,
                    Cell {
                        instances: kept,
                        revision: if dirty { next } else { cell.revision },
                    },
                );
            }
        }
        for inst in moved {
            let key = self.key_of(inst.position[0], inst.position[2]);
            let cell = self.cells.entry(key).or_default();
            cell.instances.push(inst);
            cell.revision = next;
            self.count += 1;
        }
        let kept = self.count;
        if kept != before || !changed_cells.is_empty() {
            self.revision = next;
        }
        (kept, before - kept)
    }

    /// Removes everything.
    pub fn clear(&mut self) {
        if self.count > 0 {
            self.revision += 1;
        }
        self.cells.clear();
        self.count = 0;
    }

    /// Instances per type (`types` entries; instances with a larger index are ignored).
    pub fn counts_per_type(&self, types: usize) -> Vec<usize> {
        let mut counts = vec![0; types];
        for i in self.iter() {
            if let Some(c) = counts.get_mut(i.type_index as usize) {
                *c += 1;
            }
        }
        counts
    }

    /// World bounds of all instance positions as `(min, max)`, or `None` when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut it = self.iter().map(|i| Vec3::from(i.position));
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, z: f32) -> Instance {
        Instance::new(0, [x, 0.0, z])
    }

    #[test]
    fn instances_file_into_cells_including_negative_coordinates() {
        let mut f = FoliageField::new(10.0).unwrap();
        f.extend([at(1.0, 1.0), at(9.9, 0.1), at(-0.1, 0.0), at(15.0, -15.0)])
            .unwrap();
        assert_eq!(f.len(), 4);
        assert_eq!(f.cell_count(), 3);
        assert_eq!(f.cell(CellKey { x: 0, z: 0 }).unwrap().instances.len(), 2);
        assert!(f.cell(CellKey { x: -1, z: 0 }).is_some());
        assert!(f.cell(CellKey { x: 1, z: -2 }).is_some());
    }

    #[test]
    fn mutations_bump_revisions_and_only_touched_cells_change() {
        let mut f = FoliageField::new(10.0).unwrap();
        f.extend([at(1.0, 1.0), at(25.0, 1.0)]).unwrap();
        let r = f.revision();
        let untouched = f.cell(CellKey { x: 0, z: 0 }).unwrap().revision;
        let removed = f.remove_where(None, |i| i.position[0] > 20.0);
        assert_eq!(removed, 1);
        assert!(f.revision() > r);
        assert_eq!(f.cell(CellKey { x: 0, z: 0 }).unwrap().revision, untouched);
        assert!(f.cell(CellKey { x: 2, z: 0 }).is_none());
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn disc_queries_use_euclidean_distance_across_cells() {
        let mut f = FoliageField::new(4.0).unwrap();
        f.extend([at(3.9, 0.0), at(4.1, 0.0), at(0.0, 3.0), at(2.9, 2.9)])
            .unwrap();
        let hits = f.query_disc(4.0, 0.0, 0.5);
        assert_eq!(hits.len(), 2);
        assert!(f.any_within(4.0, 0.0, 0.5, Some(0)));
        assert!(!f.any_within(4.0, 0.0, 0.5, Some(1)));
        assert!(!f.any_within(2.0, 2.0, 0.5, None));
    }

    #[test]
    fn capacity_and_invalid_instances_are_rejected() {
        let mut f = FoliageField::new(8.0).unwrap();
        let mut bad = at(0.0, 0.0);
        bad.rotation = [0.0, 0.0, 0.0, 2.0];
        assert_eq!(f.insert(bad).unwrap_err().code, "foliage.instance.invalid");
        bad = at(0.0, 0.0);
        bad.position[1] = f32::NAN;
        assert!(f.insert(bad).is_err());
        assert!(FoliageField::new(0.5).is_err());
        assert!(FoliageField::new(f32::NAN).is_err());
    }

    #[test]
    fn map_instances_refiles_moved_instances_and_deletes() {
        let mut f = FoliageField::new(10.0).unwrap();
        f.extend([at(1.0, 1.0), at(2.0, 2.0), at(3.0, 3.0)])
            .unwrap();
        let (kept, removed) = f.map_instances(|i| {
            if i.position[0] == 1.0 {
                i.position[0] = 12.0;
            }
            i.position[0] != 3.0
        });
        assert_eq!((kept, removed), (2, 1));
        assert_eq!(f.len(), 2);
        assert_eq!(f.cell(CellKey { x: 1, z: 0 }).unwrap().instances.len(), 1);
        assert_eq!(f.cell(CellKey { x: 0, z: 0 }).unwrap().instances.len(), 1);
    }

    #[test]
    fn serde_round_trip_preserves_instances_and_validates_input() {
        let mut f = FoliageField::new(16.0).unwrap();
        f.extend([at(1.0, 1.0), at(40.0, -3.0)]).unwrap();
        let json = serde_json::to_string(&f).unwrap();
        let back: FoliageField = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(
            back.iter().copied().collect::<Vec<_>>(),
            f.iter().copied().collect::<Vec<_>>()
        );
        assert!(
            serde_json::from_str::<FoliageField>(r#"{"cell_size":0.1,"instances":[]}"#).is_err()
        );
    }
}
