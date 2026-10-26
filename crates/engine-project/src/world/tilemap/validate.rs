//! Scene validation for `engine.Tilemap`: component shape, asset references with the right
//! kinds and the inline map document.
use super::assets::{self, mapped};
use super::component::TilemapComponent;
use crate::{Error, Project, Result, Scene};
use engine_tilemap::{Tilemap, TilemapDocument};

const COMPONENT_NAME: &str = "engine.Tilemap";

/// Validates every `engine.Tilemap` of `scene`.
///
/// Checks the component, that `tileset` is a loadable `tileset` asset, that `map` is a
/// `tilemap` asset consistent with the tileset and that an inline document parses against it.
/// Failures carry the entity id.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    for entity in &scene.entities {
        let Some(value) = entity.components.custom.get(COMPONENT_NAME) else {
            continue;
        };
        let at = |e: Error| e.with_context(&format!("entity {}", entity.id));
        let component: TilemapComponent =
            serde_json::from_value(value.data.clone()).map_err(|e| {
                Error::new(
                    "tilemap.component.invalid",
                    format!("entity {}: {e}", entity.id),
                )
            })?;
        component.validate().map_err(at)?;
        let (tileset, _) = assets::load_tileset(project, &component.tileset).map_err(at)?;
        if let Some(map) = &component.map {
            assets::load_tilemap(project, map, &tileset).map_err(at)?;
        } else if let Some(inline) = &component.inline {
            let document: TilemapDocument =
                serde_json::from_value(inline.clone()).map_err(|e| {
                    Error::new(
                        "tilemap.inline.invalid",
                        format!("entity {}: {e}", entity.id),
                    )
                })?;
            Tilemap::from_document(&document, Some(&tileset))
                .map_err(mapped)
                .map_err(at)?;
        }
    }
    Ok(())
}
