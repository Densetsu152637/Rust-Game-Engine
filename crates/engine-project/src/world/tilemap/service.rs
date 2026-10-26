//! Runtime service: loads maps, keeps per-chunk render entities and generated colliders in step
//! with the map, and exposes the native painting and query API.
use super::assets::{self, mapped};
use super::component::{TileChunkRender, TilemapComponent, TilemapGenerated};
use crate::content::{Mesh, Vertex};
use crate::runtime::{AuthoredId, ProjectResource, RuntimeLog};
use crate::{Collider2D, Error, GeometryOverride, MeshRenderer, Parent, Result, Transform};
use bevy_ecs::prelude::*;
use engine_tilemap::{
    CellPos, CellRect, ChunkKey, PaintReport, PropertyValue, TileId, Tilemap, TilemapDocument,
    Tileset, collision, geometry,
};
use glam::Mat4;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const MAX_TILEMAPS: usize = 64;
const MAX_TILESET_CACHE: usize = 16;
/// Generated colliders per tilemap; the 2D solver holds 1024 bodies in total.
pub const MAX_COLLIDERS: usize = 1024;

fn invalid(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

/// A tileset plus the pixel size of its atlas texture.
pub struct LoadedTileset {
    /// The validated tileset.
    pub tileset: Tileset,
    /// Atlas size in pixels.
    pub texture_size: [u32; 2],
}

#[derive(Clone, PartialEq)]
struct Source {
    tileset: String,
    map: Option<String>,
    inline: Option<Value>,
}
#[derive(Clone, PartialEq)]
struct VisualSig {
    revision: u64,
    opacity: u32,
    z_order: i32,
    cell: [u32; 2],
    tint: [u32; 4],
    base: i32,
}
#[derive(Clone, PartialEq)]
struct CollisionSig {
    chunks: Vec<(String, ChunkKey, u64)>,
    cell: [u32; 2],
    layer: u32,
    mask: u32,
    trigger: bool,
}
struct State {
    source: Source,
    loaded: Arc<LoadedTileset>,
    map: Tilemap,
    visuals: BTreeMap<(String, ChunkKey), (Entity, VisualSig)>,
    colliders: Vec<Entity>,
    collision_sig: Option<CollisionSig>,
    quads: usize,
}

/// Runtime tilemap state, one entry per `engine.Tilemap` entity.
#[derive(Resource, Default)]
pub struct Tilemaps {
    states: BTreeMap<Entity, State>,
    tilesets: BTreeMap<String, Arc<LoadedTileset>>,
    /// Entities the last sync could not serve and why (capacity, bad asset, no authored id).
    /// They are skipped; the rest of the world keeps ticking.
    failures: BTreeMap<Entity, String>,
}

/// Why `entity` was skipped by the last [`sync_tilemaps`] (more than 64 tilemap entities, an
/// unreadable asset, no registered authored id, ...), if it was.
pub fn failure(world: &World, entity: Entity) -> Option<String> {
    world
        .get_resource::<Tilemaps>()?
        .failures
        .get(&entity)
        .cloned()
}

/// Snapshot of a tilemap's runtime state for telemetry and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TilemapStatus {
    /// Tile layers in the map.
    pub layers: usize,
    /// Non-empty chunks across layers.
    pub chunks: usize,
    /// Render chunk entities currently alive.
    pub render_chunks: usize,
    /// Quads across those entities.
    pub quads: usize,
    /// Generated collider entities.
    pub colliders: usize,
    /// How tiles reach the screen: one `MeshRenderer` + `GeometryOverride` entity per chunk,
    /// drawn through the 2D sprite draw path (`runtime::Draw`, straight alpha, no depth).
    pub render_path: &'static str,
}

/// A tile found at a position.
#[derive(Clone, Debug, PartialEq)]
pub struct TileHit {
    /// Cell coordinate.
    pub cell: CellPos,
    /// Painted tile id (a rule tile id for autotiled cells).
    pub tile: TileId,
    /// Concrete tile id displayed.
    pub shown: TileId,
    /// Orientation flags of the displayed tile.
    pub flags: u8,
    /// Properties of the displayed tile.
    pub properties: BTreeMap<String, PropertyValue>,
}

