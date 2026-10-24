//! Bounded glTF ingestion and typed runtime asset ownership.
//! Project manifests remain the authority for stable authored asset IDs.
mod import;
mod model;
mod store;

pub use import::{IMPORTER_VERSION, ImportLimits, import_gltf};
pub use model::*;
pub use store::{AssetHandle, AssetStore, LoadedAsset};

/// Errors from the asset trust boundary and runtime store.
#[derive(Debug)]
pub enum AssetError {
    Io(std::io::Error),
    Invalid(String),
    Limit(&'static str),
    OutsideProject,
    Unsupported(String),
    InvalidHandle,
}
impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "asset I/O: {e}"),
            Self::Invalid(e) => write!(f, "invalid asset: {e}"),
            Self::Limit(e) => write!(f, "asset limit exceeded: {e}"),
            Self::OutsideProject => f.write_str("asset path is outside the project root"),
            Self::Unsupported(e) => write!(f, "unsupported asset feature: {e}"),
            Self::InvalidHandle => f.write_str("stale or unknown asset handle"),
        }
    }
}
impl std::error::Error for AssetError {}
impl From<std::io::Error> for AssetError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
pub type Result<T> = std::result::Result<T, AssetError>;
