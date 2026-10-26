//! Runtime foliage service: generates or loads instances, keeps them on the ground, draws
//! them through the CPU cluster fallback or publishes them for the GPU feature, and exposes
//! the native painting and query API.
use super::assets::{Loaded, load_foliage, mapped, resolve_density_maps};
use super::component::{
    FoliageComponent, FoliageGenerated, FoliageMode, FoliageRenderMode, GenerationKey,
};
use super::cpu::{self, Cpu};
use super::ground::{self, GroundKey};
use crate::content::{Mesh, Texture};
use crate::rendering3d::Camera3D;
use crate::runtime::{AuthoredId, ProjectResource, RuntimeLog};
use crate::{Error, Result};
use bevy_ecs::prelude::*;
use engine_foliage::{
    Brush, FoliageField, FoliageType, LodBands, PaintContext, PaintOp, PaintReport, ScatterParams,
    TypeReport, WindParams, scatter, validate_types,
};
use engine_pcg::Aabb;
use glam::Vec3;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Most `engine.Foliage` entities in one world.
pub const MAX_FOLIAGES: usize = 16;

fn invalid(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

/// Marker resource: a GPU foliage renderer (the `world.foliage` render feature of
/// `engine-world-render`) is installed. `render_mode: auto` foliage then builds no CPU
/// cluster entities and publishes itself in [`FoliageRenderSet`] instead.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FoliageGpuRenderer;

/// The billboard of a type as drawn.
#[derive(Clone, Debug)]
pub struct RenderImpostor {
    pub texture: Arc<Texture>,
    /// Width and height in metres at scale 1.
    pub size: [f32; 2],
}

/// One foliage type as the GPU feature draws it.
#[derive(Clone, Debug)]
pub struct FoliageRenderType {
    pub name: String,
    /// LOD 0 first.
    pub meshes: Vec<Arc<Mesh>>,
    pub texture: Option<Arc<Texture>>,
    pub tint: [f32; 4],
    pub impostor: Option<RenderImpostor>,
    pub bands: LodBands,
    /// How strongly the wind bends the type (0..=1).
    pub wind_weight: f32,
}

/// A foliage set the GPU feature draws this frame.
#[derive(Clone, Debug)]
pub struct FoliageRenderItem {
    pub owner: Entity,
    /// Authored id of the foliage entity.
    pub id: String,
    pub types: Vec<FoliageRenderType>,
    /// The instances (shared, not copied).
    pub field: Arc<FoliageField>,
    pub wind: WindParams,
}

/// Foliage on the GPU path, rebuilt after transforms each tick. The render extraction copies
/// it into the render world.
#[derive(Resource, Clone, Debug, Default)]
pub struct FoliageRenderSet {
    pub items: Vec<FoliageRenderItem>,
}

/// Snapshot of one foliage entity's runtime state.
#[derive(Clone, Debug, PartialEq)]
pub struct FoliageStatus {
    pub instances: usize,
    pub cells: usize,
    /// Field revision (changes with every edit, regeneration and re-projection).
    pub revision: u64,
    /// Instances per type, in type order.
    pub per_type: Vec<(String, usize)>,
    /// `cpu-clusters`, `gpu-feature` or `hidden`.
    pub render_path: &'static str,
    /// CPU path: cluster entities, their vertices and clusters per level (mesh LOD 0..=3,
    /// impostor).
    pub cluster_entities: usize,
    pub cpu_vertices: usize,
    pub cluster_levels: [usize; 5],
    /// CPU path: some clusters were dropped or trimmed by `cpu_vertex_budget`.
    pub budget_limited: bool,
    /// Generation and loading attempts so far.
    pub attempts: u64,
    /// Rejection counts of the last scatter (generated mode).
    pub scatter: Vec<TypeReport>,
    /// What the ground is (`terrain:<id>`, `surface:<name>`) or why there is none.
    pub ground: String,
    /// Empty on success; otherwise why the last attempt failed (the previous output stays).
    pub diagnostics: Vec<String>,
    pub notes: Vec<String>,
}

struct State {
    applied: Option<GenerationKey>,
    ground_key: Option<std::result::Result<GroundKey, String>>,
    ground_label: String,
    types: Vec<FoliageType>,
    field: Arc<FoliageField>,
    loaded: Option<Loaded>,
    cpu: Cpu,
    render_path: &'static str,
    notes: Vec<String>,
    diagnostics: Vec<String>,
    attempts: u64,
    scatter: Vec<TypeReport>,
    render_error: Option<String>,
    /// `WorldOrigin.offset` the field's positions are expressed in (assets are absolute: zero).
    frame: [f64; 3],
}

impl State {
    fn new() -> Self {
        Self {
            applied: None,
            ground_key: None,
            ground_label: String::new(),
            types: Vec::new(),
            field: Arc::new(FoliageField::new(32.0).expect("valid cell size")),
            loaded: None,
            cpu: Cpu::default(),
            render_path: "hidden",
            notes: Vec::new(),
            diagnostics: Vec::new(),
            attempts: 0,
            scatter: Vec::new(),
            render_error: None,
            frame: [0.0; 3],
        }
    }
}

/// Runtime foliage state, one entry per `engine.Foliage` entity.
#[derive(Resource, Default)]
pub struct Foliages {
    states: BTreeMap<Entity, State>,
    /// Entities that could not be given a state (capacity, missing authored id) and why. The
    /// rest of the world keeps ticking; see [`failure`].
    failures: BTreeMap<Entity, String>,
}

pub(super) fn install(world: &mut World) {
    world.insert_resource(Foliages::default());
    world.insert_resource(FoliageRenderSet::default());
}

fn owner_id(world: &World, entity: Entity) -> Result<String> {
    world
        .get::<AuthoredId>(entity)
        .map(|a| a.id.clone())
        .filter(|id| crate::services::find_entity(world, id) == Some(entity))
        .ok_or_else(|| {
            invalid(
                "foliage.owner",
                "engine.Foliage entities need an authored ID registered in the entity lookup",
            )
        })
}

fn sorted_owners(world: &mut World) -> Vec<Entity> {
    let mut owners: Vec<(String, Entity)> = world
        .iter_entities()
        .filter(|e| e.contains::<FoliageComponent>())
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

fn log(world: &mut World, id: &str, message: &str) {
    if let Some(mut log) = world.get_resource_mut::<RuntimeLog>() {
        log.0.push(format!("foliage {id}: {message}"));
    }
}

/// Records why `entity` has no usable foliage state, logging each distinct reason once.
fn fail(world: &mut World, entity: Entity, label: &str, message: String) {
    let known = world
        .resource::<Foliages>()
        .failures
        .get(&entity)
        .is_some_and(|m| *m == message);
    if !known {
        log(world, label, &message);
        world
            .resource_mut::<Foliages>()
            .failures
            .insert(entity, message);
    }
}

fn remove_stale(world: &mut World, owners: &[Entity]) {
    world
        .resource_mut::<Foliages>()
        .failures
        .retain(|e, _| owners.contains(e));
    let stale: Vec<Entity> = world
        .resource::<Foliages>()
        .states
        .keys()
        .filter(|e| !owners.contains(e))
        .copied()
        .collect();
    for entity in stale {
        if let Some(mut state) = world.resource_mut::<Foliages>().states.remove(&entity) {
            state.cpu.clear(world);
        }
    }
    let orphans: Vec<Entity> = {
        let foliages = world.resource::<Foliages>();
        world
            .iter_entities()
            .filter(|e| {
                e.get::<FoliageGenerated>()
                    .is_some_and(|g| !foliages.states.contains_key(&g.owner))
            })
            .map(|e| e.id())
            .collect()
    };
    for entity in orphans {
        world.despawn(entity);
    }
}

fn bounds_of(world: &World, entity: Entity, component: &FoliageComponent) -> Result<Aabb> {
    let position = crate::world_transform(world, entity)?.position();
    let half = component.size.map(|v| v * 0.5);
    let centre: [f32; 3] = std::array::from_fn(|i| position[i] + component.center[i]);
    Ok(Aabb::new(
        std::array::from_fn(|i| centre[i] - half[i]),
        std::array::from_fn(|i| centre[i] + half[i]),
    ))
}

// ---- generation ----------------------------------------------------------------------------

fn generate(
    world: &mut World,
    entity: Entity,
    component: &FoliageComponent,
    ground: Result<ground::Ground>,
    state: &mut State,
) -> Result<()> {
    let ground = ground?;
    let project = world
        .get_resource::<ProjectResource>()
        .ok_or_else(|| invalid("runtime.project.unavailable", "No project resource"))?
        .0
        .clone();
    let mut resolved = component.types.clone();
    resolve_density_maps(&project, &mut resolved)?;
    let bounds = bounds_of(world, entity, component)?;
    let origin = crate::world::coordinates::origin(world);
    let params = ScatterParams {
        seed: component.seed,
        bounds,
        cell_size: component.cell_size,
        zones: &component.exclusion_zones,
        holes: ground.holes(),
        max_instances: component.max_instances as usize,
        // Candidate cells hash in absolute coordinates: the same forest at any origin offset.
        origin: [origin[0], origin[2]],
    };
    let result = scatter(&resolved, &*ground.surface, &params).map_err(mapped)?;
    state.types = component.types.clone();
    state.field = Arc::new(result.field);
    state.scatter = result.types;
    state.frame = origin;
    Ok(())
}

fn load_asset_mode(world: &World, component: &FoliageComponent, state: &mut State) -> Result<()> {
    let project = world
        .get_resource::<ProjectResource>()
        .ok_or_else(|| invalid("runtime.project.unavailable", "No project resource"))?
        .0
        .clone();
    let asset = load_foliage(&project, &component.instances)?;
    let mut types = asset.types;
    let mut notes = Vec::new();
    for over in &component.types {
        match types.iter_mut().find(|t| t.name == over.name) {
            Some(slot) => *slot = over.clone(),
            None => notes.push(format!(
                "type override `{}` matches no type of the asset",
                over.name
            )),
        }
    }
    validate_types(&types).map_err(mapped)?;
    state.types = types;
    state.field = Arc::new(asset.field);
    state.frame = [0.0; 3];
    state.scatter.clear();
    state.notes.retain(|n| !n.starts_with("type override"));
    state.notes.extend(notes);
    Ok(())
}

/// Puts every instance back on the ground: height from the surface, instances over terrain
/// holes removed. Returns `(moved or removed instances)`.
fn reproject(state: &mut State, ground: &ground::Ground) -> usize {
    let types = state.types.clone();
    let bounds = ground.surface.bounds();
    let field = Arc::make_mut(&mut state.field);
    let (_, removed) = field.map_instances(|i| {
        let (x, z) = (i.position[0], i.position[2]);
        if x < bounds.min.x || x > bounds.max.x || z < bounds.min.z || z > bounds.max.z {
            return true;
        }
        if ground.holes().is_some_and(|h| h.is_hole(x, z)) {
            return false;
        }
        let offset = types
            .get(usize::from(i.type_index))
            .map_or(0.0, |t| t.ground_offset);
        i.position[1] = ground.surface.height(x, z) + offset;
        true
    });
    removed
}

fn ground_label(component: &FoliageComponent) -> String {
    if component.terrain.is_empty() {
        format!("surface:{}", component.surface)
    } else {
        format!("terrain:{}", component.terrain)
    }
}

/// Moves the instances into the current `WorldOrigin` frame when it differs from the one they
/// are expressed in (after an origin shift, or for absolute asset positions). The coordinates
/// service already shifted the CPU cluster entities; they are dropped and rebuilt from the
/// moved field so nothing is shifted twice. Returns whether the frame changed.
fn reexpress(world: &mut World, state: &mut State) -> Result<bool> {
    let offset = crate::world::coordinates::origin(world);
    if state.frame == offset {
        return Ok(false);
    }
    let delta: [f64; 3] = std::array::from_fn(|i| state.frame[i] - offset[i]);
    if !state.field.is_empty() {
        let mut moved = FoliageField::new(state.field.cell_size()).map_err(mapped)?;
        moved
            .extend(state.field.iter().map(|instance| {
                let mut instance = *instance;
                for (value, delta) in instance.position.iter_mut().zip(delta) {
                    *value = (f64::from(*value) + delta) as f32;
                }
                instance
            }))
            .map_err(mapped)?;
        state.field = Arc::new(moved);
        state.cpu.clear(world);
    }
    state.frame = offset;
    Ok(true)
}

fn sync_one(
    world: &mut World,
    entity: Entity,
    id: &str,
    component: &FoliageComponent,
    state: &mut State,
    rebased: bool,
) {
    let key = component.generation_key();
    let ground = ground::resolve(world, component);
    let ground_key = ground
        .as_ref()
        .map(|g| g.key.clone())
        .map_err(|e| e.to_string());
    if rebased && ground_key.is_ok() && state.ground_key.as_ref().is_some_and(|k| k.is_ok()) {
        // The terrain matrix or surface changed with the origin, not its content: keep the
        // translated instances instead of regenerating them in the new frame.
        state.ground_key = Some(ground_key.clone());
    }
    let key_changed = state.applied.as_ref() != Some(&key);
    let ground_changed = state.ground_key.as_ref() != Some(&ground_key);
    if !key_changed && !ground_changed {
        return;
    }
    state.applied = Some(key);
    state.ground_key = Some(ground_key);
    state.ground_label = match &ground {
        Ok(_) => ground_label(component),
        Err(e) => format!("{} ({e})", ground_label(component)),
    };
    state.attempts += 1;
    let result = match component.mode {
        FoliageMode::Generated => generate(world, entity, component, ground, state),
        FoliageMode::Asset => {
            let loaded = if key_changed {
                load_asset_mode(world, component, state)
            } else {
                Ok(())
            };
            // A freshly loaded asset holds absolute positions: bring them into the current
            // frame before they are projected onto the (local-frame) ground.
            loaded
                .and_then(|()| reexpress(world, state).map(|_| ()))
                .map(|()| {
                    if let Ok(g) = &ground {
                        reproject(state, g);
                    }
                })
        }
    };
    match result {
        Ok(()) => state.diagnostics.clear(),
        Err(error) => {
            log(world, id, &error.to_string());
            state.diagnostics = vec![error.to_string()];
        }
    }
}

/// Generates, loads and re-projects foliage. Runs from `world::pre_simulation`, after the
/// terrain service, so an edit made by a script reaches the foliage on the next tick.
pub fn sync_foliage(world: &mut World) -> Result<()> {
    if !world.contains_resource::<Foliages>() {
        return Ok(());
    }
    let owners = sorted_owners(world);
    remove_stale(world, &owners);
    for entity in owners {
        let Some(component) = world.get::<FoliageComponent>(entity).cloned() else {
            continue;
        };
        let id = match owner_id(world, entity) {
            Ok(id) => id,
            Err(error) => {
                let label = format!("entity:{}", entity.to_bits());
                fail(world, entity, &label, error.to_string());
                continue;
            }
        };
        let known = world.resource::<Foliages>().states.contains_key(&entity);
        if !known && world.resource::<Foliages>().states.len() >= MAX_FOLIAGES {
            let message = format!("foliage.capacity: At most {MAX_FOLIAGES} foliage entities");
            fail(world, entity, &id, message);
            continue;
        }
        world.resource_mut::<Foliages>().failures.remove(&entity);
        let mut state = world
            .resource_mut::<Foliages>()
            .states
            .remove(&entity)
            .unwrap_or_else(State::new);
        let rebased = reexpress(world, &mut state);
        if let Err(error) = component.validate() {
            if state.diagnostics != [error.to_string()] {
                log(world, &id, &error.to_string());
                state.diagnostics = vec![error.to_string()];
            }
        } else {
            let rebased = rebased.as_ref().is_ok_and(|r| *r);
            sync_one(world, entity, &id, &component, &mut state, rebased);
        }
        // A freshly loaded asset holds absolute positions.
        if let Err(error) = rebased.and_then(|_| reexpress(world, &mut state)) {
            log(world, &id, &error.to_string());
            state.diagnostics = vec![error.to_string()];
        }
        world
            .resource_mut::<Foliages>()
            .states
            .insert(entity, state);
    }
    Ok(())
}

// ---- rendering -----------------------------------------------------------------------------

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

fn ensure_loaded(world: &World, state: &mut State) -> Result<()> {
    if state
        .loaded
        .as_ref()
        .is_some_and(|l| l.types == state.types)
    {
        return Ok(());
    }
    let project = world
        .get_resource::<ProjectResource>()
        .ok_or_else(|| invalid("runtime.project.unavailable", "No project resource"))?
        .0
        .clone();
    state.loaded = Some(Loaded::load(&project, &state.types)?);
    Ok(())
}

fn render_item(
    entity: Entity,
    id: &str,
    component: &FoliageComponent,
    state: &State,
) -> Option<FoliageRenderItem> {
    let loaded = state.loaded.as_ref()?;
    let c = &component.culling;
    let types = state
        .types
        .iter()
        .zip(&loaded.data)
        .map(|(t, data)| FoliageRenderType {
            name: t.name.clone(),
            meshes: data.meshes.clone(),
            texture: data.texture.clone(),
            tint: t.tint,
            impostor: t
                .impostor
                .as_ref()
                .zip(data.impostor.clone())
                .map(|(i, texture)| RenderImpostor {
                    texture,
                    size: i.size,
                }),
            bands: LodBands::for_type(t, c.max_distance, c.lod_scale, c.hysteresis),
            wind_weight: t.wind_weight,
        })
        .collect();
    Some(FoliageRenderItem {
        owner: entity,
        id: id.to_owned(),
        types,
        field: state.field.clone(),
        wind: component.wind,
    })
}

/// Chooses each foliage's render path, builds CPU cluster meshes for the active `Camera3D`
/// and publishes GPU-path foliage in [`FoliageRenderSet`]. Runs from `world::post_transforms`.
pub fn update_foliage_render(world: &mut World) -> Result<()> {
    if !world.contains_resource::<Foliages>() {
        return Ok(());
    }
    let gpu_installed = world.contains_resource::<FoliageGpuRenderer>();
    let camera = active_camera_position(world);
    let owners: Vec<Entity> = world
        .resource::<Foliages>()
        .states
        .keys()
        .copied()
        .collect();
    let mut set = FoliageRenderSet::default();
    for entity in owners {
        let Some(component) = world.get::<FoliageComponent>(entity).cloned() else {
            continue;
        };
        let Ok(id) = owner_id(world, entity) else {
            continue;
        };
        let mut state = world
            .resource_mut::<Foliages>()
            .states
            .remove(&entity)
            .expect("listed above");
        let gpu = match component.render_mode {
            FoliageRenderMode::Cpu => false,
            FoliageRenderMode::Gpu => true,
            FoliageRenderMode::Auto => gpu_installed,
        };
        state.notes.retain(|n| !n.starts_with("render"));
        if component.render_mode == FoliageRenderMode::Gpu && !gpu_installed {
            state.notes.push(
                "render_mode gpu but no foliage GPU renderer is installed \
                 (engine_world_render::install); nothing draws"
                    .to_owned(),
            );
        }
        let loaded = if component.visible && !state.field.is_empty() {
            ensure_loaded(world, &mut state)
        } else {
            Ok(())
        };
        if let Err(error) = loaded {
            let message = format!("render assets: {error}");
            if state.render_error.as_ref() != Some(&message) {
                log(world, &id, &message);
                state.render_error = Some(message.clone());
            }
            state.notes.push(message);
            state.cpu.clear(world);
            state.render_path = "hidden";
        } else if !component.visible {
            state.render_error = None;
            state.cpu.clear(world);
            state.render_path = "hidden";
        } else if gpu {
            state.cpu.clear(world);
            state.render_path = "gpu-feature";
            if let Some(item) = render_item(entity, &id, &component, &state) {
                set.items.push(item);
            }
        } else {
            state.render_path = "cpu-clusters";
            if let Some(loaded) = state.loaded.clone().filter(|_| !state.field.is_empty()) {
                let field = state.field.clone();
                let types = state.types.clone();
                cpu::update(
                    world,
                    &mut state.cpu,
                    &cpu::Inputs {
                        owner: entity,
                        owner_id: &id,
                        field: &field,
                        types: &types,
                        loaded: &loaded,
                        default_cull: component.culling.max_distance,
                        lod_scale: component.culling.lod_scale,
                        hysteresis: component.culling.hysteresis,
                        budget: component.cpu_vertex_budget,
                        camera,
                    },
                );
            } else {
                state.cpu.clear(world);
            }
        }
        world
            .resource_mut::<Foliages>()
            .states
            .insert(entity, state);
    }
    world.insert_resource(set);
    Ok(())
}

// ---- native API ----------------------------------------------------------------------------

/// Paints into the live instances of `entity` (stroke centre and radius in world units).
/// Edits last until the foliage regenerates; [`export_asset`] serializes them.
pub fn paint(
    world: &mut World,
    entity: Entity,
    brush: &Brush,
    op: PaintOp,
    seed: u64,
) -> Result<PaintReport> {
    let component = world
        .get::<FoliageComponent>(entity)
        .cloned()
        .ok_or_else(|| invalid("foliage.component.missing", "Entity has no engine.Foliage"))?;
    component.validate()?;
    if !world.contains_resource::<Foliages>() {
        install(world);
    }
    if !world.resource::<Foliages>().states.contains_key(&entity) {
        sync_foliage(world)?;
    }
    let bounds = bounds_of(world, entity, &component)?;
    let ground = ground::resolve(world, &component)?;
    let mut state = world
        .resource_mut::<Foliages>()
        .states
        .remove(&entity)
        .ok_or_else(|| {
            invalid(
                "foliage.state.missing",
                "The foliage has not been synced yet",
            )
        })?;
    let types = state.types.clone();
    let origin = crate::world::coordinates::origin(world);
    let ctx = PaintContext {
        surface: &*ground.surface,
        bounds,
        zones: &component.exclusion_zones,
        holes: ground.holes(),
        max_instances: component.max_instances as usize,
        origin: [origin[0], origin[2]],
    };
    let result = engine_foliage::paint(
        Arc::make_mut(&mut state.field),
        &types,
        &ctx,
        brush,
        op,
        seed,
    )
    .map_err(mapped);
    world
        .resource_mut::<Foliages>()
        .states
        .insert(entity, state);
    result
}

/// The live instances (loading or generating them first if the foliage has not been synced).
pub fn field(world: &mut World, entity: Entity) -> Result<Arc<FoliageField>> {
    if !world
        .get_resource::<Foliages>()
        .is_some_and(|f| f.states.contains_key(&entity))
    {
        sync_foliage(world)?;
    }
    world
        .resource::<Foliages>()
        .states
        .get(&entity)
        .map(|s| s.field.clone())
        .ok_or_else(|| invalid("foliage.state.missing", "The entity has no foliage state"))
}

/// Why `entity` has no foliage state (more than [`MAX_FOLIAGES`] foliage entities, no
/// registered authored id), if that is the case. Such an entity is skipped; the rest keep running.
pub fn failure(world: &World, entity: Entity) -> Option<String> {
    world
        .get_resource::<Foliages>()?
        .failures
        .get(&entity)
        .cloned()
}

/// The effective types (generated mode: the component's; asset mode: the asset's with the
/// component's overrides).
pub fn types(world: &World, entity: Entity) -> Option<Vec<FoliageType>> {
    world
        .get_resource::<Foliages>()?
        .states
        .get(&entity)
        .map(|s| s.types.clone())
}

/// The live instances as `.foliage` file bytes (write them to the asset to keep edits).
/// Positions are written in absolute coordinates (`WorldOrigin.offset + local`).
pub fn export_asset(world: &mut World, entity: Entity) -> Result<Vec<u8>> {
    let field = field(world, entity)?;
    let types = types(world, entity).unwrap_or_default();
    let offset = crate::world::coordinates::origin(world);
    if offset == [0.0; 3] {
        return engine_foliage::asset::encode(&types, &field).map_err(mapped);
    }
    let mut absolute = FoliageField::new(field.cell_size()).map_err(mapped)?;
    absolute
        .extend(field.iter().map(|instance| {
            let mut instance = *instance;
            for (value, offset) in instance.position.iter_mut().zip(offset) {
                *value = (f64::from(*value) + offset) as f32;
            }
            instance
        }))
        .map_err(mapped)?;
    engine_foliage::asset::encode(&types, &absolute).map_err(mapped)
}

/// Counts for telemetry and tests; `None` before the entity's first sync.
pub fn status(world: &World, entity: Entity) -> Option<FoliageStatus> {
    let state = world.get_resource::<Foliages>()?.states.get(&entity)?;
    let counts = state.field.counts_per_type(state.types.len());
    Some(FoliageStatus {
        instances: state.field.len(),
        cells: state.field.cell_count(),
        revision: state.field.revision(),
        per_type: state
            .types
            .iter()
            .map(|t| t.name.clone())
            .zip(counts)
            .collect(),
        render_path: state.render_path,
        cluster_entities: state.cpu.cluster_count(),
        cpu_vertices: state.cpu.vertices,
        cluster_levels: state.cpu.levels,
        budget_limited: state.cpu.budget_limited,
        attempts: state.attempts,
        scatter: state.scatter.clone(),
        ground: state.ground_label.clone(),
        diagnostics: state.diagnostics.clone(),
        notes: state.notes.clone(),
    })
}