pub(super) fn install(world: &mut World) {
    world.insert_resource(Tilemaps::default());
}

fn despawn_state(world: &mut World, state: State) {
    for (entity, _) in state.visuals.into_values() {
        world.despawn(entity);
    }
    for entity in state.colliders {
        world.despawn(entity);
    }
}

fn ensure_loaded(world: &mut World, entity: Entity) -> Result<()> {
    let component = world
        .get::<TilemapComponent>(entity)
        .ok_or_else(|| invalid("tilemap.component.missing", "Entity has no engine.Tilemap"))?
        .clone();
    component.validate()?;
    let source = Source {
        tileset: component.tileset.clone(),
        map: component.map.clone(),
        inline: component.inline.clone(),
    };
    if !world.contains_resource::<Tilemaps>() {
        world.insert_resource(Tilemaps::default());
    }
    if world
        .resource::<Tilemaps>()
        .states
        .get(&entity)
        .is_some_and(|s| s.source == source)
    {
        return Ok(());
    }
    if world.resource::<Tilemaps>().states.len() >= MAX_TILEMAPS
        && !world.resource::<Tilemaps>().states.contains_key(&entity)
    {
        return Err(invalid("tilemap.capacity", "At most 64 tilemap entities"));
    }
    let project = world
        .get_resource::<ProjectResource>()
        .ok_or_else(|| invalid("runtime.project.unavailable", "No project resource"))?
        .0
        .clone();
    let cached = world
        .resource::<Tilemaps>()
        .tilesets
        .get(&component.tileset)
        .cloned();
    let loaded = match cached {
        Some(loaded) => loaded,
        None => {
            let (tileset, texture_size) = assets::load_tileset(&project, &component.tileset)?;
            let loaded = Arc::new(LoadedTileset {
                tileset,
                texture_size,
            });
            let service = &mut world.resource_mut::<Tilemaps>();
            if service.tilesets.len() >= MAX_TILESET_CACHE {
                service.tilesets.clear();
            }
            service
                .tilesets
                .insert(component.tileset.clone(), loaded.clone());
            loaded
        }
    };
    let map = if let Some(id) = &component.map {
        assets::load_tilemap(&project, id, &loaded.tileset)?
    } else if let Some(value) = &component.inline {
        let document: TilemapDocument = serde_json::from_value(value.clone())?;
        Tilemap::from_document(&document, Some(&loaded.tileset)).map_err(mapped)?
    } else {
        Tilemap::new(CellRect::default()).map_err(mapped)?
    };
    let previous = world.resource_mut::<Tilemaps>().states.insert(
        entity,
        State {
            source,
            loaded,
            map,
            visuals: BTreeMap::new(),
            colliders: Vec::new(),
            collision_sig: None,
            quads: 0,
        },
    );
    if let Some(previous) = previous {
        despawn_state(world, previous);
    }
    Ok(())
}

fn visual_mesh(geometry: &geometry::ChunkGeometry, opacity: f32, tint: [f32; 4]) -> Mesh {
    let color = [tint[0], tint[1], tint[2], tint[3] * opacity];
    Mesh {
        normals: Vec::new(),
        vertices: geometry
            .positions
            .iter()
            .zip(&geometry.uvs)
            .map(|(p, uv)| Vertex {
                position: [p[0], p[1], 0.0],
                uv: *uv,
                color,
            })
            .collect(),
        indices: geometry.indices.clone(),
    }
}

