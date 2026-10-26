//! Terrain as navigation-mesh input: the walkable raster-aligned soup of
//! `engine_terrain::navigation`, packaged as a validated `navigation` document.
use super::assets::mapped;
use crate::Result;
use crate::simulation::{BakeInput, BakeSettings, NavMeshDocument, Obstacle};
use engine_terrain::{Heightfield, NavInputOptions, navigation_soup};

/// Bake input for `field` placed at `options.origin`; the soup's raster uses
/// `settings.cell_size` (the `cell_size` in `options` is ignored). Holes and cells steeper than
/// `settings.max_slope_degrees` are excluded.
pub fn navigation_input(
    field: &Heightfield,
    options: &NavInputOptions,
    settings: BakeSettings,
    obstacles: Vec<Obstacle>,
) -> Result<BakeInput> {
    let options = NavInputOptions {
        cell_size: settings.cell_size,
        max_slope_degrees: settings.max_slope_degrees,
        ..*options
    };
    let soup = navigation_soup(field, &options).map_err(mapped)?;
    if soup.cells == 0 {
        return Err(crate::Error::new(
            "terrain.navigation.empty",
            "no walkable terrain cells in the region",
        ));
    }
    Ok(BakeInput {
        vertices: soup.vertices,
        triangles: soup.triangles,
        obstacles,
        settings,
    })
}

/// A validated (baked once) navigation document for `field`.
pub fn navigation_document(
    field: &Heightfield,
    options: &NavInputOptions,
    settings: BakeSettings,
    obstacles: Vec<Obstacle>,
) -> Result<NavMeshDocument> {
    let document = NavMeshDocument {
        version: 1,
        input: navigation_input(field, options, settings, obstacles)?,
    };
    document.bake()?;
    Ok(document)
}
