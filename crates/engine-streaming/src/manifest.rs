//! The partition manifest: which cells exist, where their content lives and their HLOD proxy.
//!
//! Cell content files are ordinary scene documents (`{"schema":1,"scene_id":...,"entities":[...]}`).
//! Root entities (no `parent`) carry positions **relative to the cell's minimum corner**, so
//! the stored f32 values stay precise at any distance from the origin; the runtime adds the
//! cell's absolute f64 origin minus the current world origin when it spawns them. Children keep
//! their parent-relative transforms. A cell's HLOD proxy mesh uses the same cell-local space.
use crate::grid::{CellKey, Grid, GridPlane};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

pub const MANIFEST_SCHEMA: u32 = 1;
/// Most cells one manifest may list.
pub const MAX_CELLS: usize = 65_536;
const MAX_NAME: usize = 64;
const MAX_PATH: usize = 260;

/// A merged, simplified stand-in mesh for a cell that is not loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HlodProxy {
    /// Project-relative mesh file (the project's JSON mesh format) in cell-local space.
    pub file: String,
    /// Triangles of the proxy.
    #[serde(default)]
    pub triangles: u32,
    /// Triangles of the merged cell content before simplification.
    #[serde(default)]
    pub source_triangles: u32,
}

/// One cell (or one data layer of one cell).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestCell {
    pub key: CellKey,
    /// Data layer holding the content; empty for the always-present default layer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layer: String,
    /// Project-relative scene document with the cell's entities.
    pub file: String,
    #[serde(default)]
    pub entities: u32,
    /// Size of `file` in bytes, used by the byte budget.
    #[serde(default)]
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hlod: Option<HlodProxy>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionManifest {
    pub schema: u32,
    pub cell_size: f64,
    #[serde(default)]
    pub plane: GridPlane,
    pub cells: Vec<ManifestCell>,
}

/// A manifest that cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError(pub String);
impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ManifestError {}

impl PartitionManifest {
    pub fn new(cell_size: f64, plane: GridPlane) -> Self {
        Self {
            schema: MANIFEST_SCHEMA,
            cell_size,
            plane,
            cells: Vec::new(),
        }
    }
    /// Parses and validates a manifest document.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ManifestError> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|e| ManifestError(format!("partition manifest: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }
    /// Canonical pretty JSON (cells sorted by key then layer).
    pub fn to_json(&self) -> String {
        let mut sorted = self.clone();
        sorted
            .cells
            .sort_by(|a, b| (a.key, &a.layer).cmp(&(b.key, &b.layer)));
        let mut text = serde_json::to_string_pretty(&sorted).expect("plain data");
        text.push('\n');
        text
    }
    pub fn grid(&self) -> Result<Grid, ManifestError> {
        Grid::new(self.cell_size, self.plane).map_err(|e| ManifestError(e.0))
    }
    pub fn validate(&self) -> Result<(), ManifestError> {
        let bad = |m: String| Err(ManifestError(format!("partition manifest: {m}")));
        if self.schema != MANIFEST_SCHEMA {
            return bad(format!("unsupported schema {}", self.schema));
        }
        self.grid()?;
        if self.cells.len() > MAX_CELLS {
            return bad(format!("more than {MAX_CELLS} cells"));
        }
        let mut seen = BTreeSet::new();
        for cell in &self.cells {
            if !seen.insert((cell.key, cell.layer.as_str())) {
                return bad(format!(
                    "duplicate cell {} layer `{}`",
                    cell.key, cell.layer
                ));
            }
            if self.plane == GridPlane::Xz && cell.key.0[1] != 0 {
                return bad(format!("cell {} has y != 0 in an xz grid", cell.key));
            }
            if cell.layer.len() > MAX_NAME || cell.layer.chars().any(char::is_control) {
                return bad(format!("cell {} has an invalid layer name", cell.key));
            }
            check_path(&cell.file)?;
            if let Some(hlod) = &cell.hlod {
                check_path(&hlod.file)?;
            }
        }
        Ok(())
    }
}

fn check_path(path: &str) -> Result<(), ManifestError> {
    let ok = !path.is_empty()
        && path.len() <= MAX_PATH
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if ok {
        Ok(())
    } else {
        Err(ManifestError(format!(
            "partition manifest: `{path}` is not a project-relative forward-slash path"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: i32, z: i32, layer: &str) -> ManifestCell {
        ManifestCell {
            key: CellKey([x, 0, z]),
            layer: layer.into(),
            file: format!("cells/{x}_{z}.json"),
            entities: 1,
            bytes: 100,
            hlod: None,
        }
    }

    #[test]
    fn round_trips_sorted_and_validated() {
        let mut m = PartitionManifest::new(64.0, GridPlane::Xz);
        m.cells = vec![cell(2, 0, ""), cell(-1, 3, "props")];
        let text = m.to_json();
        let back = PartitionManifest::from_json(text.as_bytes()).unwrap();
        assert_eq!(back.cells[0].key, CellKey([-1, 0, 3]));
        assert_eq!(back.cells.len(), 2);
        assert_eq!(back.to_json(), text);
    }

    #[test]
    fn rejects_duplicates_bad_paths_and_unknown_fields() {
        let mut m = PartitionManifest::new(64.0, GridPlane::Xz);
        m.cells = vec![cell(0, 0, ""), cell(0, 0, "")];
        assert!(m.validate().is_err());
        for bad in ["../x.json", "/abs.json", "a\\b.json", "c:/x", "", "a//b"] {
            let mut m = PartitionManifest::new(64.0, GridPlane::Xz);
            let mut c = cell(0, 0, "");
            c.file = bad.into();
            m.cells = vec![c];
            assert!(m.validate().is_err(), "{bad}");
        }
        let mut raised = PartitionManifest::new(64.0, GridPlane::Xz);
        let mut c = cell(0, 0, "");
        c.key = CellKey([0, 1, 0]);
        raised.cells = vec![c];
        assert!(raised.validate().is_err());
        assert!(
            PartitionManifest::from_json(br#"{"schema":1,"cell_size":8,"cells":[],"extra":1}"#)
                .is_err()
        );
        assert!(PartitionManifest::from_json(br#"{"schema":1,"cell_size":0,"cells":[]}"#).is_err());
    }
}