fn sync_visuals(
    world: &mut World,
    entity: Entity,
    owner_id: &str,
    component: &TilemapComponent,
    state: &mut State,
) -> Result<()> {
    let texture = state.loaded.tileset.texture().to_owned();
    let State {
        map,
        visuals,
        loaded,
        quads,
        ..
    } = state;
    let mut wanted = BTreeSet::new();
    for layer in map.layers() {
        if !component.visible || !layer.visible || layer.opacity <= 0.0 {
            continue;
        }
        for key in layer.chunk_keys() {
            let id = (layer.name.clone(), key);
            let signature = VisualSig {
                revision: layer.chunk_revision(key).unwrap_or(0),
                opacity: layer.opacity.to_bits(),
                z_order: layer.z_order,
                cell: component.cell_size.map(f32::to_bits),
                tint: component.tint.map(f32::to_bits),
                base: component.render_layer,
            };
            if visuals.get(&id).is_some_and(|(_, s)| *s == signature) {
                wanted.insert(id);
                continue;
            }
            let data = geometry::chunk_geometry(
                layer,
                key,
                &loaded.tileset,
                component.cell_size,
                loaded.texture_size,
            )
            .map_err(mapped)?;
            if data.quads() == 0 {
                continue;
            }
            let mesh = Arc::new(visual_mesh(&data, layer.opacity, component.tint));
            let marker = TileChunkRender {
                owner: entity,
                layer: layer.name.clone(),
                chunk: key,
                draw_layer: component.render_layer.saturating_add(layer.z_order),
            };
            if let Some((existing, old)) = visuals.get_mut(&id) {
                world
                    .entity_mut(*existing)
                    .insert((GeometryOverride(mesh), marker));
                *old = signature;
            } else {
                let name = format!("tilemap:{owner_id}/{}/{},{}", layer.name, key[0], key[1]);
                let spawned = world
                    .spawn((
                        AuthoredId {
                            id: name.clone(),
                            name,
                        },
                        Transform::default(),
                        Parent(owner_id.to_owned()),
                        MeshRenderer {
                            version: 1,
                            mesh: String::new(),
                            texture: Some(texture.clone()),
                        },
                        GeometryOverride(mesh),
                        marker,
                        TilemapGenerated { owner: entity },
                    ))
                    .id();
                visuals.insert(id.clone(), (spawned, signature));
            }
            wanted.insert(id);
        }
    }
    let gone: Vec<_> = visuals
        .keys()
        .filter(|id| !wanted.contains(*id))
        .cloned()
        .collect();
    for id in gone {
        if let Some((spawned, _)) = visuals.remove(&id) {
            world.despawn(spawned);
        }
    }
    *quads = visuals
        .values()
        .filter_map(|(e, _)| world.get::<GeometryOverride>(*e))
        .map(|g| g.0.indices.len() / 6)
        .sum();
    Ok(())
}

fn sync_collision(
    world: &mut World,
    entity: Entity,
    owner_id: &str,
    component: &TilemapComponent,
    state: &mut State,
) -> Result<()> {
    let settings = &component.collision;
    let selected: Vec<&engine_tilemap::TileLayer> = state
        .map
        .layers()
        .iter()
        .filter(|l| settings.layers.is_empty() || settings.layers.contains(&l.name))
        .collect();
    let signature = settings.enabled.then(|| CollisionSig {
        chunks: selected
            .iter()
            .flat_map(|l| {
                l.chunk_keys()
                    .into_iter()
                    .map(|k| (l.name.clone(), k, l.chunk_revision(k).unwrap_or(0)))
            })
            .collect(),
        cell: component.cell_size.map(f32::to_bits),
        layer: settings.layer,
        mask: settings.mask,
        trigger: settings.trigger,
    });
    if signature == state.collision_sig && (signature.is_some() || state.colliders.is_empty()) {
        return Ok(());
    }
    let mut boxes = Vec::new();
    if settings.enabled {
        for layer in &selected {
            boxes.extend(collision::collision_boxes(layer, &state.loaded.tileset));
        }
        if boxes.len() > MAX_COLLIDERS {
            return Err(invalid(
                "tilemap.collision.capacity",
                format!(
                    "{} collision boxes exceed the {MAX_COLLIDERS} the 2D solver holds; \
                     use fewer solid regions or disable collision",
                    boxes.len()
                ),
            ));
        }
    }
    for old in state.colliders.drain(..) {
        world.despawn(old);
    }
    for (index, aabb) in boxes.iter().enumerate() {
        let size = aabb.size();
        let name = format!("tilemap:{owner_id}/collider/{index}");
        let spawned = world
            .spawn((
                AuthoredId {
                    id: name.clone(),
                    name,
                },
                Transform::default(),
                Parent(owner_id.to_owned()),
                Collider2D {
                    version: 1,
                    size: [
                        size[0] * component.cell_size[0],
                        size[1] * component.cell_size[1],
                    ],
                    offset: [
                        (aabb.min[0] + aabb.max[0]) / 2.0 * component.cell_size[0],
                        (aabb.min[1] + aabb.max[1]) / 2.0 * component.cell_size[1],
                    ],
                    trigger: settings.trigger,
                    layer: settings.layer,
                    mask: settings.mask,
                },
                TilemapGenerated { owner: entity },
            ))
            .id();
        state.colliders.push(spawned);
    }
    state.collision_sig = signature;
    Ok(())
}

