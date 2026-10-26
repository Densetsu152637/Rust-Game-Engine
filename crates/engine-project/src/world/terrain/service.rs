//! Runtime terrain service: loads terrain assets, keeps collider tiles, PCG surfaces, CPU
//! fallback chunk entities and the GPU render set in step with the heightfield, and exposes
//! the native editing and query API.
use super::assets::{load_terrain, mapped};
use super::component::{
    TerrainChunk, TerrainColliderTile, TerrainCollision, TerrainComponent, TerrainGenerated,
    TerrainMaterial, TerrainRenderMode,
};
use super::surface::TerrainSurface;
use crate::content::{Mesh, Texture, Vertex};
use crate::rendering3d::Camera3D;
use crate::runtime::{AuthoredId, ProjectResource, RuntimeLog};
use crate::simulation::physics::{BodyMaterial, Motion, PhysicsBody3D, Shape};
use crate::world::pcg::{PcgSurfaceRegistry, PcgVolume};
use crate::{Error, GeometryOverride, MeshRenderer, Parent, Result, Transform};
use bevy_ecs::prelude::*;
use engine_terrain::collision::{self, TileShape};
use engine_terrain::lod::NO_MORPH;
use engine_terrain::{
    Brush, EditReport, Heightfield, LodLayout, LodSettings, LodViewer, NodeKey, SampleRect,
    SculptOp, SelectedNode, build_chunk_mesh,
};
use glam::{Mat4, Vec2, Vec3};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Most `engine.Terrain` entities in one world.
pub const MAX_TERRAINS: usize = 16;
/// Most collider tiles per terrain.
pub const MAX_COLLIDER_TILES: usize = 1024;

fn invalid(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

/// Marker resource: a GPU terrain renderer (the `world.terrain` render feature of
/// `engine-world-render`) is installed. `render_mode: auto` terrains then skip the CPU chunk
/// entities and publish themselves in [`TerrainRenderSet`] instead.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerrainGpuRenderer;

/// One splat layer as drawn: asset definition plus the component's overrides.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedLayer {
    pub name: String,
    pub tint: [f32; 4],
    pub uv_tiling: f32,
    pub texture: Option<String>,
    /// Decoded texture (GPU path only; `None` until needed or when the layer has none).
    pub image: Option<Arc<Texture>>,
}

/// A terrain the GPU feature draws this frame.
#[derive(Clone, Debug)]
pub struct TerrainRenderItem {
    pub owner: Entity,
    /// Authored id of the terrain entity.
    pub id: String,
    pub field: Arc<Heightfield>,
    pub lod: LodSettings,
    pub layers: Vec<ResolvedLayer>,
    pub world_from_local: [[f32; 4]; 4],
}

/// Terrains on the GPU path, rebuilt after transforms each tick. The render extraction copies
/// it into the render world; [`TerrainRenderItem::field`] is shared, not copied.
#[derive(Resource, Clone, Debug, Default)]
pub struct TerrainRenderSet {
    pub items: Vec<TerrainRenderItem>,
}

