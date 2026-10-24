//! Shared navigation asset service for native callers, runtime agents and MCP.
use super::*;
pub use engine_navigation::{
    BakeInput, BakeSettings, NavLocation, NavMesh, NavPath, Obstacle, QuerySettings,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Persist source geometry/settings, never trusted adjacency. Loading revalidates and bakes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NavMeshDocument {
    pub version: u32,
    pub input: BakeInput,
}
impl NavMeshDocument {
    pub fn bake(&self) -> Result<Arc<NavMesh>> {
        if self.version != 1 {
            return Err(invalid("Unsupported navigation document version"));
        }
        engine_navigation::bake(&self.input)
            .map(Arc::new)
            .map_err(mapped)
    }
}
/// Mesh assets are explicit world-space floors; callers bake transforms into positions.
pub fn navigation_from_meshes(
    project: &crate::Project,
    assets: &[String],
    settings: BakeSettings,
    obstacles: Vec<Obstacle>,
) -> Result<NavMeshDocument> {
    if assets.is_empty() || assets.len() > 128 {
        return Err(invalid("Navigation requires 1-128 mesh assets"));
    }
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    for id in assets {
        project.require_asset(id, "mesh")?;
        let mesh = crate::content::load_mesh(&project.resolve_asset(id)?)?;
        if vertices.len() + mesh.vertices.len() > 65_536
            || triangles.len() + mesh.indices.len() / 3 > 32_768
        {
            return Err(invalid("Navigation source geometry capacity exceeded"));
        }
        let offset = vertices.len() as u32;
        vertices.extend(mesh.vertices.iter().map(|v| v.position));
        triangles.extend(
            mesh.indices
                .as_chunks::<3>()
                .0
                .iter()
                .map(|t| [t[0] + offset, t[1] + offset, t[2] + offset]),
        );
    }
    let document = NavMeshDocument {
        version: 1,
        input: BakeInput {
            vertices,
            triangles,
            obstacles,
            settings,
        },
    };
    document.bake()?;
    Ok(document)
}
pub fn load_navmesh(project: &crate::Project, id: &str) -> Result<Arc<NavMesh>> {
    project.require_asset(id, "navigation")?;
    load_navmesh_file(
        &project.root,
        &project.require_asset(id, "navigation")?.path,
    )
}
pub fn load_navmesh_file(root: &std::path::Path, path: &std::path::Path) -> Result<Arc<NavMesh>> {
    let path = crate::confined_file(root, path)?;
    let document: NavMeshDocument =
        serde_json::from_slice(&crate::schema::read_bounded(&path, 16 * 1024 * 1024)?)?;
    document.bake()
}