fn sync_entity(world: &mut World, entity: Entity) -> Result<()> {
    ensure_loaded(world, entity)?;
    let component = world
        .get::<TilemapComponent>(entity)
        .expect("checked by ensure_loaded")
        .clone();
    let owner_id = world
        .get::<AuthoredId>(entity)
        .map(|a| a.id.clone())
        .filter(|id| crate::services::find_entity(world, id) == Some(entity))
        .ok_or_else(|| {
            invalid(
                "tilemap.owner",
                "engine.Tilemap entities need an authored ID registered in the entity lookup",
            )
        })?;
    let mut state = world
        .resource_mut::<Tilemaps>()
        .states
        .remove(&entity)
        .expect("loaded");
    let result = sync_visuals(world, entity, &owner_id, &component, &mut state)
        .and_then(|_| sync_collision(world, entity, &owner_id, &component, &mut state));
    world
        .resource_mut::<Tilemaps>()
        .states
        .insert(entity, state);
    result
}

/// Brings render chunks and generated colliders up to date for every `engine.Tilemap` entity.
/// Runs from `world::pre_simulation` each tick; native code may call it after painting to see
/// the effect immediately. Generated colliders enter the 2D solver at the next collision sync.
pub fn sync_tilemaps(world: &mut World) -> Result<()> {
    if !world.contains_resource::<Tilemaps>() {
        return Ok(());
    }
    let mut named: Vec<(String, Entity)> = world
        .iter_entities()
        .filter(|e| e.contains::<TilemapComponent>())
        .map(|e| {
            (
                e.get::<AuthoredId>()
                    .map(|a| a.id.clone())
                    .unwrap_or_default(),
                e.id(),
            )
        })
        .collect();
    named.sort();
    let owners: Vec<Entity> = named.into_iter().map(|(_, e)| e).collect();
    let stale: Vec<Entity> = world
        .resource::<Tilemaps>()
        .states
        .keys()
        .filter(|e| !owners.contains(e))
        .copied()
        .collect();
    for entity in stale {
        if let Some(state) = world.resource_mut::<Tilemaps>().states.remove(&entity) {
            despawn_state(world, state);
        }
    }
    let orphans: Vec<Entity> = {
        let service = world.resource::<Tilemaps>();
        world
            .iter_entities()
            .filter(|e| {
                e.get::<TilemapGenerated>()
                    .is_some_and(|g| !service.states.contains_key(&g.owner))
            })
            .map(|e| e.id())
            .collect()
    };
    for entity in orphans {
        world.despawn(entity);
    }
    let mut failures = BTreeMap::new();
    for entity in owners {
        if let Err(error) = sync_entity(world, entity) {
            failures.insert(entity, error.to_string());
        }
    }
    let fresh: Vec<String> = failures
        .iter()
        .filter(|(entity, message)| {
            world.resource::<Tilemaps>().failures.get(*entity) != Some(*message)
        })
        .map(|(entity, message)| format!("tilemap entity:{}: {message}", entity.to_bits()))
        .collect();
    if let Some(mut log) = world.get_resource_mut::<RuntimeLog>() {
        log.0.extend(fresh);
    }
    world.resource_mut::<Tilemaps>().failures = failures;
    Ok(())
}