/// Snapshot of one terrain's runtime state.
#[derive(Clone, Debug, PartialEq)]
pub struct TerrainStatus {
    /// Samples `[columns, rows]`.
    pub samples: [u32; 2],
    /// Heightfield revision.
    pub revision: u64,
    /// Effective LOD levels.
    pub levels: u32,
    /// `cpu-mesh-chunks`, `gpu-feature` or `hidden`.
    pub render_path: &'static str,
    /// CPU path: selected nodes and per-level counts (finest first).
    pub nodes: usize,
    pub level_histogram: Vec<usize>,
    pub budget_limited: bool,
    /// CPU path: chunk entities, vertices and triangles.
    pub chunk_entities: usize,
    pub vertices: usize,
    pub triangles: usize,
    /// Collider tiles (heightfields), and how many of them have removed hole cells.
    pub colliders: usize,
    pub holed_colliders: usize,
    /// Registered `PcgSurfaceRegistry` name.
    pub surface: Option<String>,
    /// Why something did not happen (no GPU renderer for `render_mode: gpu`, surface name
    /// taken, ...).
    pub notes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MorphClass {
    Unmorphed,
    Full,
    Partial,
}
struct Chunk {
    entity: Entity,
    morph: MorphClass,
    vertices: usize,
    triangles: usize,
}
struct Cpu {
    lod: LodSettings,
    layout: LodLayout,
    viewer: Option<Vec3>,
    revision: u64,
    pending: Option<SampleRect>,
    nodes: usize,
    histogram: Vec<usize>,
    budget_limited: bool,
}
/// Registered surface name, heightfield revision, world matrix and the `WorldOrigin.offset` the
/// surface was built for.
type SurfaceBuilt = (String, u64, [[f32; 4]; 4], [f64; 3]);
struct State {
    asset: String,
    field: Arc<Heightfield>,
    materials: Vec<TerrainMaterial>,
    layers: Vec<ResolvedLayer>,
    images_loaded: bool,
    cpu: Option<Cpu>,
    chunks: BTreeMap<NodeKey, Chunk>,
    colliders: BTreeMap<[u32; 2], (Entity, bool)>,
    collision: Option<TerrainCollision>,
    pending_collision: Option<SampleRect>,
    surface: Option<SurfaceBuilt>,
    surface_note: Option<String>,
    render_path: &'static str,
    render_note: Option<String>,
}

/// Runtime terrain state, one entry per `engine.Terrain` entity.
#[derive(Resource, Default)]
pub struct Terrains {
    states: BTreeMap<Entity, State>,
    /// Entities the load and collision/surface sync could not serve, and why (capacity, bad
    /// asset, missing authored id). They are skipped; the rest of the world keeps ticking.
    failures: BTreeMap<Entity, String>,
    /// Entities the render update could not serve, and why.
    render_failures: BTreeMap<Entity, String>,
}

/// Records (and logs once per distinct reason) why `entity` could not be served; a success
/// clears the record.
fn record_failure(world: &mut World, entity: Entity, render: bool, result: Result<()>) {
    let mut terrains = world.resource_mut::<Terrains>();
    let map = if render {
        &mut terrains.render_failures
    } else {
        &mut terrains.failures
    };
    let Err(error) = result else {
        map.remove(&entity);
        return;
    };
    let message = error.to_string();
    if map.get(&entity) == Some(&message) {
        return;
    }
    map.insert(entity, message.clone());
    if let Some(mut log) = world.get_resource_mut::<RuntimeLog>() {
        log.0
            .push(format!("terrain entity:{}: {message}", entity.to_bits()));
    }
}

/// Why `entity` has no terrain state or could not be updated (more than [`MAX_TERRAINS`]
/// terrain entities, an unreadable asset, no registered authored id, ...), if that is the
/// case. Such a terrain is skipped; the rest keep running.
pub fn failure(world: &World, entity: Entity) -> Option<String> {
    let terrains = world.get_resource::<Terrains>()?;
    terrains
        .failures
        .get(&entity)
        .or_else(|| terrains.render_failures.get(&entity))
        .cloned()
}

pub(super) fn install(world: &mut World) {
    world.insert_resource(Terrains::default());
    world.insert_resource(TerrainRenderSet::default());
}

fn resolve_layers(field: &Heightfield, materials: &[TerrainMaterial]) -> Vec<ResolvedLayer> {
    field
        .layers()
        .iter()
        .enumerate()
        .map(|(i, layer)| {
            let m = materials.get(i).cloned().unwrap_or_default();
            ResolvedLayer {
                name: layer.name.clone(),
                tint: m.tint.unwrap_or(layer.tint),
                uv_tiling: m.uv_tiling.unwrap_or(layer.uv_tiling),
                texture: m.texture.or_else(|| layer.texture.clone()),
                image: None,
            }
        })
        .collect()
}

fn despawn_generated(world: &mut World, state: &mut State) {
    for (_, chunk) in std::mem::take(&mut state.chunks) {
        world.despawn(chunk.entity);
    }
    for (_, (entity, _)) in std::mem::take(&mut state.colliders) {
        world.despawn(entity);
    }
    state.collision = None;
}

fn component(world: &World, entity: Entity) -> Result<TerrainComponent> {
    world
        .get::<TerrainComponent>(entity)
        .cloned()
        .ok_or_else(|| invalid("terrain.component.missing", "Entity has no engine.Terrain"))
}

fn ensure_loaded(world: &mut World, entity: Entity) -> Result<()> {
    let component = component(world, entity)?;
    component.validate()?;
    if !world.contains_resource::<Terrains>() {
        install(world);
    }
    if let Some(state) = world.resource_mut::<Terrains>().states.get_mut(&entity)
        && state.asset == component.asset
    {
        if state.materials != component.materials {
            state.layers = resolve_layers(&state.field, &component.materials);
            state.materials = component.materials.clone();
            state.images_loaded = false;
        }
        return Ok(());
    }
    {
        let terrains = world.resource::<Terrains>();
        if terrains.states.len() >= MAX_TERRAINS && !terrains.states.contains_key(&entity) {
            return Err(invalid(
                "terrain.capacity",
                format!("At most {MAX_TERRAINS} terrain entities"),
            ));
        }
    }
    let project = world
        .get_resource::<ProjectResource>()
        .ok_or_else(|| invalid("runtime.project.unavailable", "No project resource"))?
        .0
        .clone();
    let field = load_terrain(&project, &component.asset)?;
    let layers = resolve_layers(&field, &component.materials);
    let state = State {
        asset: component.asset.clone(),
        field: Arc::new(field),
        materials: component.materials.clone(),
        layers,
        images_loaded: false,
        cpu: None,
        chunks: BTreeMap::new(),
        colliders: BTreeMap::new(),
        collision: None,
        pending_collision: None,
        surface: None,
        surface_note: None,
        render_path: "hidden",
        render_note: None,
    };
    let previous = world
        .resource_mut::<Terrains>()
        .states
        .insert(entity, state);
    if let Some(mut previous) = previous {
        despawn_generated(world, &mut previous);
        if let Some((name, ..)) = previous.surface
            && let Some(mut registry) = world.get_resource_mut::<PcgSurfaceRegistry>()
        {
            registry.remove(&name);
        }
    }
    Ok(())
}

fn owner_id(world: &World, entity: Entity) -> Result<String> {
    world
        .get::<AuthoredId>(entity)
        .map(|a| a.id.clone())
        .filter(|id| crate::services::find_entity(world, id) == Some(entity))
        .ok_or_else(|| {
            invalid(
                "terrain.owner",
                "engine.Terrain entities need an authored ID registered in the entity lookup",
            )
        })
}

fn take_state(world: &mut World, entity: Entity) -> State {
    world
        .resource_mut::<Terrains>()
        .states
        .remove(&entity)
        .expect("loaded")
}
fn put_state(world: &mut World, entity: Entity, state: State) {
    world
        .resource_mut::<Terrains>()
        .states
        .insert(entity, state);
}

// ---- collision ---------------------------------------------------------------------------

fn body(settings: &TerrainCollision, tile: &collision::CollisionTile) -> Option<PhysicsBody3D> {
    let material = BodyMaterial {
        friction: settings.friction,
        restitution: settings.restitution,
        layer: settings.layer,
        mask: settings.mask,
        ..BodyMaterial::default()
    };
    let base = PhysicsBody3D {
        motion: Motion::Fixed,
        material,
        ..PhysicsBody3D::default()
    };
    match &tile.shape {
        TileShape::HeightField {
            rows,
            columns,
            heights,
            size,
            holes,
        } => Some(PhysicsBody3D {
            shape: Shape::HeightField,
            heights: heights.clone(),
            heightfield_rows: *rows,
            heightfield_columns: *columns,
            heightfield_scale: [size[0], 1.0, size[1]],
            heightfield_holes: holes.clone(),
            ..base
        }),
        TileShape::Empty => None,
    }
}

fn sync_collision(
    world: &mut World,
    entity: Entity,
    owner: &str,
    component: &TerrainComponent,
    state: &mut State,
) -> Result<()> {
    let settings = &component.collision;
    if !settings.enabled {
        for (_, (collider, _)) in std::mem::take(&mut state.colliders) {
            world.despawn(collider);
        }
        state.collision = Some(settings.clone());
        state.pending_collision = None;
        return Ok(());
    }
    let field = state.field.clone();
    let [nx, nz] = collision::tile_counts(&field, settings.tile_cells).map_err(mapped)?;
    if (nx * nz) as usize > MAX_COLLIDER_TILES {
        return Err(invalid(
            "terrain.collision.capacity",
            format!(
                "{} collider tiles exceed {MAX_COLLIDER_TILES}; raise collision.tile_cells",
                nx * nz
            ),
        ));
    }
    let tiles: Vec<[u32; 2]> = if state.collision.as_ref() != Some(settings) {
        for (_, (collider, _)) in std::mem::take(&mut state.colliders) {
            world.despawn(collider);
        }
        (0..nz).flat_map(|z| (0..nx).map(move |x| [x, z])).collect()
    } else if let Some(region) = state.pending_collision {
        collision::tiles_touching(&field, settings.tile_cells, region).map_err(mapped)?
    } else {
        return Ok(());
    };
    for key in tiles {
        let tile = collision::collision_tile(&field, settings.tile_cells, key).map_err(mapped)?;
        let Some(body) = body(settings, &tile) else {
            if let Some((collider, _)) = state.colliders.remove(&key) {
                world.despawn(collider);
            }
            continue;
        };
        let holed = !body.heightfield_holes.is_empty();
        let transform = Transform {
            position: [tile.center[0], 0.0, tile.center[1]],
            ..Transform::default()
        };
        match state.colliders.get_mut(&key) {
            Some((collider, mesh)) => {
                world.entity_mut(*collider).insert((body, transform));
                *mesh = holed;
            }
            None => {
                let name = format!("terrain:{owner}/collider/{},{}", key[0], key[1]);
                let spawned = world
                    .spawn((
                        AuthoredId {
                            id: name.clone(),
                            name,
                        },
                        transform,
                        Parent(owner.to_owned()),
                        body,
                        TerrainColliderTile {
                            owner: entity,
                            tile: key,
                        },
                        TerrainGenerated { owner: entity },
                    ))
                    .id();
                state.colliders.insert(key, (spawned, holed));
            }
        }
    }
    state.collision = Some(settings.clone());
    state.pending_collision = None;
    Ok(())
}

// ---- PCG surfaces ------------------------------------------------------------------------

fn sync_surface(
    world: &mut World,
    entity: Entity,
    component: &TerrainComponent,
    state: &mut State,
    claimed: &mut BTreeSet<String>,
) -> Result<()> {
    let name = component.surface.clone();
    state.surface_note = None;
    let previous = state.surface.as_ref().map(|(n, ..)| n.clone());
    if name.is_empty() || claimed.contains(&name) {
        if !name.is_empty() {
            state.surface_note = Some(format!(
                "surface name {name} is already registered by another terrain"
            ));
        }
        if let Some(old) = previous {
            world.resource_mut::<PcgSurfaceRegistry>().remove(&old);
        }
        state.surface = None;
        return Ok(());
    }
    claimed.insert(name.clone());
    let matrix = crate::world_transform(world, entity)?.matrix;
    let revision = state.field.revision();
    let origin = crate::world::coordinates::origin(world);
    if state
        .surface
        .as_ref()
        .is_some_and(|(n, r, m, _)| *n == name && *r == revision && *m == matrix)
        && world
            .get_resource::<PcgSurfaceRegistry>()
            .is_some_and(|r| r.get(&name).is_some())
    {
        return Ok(());
    }
    // A pure origin translation moves the terrain matrix by exactly the origin delta: the
    // ground is unchanged in absolute terms and PCG hashes absolute coordinates, so the volumes
    // standing on it need no regeneration.
    let translated_only = state.surface.as_ref().is_some_and(|(n, r, m, o)| {
        *n == name && *r == revision && is_origin_translation(m, &matrix, o, &origin)
    });
    let surface = TerrainSurface::new(state.field.clone(), Mat4::from_cols_array_2d(&matrix))
        .ok_or_else(|| invalid("terrain.transform", "Singular terrain transform"))?;
    if !world.contains_resource::<PcgSurfaceRegistry>() {
        world.init_resource::<PcgSurfaceRegistry>();
    }
    let mut registry = world.resource_mut::<PcgSurfaceRegistry>();
    if let Some(old) = previous.filter(|old| *old != name) {
        registry.remove(&old);
    }
    registry.insert(name.clone(), Arc::new(surface));
    // Surface contents are not tracked by PCG: regenerate the volumes standing on this one.
    if !translated_only {
        let mut volumes = world.query::<&mut PcgVolume>();
        for mut volume in volumes.iter_mut(world) {
            if volume.surface == name {
                volume.regenerate = volume.regenerate.wrapping_add(1);
            }
        }
    }
    state.surface = Some((name, revision, matrix, origin));
    Ok(())
}

/// Whether `new` is `old` moved by exactly the origin shift `old_origin -> new_origin` (same
/// linear part; translation changed by `old_origin - new_origin`).
fn is_origin_translation(
    old: &[[f32; 4]; 4],
    new: &[[f32; 4]; 4],
    old_origin: &[f64; 3],
    new_origin: &[f64; 3],
) -> bool {
    if old_origin == new_origin {
        return false;
    }
    let linear = (0..3).all(|c| (0..4).all(|r| old[c][r] == new[c][r]));
    linear
        && (0..3).all(|axis| {
            let expected = f64::from(old[3][axis]) + old_origin[axis] - new_origin[axis];
            let tolerance = 1e-5 * expected.abs().max(1.0) + 1e-3;
            (f64::from(new[3][axis]) - expected).abs() <= tolerance
        })
}

// ---- tick hooks --------------------------------------------------------------------------

fn sorted_owners(world: &mut World) -> Vec<Entity> {
    let mut owners: Vec<(String, Entity)> = world
        .iter_entities()
        .filter(|e| e.contains::<TerrainComponent>())
        .map(|e| {
            (
                e.get::<AuthoredId>()
                    .map(|a| a.id.clone())
                    .unwrap_or_default(),
                e.id(),
            )
        })
        .collect();
    owners.sort();
    owners.into_iter().map(|(_, e)| e).collect()
}

fn remove_stale(world: &mut World, owners: &[Entity]) {
    let stale: Vec<Entity> = world
        .resource::<Terrains>()
        .states
        .keys()
        .filter(|e| !owners.contains(e))
        .copied()
        .collect();
    for entity in stale {
        if let Some(mut state) = world.resource_mut::<Terrains>().states.remove(&entity) {
            despawn_generated(world, &mut state);
            if let Some((name, ..)) = state.surface
                && let Some(mut registry) = world.get_resource_mut::<PcgSurfaceRegistry>()
            {
                registry.remove(&name);
            }
        }
    }
    let orphans: Vec<Entity> = {
        let terrains = world.resource::<Terrains>();
        world
            .iter_entities()
            .filter(|e| {
                e.get::<TerrainGenerated>()
                    .is_some_and(|g| !terrains.states.contains_key(&g.owner))
            })
            .map(|e| e.id())
            .collect()
    };
    for entity in orphans {
        world.despawn(entity);
    }
}

/// Loads one terrain and syncs its collider tiles and PCG surface.
fn sync_one(world: &mut World, entity: Entity, claimed: &mut BTreeSet<String>) -> Result<()> {
    ensure_loaded(world, entity)?;
    let component = component(world, entity)?;
    let owner = owner_id(world, entity)?;
    let mut state = take_state(world, entity);
    let result = sync_collision(world, entity, &owner, &component, &mut state)
        .and_then(|_| sync_surface(world, entity, &component, &mut state, claimed));
    put_state(world, entity, state);
    result
}

/// Loads terrains, rebuilds dirty collider tiles and refreshes PCG surfaces. Runs from
/// `world::pre_simulation` so edits made by scripts this tick reach physics in the same step.
/// A terrain that cannot be served (capacity, bad asset, no authored id) is skipped and
/// reported by [`failure`]; it never fails the tick.
pub fn sync_terrains(world: &mut World) -> Result<()> {
    if !world.contains_resource::<Terrains>() {
        return Ok(());
    }
    let owners = sorted_owners(world);
    remove_stale(world, &owners);
    {
        let mut terrains = world.resource_mut::<Terrains>();
        terrains.failures.retain(|e, _| owners.contains(e));
        terrains.render_failures.retain(|e, _| owners.contains(e));
    }
    let mut claimed = BTreeSet::new();
    for entity in owners {
        let result = sync_one(world, entity, &mut claimed);
        record_failure(world, entity, false, result);
    }
    Ok(())
}

fn active_camera_position(world: &mut World) -> Option<Vec3> {
    let mut cameras: Vec<(String, Entity)> = world
        .iter_entities()
        .filter(|e| e.get::<Camera3D>().is_some_and(|c| c.active))
        .map(|e| {
            (
                e.get::<AuthoredId>()
                    .map(|a| a.id.clone())
                    .unwrap_or_else(|| e.id().to_bits().to_string()),
                e.id(),
            )
        })
        .collect();
    cameras.sort();
    let (_, camera) = cameras.into_iter().next()?;
    crate::world_transform(world, camera)
        .ok()
        .map(|t| Vec3::from(t.position()))
}

fn load_images(world: &World, state: &mut State) -> Result<()> {
    if state.images_loaded {
        return Ok(());
    }
    let project = world
        .get_resource::<ProjectResource>()
        .ok_or_else(|| invalid("runtime.project.unavailable", "No project resource"))?
        .0
        .clone();
    for layer in &mut state.layers {
        layer.image = match &layer.texture {
            Some(id) => {
                project.require_asset(id, "texture")?;
                Some(Arc::new(crate::content::load_texture(
                    &project.resolve_asset(id)?,
                )?))
            }
            None => None,
        };
    }
    state.images_loaded = true;
    Ok(())
}

/// Chooses each terrain's render path, rebuilds CPU fallback chunks for the active
/// `Camera3D` and publishes GPU-path terrains in [`TerrainRenderSet`]. Runs from
/// `world::post_transforms`.
pub fn update_terrain_render(world: &mut World) -> Result<()> {
    if !world.contains_resource::<Terrains>() {
        return Ok(());
    }
    let gpu_installed = world.contains_resource::<TerrainGpuRenderer>();
    let camera = active_camera_position(world);
    let owners: Vec<Entity> = world
        .resource::<Terrains>()
        .states
        .keys()
        .copied()
        .collect();
    let mut set = TerrainRenderSet::default();
    for entity in owners {
        let result = render_one(world, entity, gpu_installed, camera, &mut set);
        record_failure(world, entity, true, result);
    }
    world.insert_resource(set);
    Ok(())
}

fn render_one(
    world: &mut World,
    entity: Entity,
    gpu_installed: bool,
    camera: Option<Vec3>,
    set: &mut TerrainRenderSet,
) -> Result<()> {
    let Ok(component) = component(world, entity) else {
        return Ok(());
    };
    let owner = owner_id(world, entity)?;
    let matrix = crate::world_transform(world, entity)?.matrix;
    let mut state = take_state(world, entity);
    let gpu = match component.render_mode {
        TerrainRenderMode::Cpu => false,
        TerrainRenderMode::Gpu => true,
        TerrainRenderMode::Auto => gpu_installed,
    };
    state.render_note =
        (component.render_mode == TerrainRenderMode::Gpu && !gpu_installed).then(|| {
            "render_mode gpu but no terrain GPU renderer is installed \
             (engine_world_render::install); nothing draws"
                .to_owned()
        });
    let result = if !component.visible || gpu {
        for (_, chunk) in std::mem::take(&mut state.chunks) {
            world.despawn(chunk.entity);
        }
        state.cpu = None;
        state.render_path = if component.visible {
            "gpu-feature"
        } else {
            "hidden"
        };
        if component.visible {
            load_images(world, &mut state).map(|_| {
                set.items.push(TerrainRenderItem {
                    owner: entity,
                    id: owner.clone(),
                    field: state.field.clone(),
                    lod: component.lod,
                    layers: state.layers.clone(),
                    world_from_local: matrix,
                });
            })
        } else {
            Ok(())
        }
    } else {
        state.render_path = "cpu-mesh-chunks";
        let viewer = camera.map(|p| {
            Mat4::from_cols_array_2d(&matrix)
                .inverse()
                .transform_point3(p)
        });
        sync_chunks(world, entity, &owner, &component, &mut state, viewer)
    };
    put_state(world, entity, state);
    result
}

fn morph_class(layout: &LodLayout, node: &SelectedNode, viewer: &LodViewer) -> MorphClass {
    if node.morph[0] >= NO_MORPH {
        return MorphClass::Unmorphed;
    }
    let rect = layout.node_samples(node.key);
    let s = layout.spacing();
    let lo = Vec2::new(rect.min[0] as f32 * s, rect.min[1] as f32 * s);
    let hi = Vec2::new((rect.max[0] - 1) as f32 * s, (rect.max[1] - 1) as f32 * s);
    let p = Vec2::new(viewer.position.x, viewer.position.z);
    let near = (lo - p).max(p - hi).max(Vec2::ZERO).length();
    let far = (p - lo).abs().max((hi - p).abs()).length();
    let g2 = viewer.gap * viewer.gap;
    let (near, far) = ((near * near + g2).sqrt(), (far * far + g2).sqrt());
    if far <= node.morph[0] {
        MorphClass::Unmorphed
    } else if near >= node.morph[1] {
        MorphClass::Full
    } else {
        MorphClass::Partial
    }
}

fn chunk_mesh(
    field: &Heightfield,
    layout: &LodLayout,
    node: &SelectedNode,
    viewer: &LodViewer,
) -> Mesh {
    let data = build_chunk_mesh(field, layout, node, viewer);
    Mesh {
        normals: data.normals,
        vertices: data
            .positions
            .iter()
            .zip(&data.uvs)
            .zip(&data.colors)
            .map(|((p, uv), color)| Vertex {
                position: *p,
                uv: *uv,
                color: *color,
            })
            .collect(),
        indices: data.indices,
    }
}

fn sync_chunks(
    world: &mut World,
    entity: Entity,
    owner: &str,
    component: &TerrainComponent,
    state: &mut State,
    viewer: Option<Vec3>,
) -> Result<()> {
    let field = state.field.clone();
    if state.cpu.as_ref().is_none_or(|c| c.lod != component.lod) {
        for (_, chunk) in std::mem::take(&mut state.chunks) {
            world.despawn(chunk.entity);
        }
        state.cpu = Some(Cpu {
            lod: component.lod,
            layout: LodLayout::for_heightfield(&field, &component.lod).map_err(mapped)?,
            viewer: None,
            revision: field.revision(),
            pending: None,
            nodes: 0,
            histogram: Vec::new(),
            budget_limited: false,
        });
    }
    let cpu = state.cpu.as_mut().expect("created above");
    // Without a camera the viewer sits far above the centre: only coarse nodes are drawn.
    let extent = field.extent();
    let position = viewer.unwrap_or(Vec3::new(
        extent[0] * 0.5,
        field.height_range()[1] + 1.0e6,
        extent[1] * 0.5,
    ));
    let threshold = if component.update_distance > 0.0 {
        component.update_distance
    } else {
        field.spacing()
    };
    let revision_changed = cpu.revision != field.revision();
    let moved = cpu
        .viewer
        .is_none_or(|last| last.distance(position) >= threshold);
    if !moved && !revision_changed && !state.chunks.is_empty() {
        return Ok(());
    }
    let selection = cpu.layout.select(position, field.height_range());
    let mut wanted = BTreeSet::new();
    for node in &selection.nodes {
        wanted.insert(node.key);
        let class = morph_class(&cpu.layout, node, &selection.viewer);
        let edited = cpu
            .pending
            .is_some_and(|region| region.intersects(&cpu.layout.node_samples(node.key)));
        if let Some(chunk) = state.chunks.get(&node.key)
            && chunk.morph == class
            && class != MorphClass::Partial
            && !edited
        {
            continue;
        }
        let mesh = chunk_mesh(&field, &cpu.layout, node, &selection.viewer);
        let (vertices, triangles) = (mesh.vertices.len(), mesh.indices.len() / 3);
        let geometry = GeometryOverride(Arc::new(mesh));
        let marker = TerrainChunk {
            owner: entity,
            node: node.key,
        };
        let spawned = match state.chunks.get(&node.key) {
            Some(chunk) => {
                world.entity_mut(chunk.entity).insert(geometry);
                chunk.entity
            }
            None => {
                let k = node.key;
                let name = format!("terrain:{owner}/chunk/{}/{},{}", k.level, k.x, k.z);
                world
                    .spawn((
                        AuthoredId {
                            id: name.clone(),
                            name,
                        },
                        Transform::default(),
                        Parent(owner.to_owned()),
                        MeshRenderer {
                            version: 1,
                            mesh: String::new(),
                            texture: None,
                        },
                        geometry,
                        marker,
                        TerrainGenerated { owner: entity },
                    ))
                    .id()
            }
        };
        state.chunks.insert(
            node.key,
            Chunk {
                entity: spawned,
                morph: class,
                vertices,
                triangles,
            },
        );
    }
    let gone: Vec<NodeKey> = state
        .chunks
        .keys()
        .filter(|k| !wanted.contains(k))
        .copied()
        .collect();
    for key in gone {
        if let Some(chunk) = state.chunks.remove(&key) {
            world.despawn(chunk.entity);
        }
    }
    cpu.viewer = Some(position);
    cpu.revision = field.revision();
    cpu.pending = None;
    cpu.nodes = selection.nodes.len();
    cpu.histogram = selection.level_histogram(cpu.layout.levels());
    cpu.budget_limited = selection.budget_limited;
    Ok(())
}

// ---- native API --------------------------------------------------------------------------

fn edit<R>(
    world: &mut World,
    entity: Entity,
    apply: impl FnOnce(&mut Heightfield) -> engine_terrain::Result<R>,
    region: impl FnOnce(&R) -> Option<Option<SampleRect>>,
) -> Result<R> {
    ensure_loaded(world, entity)?;
    let mut terrains = world.resource_mut::<Terrains>();
    let state = terrains.states.get_mut(&entity).expect("loaded");
    let result = apply(Arc::make_mut(&mut state.field)).map_err(mapped)?;
    // `Some(None)`: nothing changed; `None`: unknown extent, rebuild everything.
    let (columns, rows) = (state.field.columns(), state.field.rows());
    let rect = match region(&result) {
        Some(None) => return Ok(result),
        // Normals read one sample beyond a node, so widen the region by one.
        Some(Some(rect)) => SampleRect::new(
            [rect.min[0].saturating_sub(1), rect.min[1].saturating_sub(1)],
            [(rect.max[0] + 1).min(columns), (rect.max[1] + 1).min(rows)],
        ),
        None => SampleRect::new([0, 0], [columns, rows]),
    };
    state.pending_collision = Some(state.pending_collision.map_or(rect, |p| p.union(rect)));
    if let Some(cpu) = state.cpu.as_mut() {
        cpu.pending = Some(cpu.pending.map_or(rect, |p| p.union(rect)));
    }
    Ok(result)
}

fn samples_of_cells(rect: SampleRect) -> SampleRect {
    SampleRect::new(rect.min, [rect.max[0] + 1, rect.max[1] + 1])
}

/// Applies a sculpt brush (centre in terrain-local coordinates, see [`world_to_local`]).
/// Render chunks and collider tiles touching the returned region rebuild at the next tick.
/// Runtime edits change the live heightfield only; [`export_asset`] serializes it.
pub fn sculpt(
    world: &mut World,
    entity: Entity,
    brush: &Brush,
    op: SculptOp,
) -> Result<EditReport> {
    edit(
        world,
        entity,
        |f| engine_terrain::sculpt(f, brush, op),
        |r: &EditReport| Some(r.region),
    )
}
/// Paints the splat layer named `layer`.
pub fn paint_layer(
    world: &mut World,
    entity: Entity,
    brush: &Brush,
    layer: &str,
) -> Result<EditReport> {
    edit(
        world,
        entity,
        |f| {
            let index = f.layer_index(layer).ok_or_else(|| {
                engine_terrain::Error::new("terrain.layer.unknown", format!("no layer {layer}"))
            })?;
            engine_terrain::paint_layer(f, brush, index)
        },
        |r: &EditReport| Some(r.region),
    )
}
/// Marks (`hole = true`) or clears hole cells; the report's region is in cells.
pub fn paint_holes(
    world: &mut World,
    entity: Entity,
    brush: &Brush,
    hole: bool,
) -> Result<EditReport> {
    edit(
        world,
        entity,
        |f| engine_terrain::paint_holes(f, brush, hole),
        |r: &EditReport| Some(r.region.map(samples_of_cells)),
    )
}
/// Runs `change` on the live heightfield; everything rebuilds at the next tick.
pub fn with_heightfield<R>(
    world: &mut World,
    entity: Entity,
    change: impl FnOnce(&mut Heightfield) -> R,
) -> Result<R> {
    edit(world, entity, |f| Ok(change(f)), |_| None)
}
/// The live heightfield (loading the asset first if needed).
pub fn heightfield(world: &mut World, entity: Entity) -> Result<Arc<Heightfield>> {
    ensure_loaded(world, entity)?;
    Ok(world.resource::<Terrains>().states[&entity].field.clone())
}
/// Terrain-local coordinates of a world-space point.
pub fn world_to_local(world: &World, entity: Entity, point: [f32; 3]) -> Result<[f32; 3]> {
    let matrix = Mat4::from_cols_array_2d(&crate::world_transform(world, entity)?.matrix);
    if matrix.determinant().abs() < 1e-12 {
        return Err(invalid("terrain.transform", "Singular terrain transform"));
    }
    Ok(matrix.inverse().transform_point3(point.into()).to_array())
}
/// World-space surface point under world `(x, z)`, or `None` outside the terrain or over a
/// hole. Assumes an upright terrain.
pub fn height_at_world(world: &mut World, entity: Entity, x: f32, z: f32) -> Result<Option<f32>> {
    let field = heightfield(world, entity)?;
    let local = world_to_local(world, entity, [x, 0.0, z])?;
    if !field.contains(local[0], local[2]) || field.is_hole_at(local[0], local[2]) {
        return Ok(None);
    }
    let h = field.height_at(local[0], local[2]);
    let matrix = Mat4::from_cols_array_2d(&crate::world_transform(world, entity)?.matrix);
    Ok(Some(
        matrix.transform_point3(Vec3::new(local[0], h, local[2])).y,
    ))
}
/// The live heightfield as `.terrain` file bytes (write them to the asset to keep edits).
pub fn export_asset(world: &mut World, entity: Entity) -> Result<Vec<u8>> {
    Ok(engine_terrain::asset::encode(&*heightfield(world, entity)?))
}
/// Navigation bake input of the terrain in absolute space (`WorldOrigin.offset` plus the local
/// position; `region` is absolute too). The terrain transform must be a pure translation
/// (navigation meshes are axis-aligned).
pub fn navigation_input(
    world: &mut World,
    entity: Entity,
    settings: crate::simulation::BakeSettings,
    region: Option<[[f32; 2]; 2]>,
) -> Result<crate::simulation::BakeInput> {
    let field = heightfield(world, entity)?;
    let matrix = Mat4::from_cols_array_2d(&crate::world_transform(world, entity)?.matrix);
    let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
    if !scale.abs_diff_eq(Vec3::ONE, 1e-4) || !rotation.abs_diff_eq(glam::Quat::IDENTITY, 1e-4) {
        return Err(invalid(
            "terrain.navigation.transform",
            "terrain navigation input needs a translation-only terrain transform",
        ));
    }
    // Navigation meshes are absolute: the terrain's local-frame translation plus the offset the
    // local frame is expressed in.
    let offset = crate::world::coordinates::origin(world);
    let origin: [f32; 3] = std::array::from_fn(|i| (f64::from(translation[i]) + offset[i]) as f32);
    let options = engine_terrain::NavInputOptions {
        origin,
        region,
        ..engine_terrain::NavInputOptions::default()
    };
    super::navigation::navigation_input(&field, &options, settings, Vec::new())
}
/// Counts for telemetry and tests; `None` before the entity's first sync.
pub fn status(world: &World, entity: Entity) -> Option<TerrainStatus> {
    let state = world.get_resource::<Terrains>()?.states.get(&entity)?;
    let mut notes: Vec<String> = Vec::new();
    notes.extend(state.surface_note.clone());
    notes.extend(state.render_note.clone());
    let cpu = state.cpu.as_ref();
    Some(TerrainStatus {
        samples: [state.field.columns(), state.field.rows()],
        revision: state.field.revision(),
        levels: cpu.map_or(0, |c| c.layout.levels()),
        render_path: state.render_path,
        nodes: cpu.map_or(0, |c| c.nodes),
        level_histogram: cpu.map_or_else(Vec::new, |c| c.histogram.clone()),
        budget_limited: cpu.is_some_and(|c| c.budget_limited),
        chunk_entities: state.chunks.len(),
        vertices: state.chunks.values().map(|c| c.vertices).sum(),
        triangles: state.chunks.values().map(|c| c.triangles).sum(),
        colliders: state.colliders.len(),
        holed_colliders: state.colliders.values().filter(|(_, holed)| *holed).count(),
        surface: state.surface.as_ref().map(|(n, ..)| n.clone()),
        notes,
    })
}
