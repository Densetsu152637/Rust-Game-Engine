//! Error type shared by every fallible entry point in the crate.

use std::fmt;

/// Reasons an input mesh or option set is rejected at the API boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum MeshLodError {
    /// The mesh has no vertices or no triangles.
    EmptyMesh,
    /// The index list length is not a multiple of three.
    IndexCountNotMultipleOfThree { len: usize },
    /// An index refers past the end of the vertex array.
    IndexOutOfRange { index: u32, vertex_count: usize },
    /// A normal or UV array does not match the position count.
    AttributeLengthMismatch {
        attribute: &'static str,
        expected: usize,
        actual: usize,
    },
    /// A position, normal, UV or matrix element is NaN or infinite.
    NonFiniteValue {
        attribute: &'static str,
        index: usize,
    },
    /// An option value is out of range or contradictory.
    InvalidOption { reason: String },
    /// A merged mesh would need more vertices than a `u32` index can address.
    TooManyVertices,
}

impl fmt::Display for MeshLodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyMesh => write!(f, "mesh has no vertices or no triangles"),
            Self::IndexCountNotMultipleOfThree { len } => {
                write!(f, "index count {len} is not a multiple of three")
            }
            Self::IndexOutOfRange {
                index,
                vertex_count,
            } => write!(f, "index {index} out of range for {vertex_count} vertices"),
            Self::AttributeLengthMismatch {
                attribute,
                expected,
                actual,
            } => write!(
                f,
                "{attribute} length {actual} does not match vertex count {expected}"
            ),
            Self::NonFiniteValue { attribute, index } => {
                write!(f, "non-finite value in {attribute} at index {index}")
            }
            Self::InvalidOption { reason } => write!(f, "invalid option: {reason}"),
            Self::TooManyVertices => write!(f, "merged mesh exceeds u32 index range"),
        }
    }
}

impl std::error::Error for MeshLodError {}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, MeshLodError>;