/// Runs `edit` on the entity's live map (loading it first if needed). Changes appear in the
/// render chunks and colliders at the next [`sync_tilemaps`] / tick. Runtime edits do not write
/// back to assets; see [`export_document`].
pub fn with_tilemap<R>(
    world: &mut World,
    entity: Entity,
    edit: impl FnOnce(&mut Tilemap, &Tileset) -> R,
) -> Result<R> {
    ensure_loaded(world, entity)?;
    let mut service = world.resource_mut::<Tilemaps>();
    let state = service.states.get_mut(&entity).expect("loaded");
    let tileset = state.loaded.clone();
    Ok(edit(&mut state.map, &tileset.tileset))
}

fn on_layer<R>(
    world: &mut World,
    entity: Entity,
    layer: &str,
    edit: impl FnOnce(&mut engine_tilemap::TileLayer, &Tileset) -> engine_tilemap::Result<R>,
) -> Result<R> {
    with_tilemap(world, entity, |map, tileset| {
        let target = map.layer_mut(layer).ok_or_else(|| {
            engine_tilemap::Error::new("tilemap.layer", format!("Unknown layer {layer}"))
        })?;
        edit(target, tileset)
    })?
    .map_err(mapped)
}

/// Paints one cell (autotiling rule tiles and their neighbours).
pub fn paint(
    world: &mut World,
    entity: Entity,
    layer: &str,
    cell: CellPos,
    tile: TileId,
    flags: u8,
) -> Result<PaintReport> {
    on_layer(world, entity, layer, |l, ts| {
        l.set(cell, tile, flags, Some(ts))
    })
}
/// Erases one cell.
pub fn erase(world: &mut World, entity: Entity, layer: &str, cell: CellPos) -> Result<PaintReport> {
    paint(world, entity, layer, cell, 0, 0)
}
/// Fills a half-open cell rectangle (`tile` 0 clears it).
pub fn fill_rect(
    world: &mut World,
    entity: Entity,
    layer: &str,
    rect: CellRect,
    tile: TileId,
    flags: u8,
) -> Result<PaintReport> {
    on_layer(world, entity, layer, |l, ts| {
        l.fill_rect(rect, tile, flags, Some(ts))
    })
}
/// Flood-fills the region around `seed`, optionally restricted to `limit`.
pub fn flood_fill(
    world: &mut World,
    entity: Entity,
    layer: &str,
    seed: CellPos,
    tile: TileId,
    flags: u8,
    limit: Option<CellRect>,
) -> Result<PaintReport> {
    on_layer(world, entity, layer, |l, ts| {
        l.flood_fill(seed, tile, flags, limit, Some(ts))
    })
}

fn local_inverse(world: &World, entity: Entity) -> Result<Mat4> {
    let matrix = crate::world_transform(world, entity)?.matrix;
    let matrix = Mat4::from_cols_array_2d(&matrix);
    if matrix.determinant().abs() < 1e-12 {
        return Err(invalid("tilemap.transform", "Singular tilemap transform"));
    }
    Ok(matrix.inverse())
}

/// The cell containing the world-space `position` (z is projected onto the tilemap plane).
pub fn world_to_cell(world: &mut World, entity: Entity, position: [f32; 3]) -> Result<CellPos> {
    ensure_loaded(world, entity)?;
    let cell_size = world
        .get::<TilemapComponent>(entity)
        .expect("loaded")
        .cell_size;
    let local = local_inverse(world, entity)?.transform_point3(position.into());
    Ok(geometry::position_to_cell([local.x, local.y], cell_size))
}

