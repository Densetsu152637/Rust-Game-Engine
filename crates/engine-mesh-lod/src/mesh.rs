//! Indexed triangle mesh container and boundary validation.

use glam::{Vec2, Vec3};

use crate::bounds::Aabb;
use crate::error::{MeshLodError, Result};

/// Indexed triangle mesh with optional per-vertex normals and first UV set.
///
/// Triangles are counter-clockwise `indices[3i..3i+3]`. Vertices that share a
/// position but differ in normal or UV are treated as attribute seams.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MeshData {
    pub positions: Vec<Vec3>,
    pub normals: Option<Vec<Vec3>>,
    pub uvs: Option<Vec<Vec2>>,
    pub indices: Vec<u32>,
}

impl MeshData {
    /// Mesh with positions and indices only.
    pub fn new(positions: Vec<Vec3>, indices: Vec<u32>) -> Self {
        Self {
            positions,
            normals: None,
            uvs: None,
            indices,
        }
    }

    /// Attach per-vertex normals.
    pub fn with_normals(mut self, normals: Vec<Vec3>) -> Self {
        self.normals = Some(normals);
        self
    }

    /// Attach per-vertex UV0 coordinates.
    pub fn with_uvs(mut self, uvs: Vec<Vec2>) -> Self {
        self.uvs = Some(uvs);
        self
    }

    /// Number of triangles (`indices.len() / 3`).
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Checks sizes, index range and finiteness. Empty meshes are rejected.
    pub fn validate(&self) -> Result<()> {
        if self.positions.is_empty() || self.indices.is_empty() {
            return Err(MeshLodError::EmptyMesh);
        }
        if !self.indices.len().is_multiple_of(3) {
            return Err(MeshLodError::IndexCountNotMultipleOfThree {
                len: self.indices.len(),
            });
        }
        let n = self.positions.len();
        if let Some(normals) = &self.normals
            && normals.len() != n
        {
            return Err(MeshLodError::AttributeLengthMismatch {
                attribute: "normals",
                expected: n,
                actual: normals.len(),
            });
        }
        if let Some(uvs) = &self.uvs
            && uvs.len() != n
        {
            return Err(MeshLodError::AttributeLengthMismatch {
                attribute: "uvs",
                expected: n,
                actual: uvs.len(),
            });
        }
        if let Some(i) = self.positions.iter().position(|p| !p.is_finite()) {
            return Err(MeshLodError::NonFiniteValue {
                attribute: "positions",
                index: i,
            });
        }
        if let Some(i) = self.normals.iter().flatten().position(|p| !p.is_finite()) {
            return Err(MeshLodError::NonFiniteValue {
                attribute: "normals",
                index: i,
            });
        }
        if let Some(i) = self.uvs.iter().flatten().position(|p| !p.is_finite()) {
            return Err(MeshLodError::NonFiniteValue {
                attribute: "uvs",
                index: i,
            });
        }
        if let Some(&index) = self.indices.iter().find(|&&i| i as usize >= n) {
            return Err(MeshLodError::IndexOutOfRange {
                index,
                vertex_count: n,
            });
        }
        Ok(())
    }

    /// Axis-aligned bounds of all positions, or `None` when empty.
    pub fn bounds(&self) -> Option<Aabb> {
        Aabb::from_points(&self.positions)
    }
}
