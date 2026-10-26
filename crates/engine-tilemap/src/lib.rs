#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod collision;
mod error;
pub mod geometry;
mod map;
pub mod orient;
mod rules;
mod tileset;

pub use error::{Error, Result};
pub use map::{
    CHUNK_SIZE, Cell, CellPos, CellRect, ChunkDocument, ChunkKey, LayerDocument, MAX_BOUNDS_EDGE,
    MAX_CHUNKS_PER_LAYER, MAX_EDIT_CELLS, MAX_LAYERS, PaintReport, TILEMAP_VERSION, TileLayer,
    Tilemap, TilemapDocument, chunk_of,
};
pub use rules::{Neighbour, OFFSETS, Rule, RuleTile, RuleTransform, blob_reduce, blob47_masks};
pub use tileset::{
    CollisionKind, MAX_RULE_TILES, MAX_RULES, MAX_TILE_ID, MAX_TILES, PropertyValue,
    TILESET_VERSION, TileDef, TileId, Tileset, TilesetDocument,
};