/// World-space centre of `cell`.
pub fn cell_to_world(world: &mut World, entity: Entity, cell: CellPos) -> Result<[f32; 3]> {
    ensure_loaded(world, entity)?;
    let cell_size = world
        .get::<TilemapComponent>(entity)
        .expect("loaded")
        .cell_size;
    let origin = geometry::cell_origin(cell, cell_size);
    let matrix = Mat4::from_cols_array_2d(&crate::world_transform(world, entity)?.matrix);
    Ok(matrix
        .transform_point3(
            [
                origin[0] + cell_size[0] / 2.0,
                origin[1] + cell_size[1] / 2.0,
                0.0,
            ]
            .into(),
        )
        .to_array())
}

/// The tile of `layer` under the world-space `position`, or `None` for an empty cell.
pub fn tile_at_world(
    world: &mut World,
    entity: Entity,
    layer: &str,
    position: [f32; 3],
) -> Result<Option<TileHit>> {
    let cell = world_to_cell(world, entity, position)?;
    with_tilemap(world, entity, |map, tileset| {
        let layer = map
            .layer(layer)
            .ok_or_else(|| invalid("tilemap.layer", format!("Unknown layer {layer}")))?;
        let data = layer.get(cell);
        if data.tile == 0 {
            return Ok(None);
        }
        Ok(Some(TileHit {
            cell,
            tile: data.tile,
            shown: data.shown,
            flags: data.shown_flags,
            properties: tileset
                .concrete(data.shown)
                .map(|t| t.properties.clone())
                .unwrap_or_default(),
        }))
    })?
}

/// Forces the generated colliders to be rebuilt immediately (they otherwise rebuild when the
/// map changes).
pub fn regenerate_collision(world: &mut World, entity: Entity) -> Result<()> {
    ensure_loaded(world, entity)?;
    if let Some(state) = world.resource_mut::<Tilemaps>().states.get_mut(&entity) {
        state.collision_sig = None;
    }
    sync_entity(world, entity)
}

/// Serializable copy of the live map (including runtime edits).
pub fn export_document(world: &mut World, entity: Entity) -> Result<TilemapDocument> {
    with_tilemap(world, entity, |map, _| map.to_document())
}

/// Stores the live map into the component's `inline` field (and clears `map`), so scene
/// snapshots carry runtime edits. Fails when the document exceeds the 64 KiB component limit.
pub fn store_inline(world: &mut World, entity: Entity) -> Result<()> {
    let document = serde_json::to_value(export_document(world, entity)?)?;
    if serde_json::to_vec(&document)?.len() > 60 * 1024 {
        return Err(invalid(
            "tilemap.inline.size",
            "Map exceeds the inline component limit; save it as a tilemap asset instead",
        ));
    }
    let source = {
        let mut component = world
            .get_mut::<TilemapComponent>(entity)
            .ok_or_else(|| invalid("tilemap.component.missing", "Entity has no engine.Tilemap"))?;
        component.map = None;
        component.inline = Some(document);
        Source {
            tileset: component.tileset.clone(),
            map: None,
            inline: component.inline.clone(),
        }
    };
    // The live map already equals the new source; adopt it so the next sync keeps the state.
    if let Some(state) = world.resource_mut::<Tilemaps>().states.get_mut(&entity) {
        state.source = source;
    }
    Ok(())
}

/// Counts for telemetry and tests; `None` before the entity's first sync.
pub fn status(world: &World, entity: Entity) -> Option<TilemapStatus> {
    let state = world.get_resource::<Tilemaps>()?.states.get(&entity)?;
    Some(TilemapStatus {
        layers: state.map.layers().len(),
        chunks: state
            .map
            .layers()
            .iter()
            .map(|l| l.chunk_keys().len())
            .sum(),
        render_chunks: state.visuals.len(),
        quads: state.quads,
        colliders: state.colliders.len(),
        render_path: "sprite-draw-mesh-per-chunk",
    })
}
