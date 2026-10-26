//! The runtime side of a partition: sources, scheduling, background reads, applying cells to
//! the world, unloading and HLOD proxies. Origin rebasing is owned by `world::coordinates`:
//! it shifts the cell roots and proxies like every other root entity, so this module only
//! reads `WorldOrigin` when it converts between absolute and local positions.
use super::component::{StreamingSource, WorldPartition, load_manifest};
use super::status::{
    CellInfo, LayerStatus, MAX_DIAGNOSTICS, MAX_LISTED_CELLS, MAX_PARTITIONS, PartitionStatus,
    StreamingStatus, clip,
};
use crate::content::Mesh;
use crate::runtime::{AuthoredId, EntityLookup, ProjectResource, RuntimeLog};
use crate::world::coordinates::WorldOrigin;
use crate::{
    Components, EngineEvent, Error, GeometryOverride, MeshRenderer, Result, Scene, SceneEntity,
    Transform,
};
use bevy_ecs::prelude::*;
use engine_streaming::{CellId, CellKey, CellState, Layer, Limits, Source, Streamer, TickBudget};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// Event raised when a cell's content has been spawned. Payload: `partition`, `cell`, `layer`,
/// `entities`.
pub const EVENT_CELL_LOADED: &str = "streaming.cell_loaded";
/// Event raised when a cell's content has been despawned. Same payload.
pub const EVENT_CELL_UNLOADED: &str = "streaming.cell_unloaded";
/// Event raised when reading or applying a cell failed. Payload adds `error`.
pub const EVENT_CELL_FAILED: &str = "streaming.cell_failed";
/// Most proxies spawned per tick.
const HLOD_SPAWNS_PER_TICK: usize = 64;
const MAX_MESH_CACHE: usize = 256;
const MAX_CELL_ENTITIES: usize = 65_536;

/// Marks entities that belong to a streamed cell (content and HLOD proxies).
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct StreamedCell {
    pub partition: String,
    pub key: [i32; 3],
    pub layer: String,
    /// `true` for the HLOD proxy entity, `false` for cell content.
    pub proxy: bool,
}

/// All streaming state of a world.
#[derive(Resource, Default)]
pub struct StreamingRuntime {
    pub(super) partitions: BTreeMap<String, Partition>,
    /// Partitions that could not be opened: their entity and manifest, retried when either
    /// changes.
    broken: BTreeMap<String, (Entity, String)>,
    errors: BTreeMap<String, String>,
}

struct CellRt {
    file: String,
    hlod: Option<String>,
    layer: String,
    name: String,
    spawned: Vec<(String, Entity)>,
}

struct JobResult {
    id: CellId,
    generation: u64,
    scene: std::result::Result<Scene, String>,
}

#[derive(Default)]
struct Stats {
    started: usize,
    applied: usize,
    bytes: u64,
    total_loaded: u64,
    total_unloaded: u64,
    total_failed: u64,
    proxy_mesh_reads: u64,
}

pub(super) struct Partition {
    id: String,
    pub(super) entity: Entity,
    pub(super) manifest_id: String,
    streamer: Streamer,
    cells: BTreeMap<CellId, CellRt>,
    tx: Sender<JobResult>,
    rx: Mutex<Receiver<JobResult>>,
    running: BTreeMap<CellId, u64>,
    ready: BTreeMap<CellId, JobResult>,
    next_generation: u64,
    proxies: BTreeMap<CellId, (String, Entity)>,
    bad_proxies: BTreeSet<CellId>,
    /// Proxy meshes by file with the clock value of their last use (LRU).
    mesh_cache: BTreeMap<String, (Arc<Mesh>, u64)>,
    mesh_clock: u64,
    stats: Stats,
    sources: usize,
    diagnostics: VecDeque<String>,
}

fn cell_name(key: CellKey, layer: &str) -> String {
    let [x, y, z] = key.0;
    if layer.is_empty() {
        format!("{x}_{y}_{z}")
    } else {
        format!("{x}_{y}_{z}@{layer}")
    }
}

fn limits(component: &WorldPartition) -> Limits {
    Limits {
        cells_per_tick: component.loads_per_tick as usize,
        bytes_per_tick: if component.bytes_per_tick == 0 {
            u64::MAX
        } else {
            component.bytes_per_tick
        },
        max_in_flight: component.max_in_flight as usize,
        max_loaded: component.max_loaded_cells as usize,
    }
}

fn origin_of(world: &World) -> [f64; 3] {
    world
        .get_resource::<WorldOrigin>()
        .map_or([0.0; 3], |o| o.offset)
}

impl Partition {
    fn open(world: &World, id: &str, entity: Entity, component: &WorldPartition) -> Result<Self> {
        component.check()?;
        let project = world.resource::<ProjectResource>().0.clone();
        let manifest = load_manifest(&project, &component.manifest)?;
        let invalid = |e: String| Error::new("streaming.manifest.invalid", e);
        let grid = manifest.grid().map_err(|e| invalid(e.0))?;
        let mut streamer = Streamer::new(grid, limits(component));
        let mut cells = BTreeMap::new();
        for cell in &manifest.cells {
            let layer = streamer.layer(&cell.layer).map_err(|e| invalid(e.0))?;
            let cell_id = CellId {
                key: cell.key,
                layer,
            };
            streamer
                .add_cell(cell_id, cell.bytes.max(1))
                .map_err(|e| invalid(e.0))?;
            cells.insert(
                cell_id,
                CellRt {
                    file: cell.file.clone(),
                    hlod: cell.hlod.as_ref().map(|h| h.file.clone()),
                    layer: cell.layer.clone(),
                    name: cell_name(cell.key, &cell.layer),
                    spawned: Vec::new(),
                },
            );
        }
        let (tx, rx) = channel();
        Ok(Self {
            id: id.to_owned(),
            entity,
            manifest_id: component.manifest.clone(),
            streamer,
            cells,
            tx,
            rx: Mutex::new(rx),
            running: BTreeMap::new(),
            ready: BTreeMap::new(),
            next_generation: 0,
            proxies: BTreeMap::new(),
            bad_proxies: BTreeSet::new(),
            mesh_cache: BTreeMap::new(),
            mesh_clock: 0,
            stats: Stats::default(),
            sources: 0,
            diagnostics: VecDeque::new(),
        })
    }

    fn note(&mut self, world: &mut World, message: String) {
        let message = clip(message);
        world
            .resource_mut::<RuntimeLog>()
            .0
            .push(format!("streaming {}: {message}", self.id));
        if self.diagnostics.len() >= MAX_DIAGNOSTICS {
            self.diagnostics.pop_front();
        }
        self.diagnostics.push_back(message);
    }

    /// Resolves a cell by key and layer name.
    pub(super) fn resolve(&self, key: [i32; 3], layer: &str) -> Option<CellId> {
        let layer = self.streamer.layer_by_name(layer)?;
        let id = CellId {
            key: CellKey(key),
            layer,
        };
        self.streamer.state(id).map(|_| id)
    }

    pub(super) fn state(&self, key: [i32; 3], layer: &str) -> Option<CellState> {
        self.resolve(key, layer)
            .and_then(|id| self.streamer.state(id))
    }

    pub(super) fn infos(&self) -> Vec<CellInfo> {
        self.cells
            .iter()
            .map(|(id, cell)| CellInfo {
                key: id.key.0,
                layer: cell.layer.clone(),
                state: self.streamer.state(*id).unwrap_or(CellState::Unloaded),
                pinned: self.streamer.is_pinned(*id),
                entities: cell.spawned.len(),
                proxy_visible: self.proxies.contains_key(id),
            })
            .collect()
    }

    pub(super) fn pin(&mut self, id: CellId, pinned: bool) -> Result<()> {
        let result = if pinned {
            self.streamer.pin(id)
        } else {
            self.streamer.unpin(id)
        };
        result.map_err(|e| Error::new("streaming.cell.unknown", e.0))
    }

    pub(super) fn cell_label(&self, id: CellId) -> String {
        self.cells
            .get(&id)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }

    fn status(&self) -> PartitionStatus {
        let counts = self.streamer.counts();
        let mut loaded_cells = Vec::new();
        let mut loaded_entities = 0;
        for (id, cell) in &self.cells {
            if self.streamer.state(*id) == Some(CellState::Loaded) {
                loaded_entities += cell.spawned.len();
                if loaded_cells.len() < MAX_LISTED_CELLS {
                    loaded_cells.push(cell.name.clone());
                }
            }
        }
        PartitionStatus {
            manifest: self.manifest_id.clone(),
            cells: self.streamer.cell_count(),
            unloaded: counts.unloaded,
            loading: counts.loading,
            loaded: counts.loaded,
            unloading: counts.unloading,
            pinned: counts.pinned,
            in_flight: self.running.len(),
            loaded_entities,
            proxies: self.proxies.len(),
            sources: self.sources,
            started_last_tick: self.stats.started,
            applied_last_tick: self.stats.applied,
            bytes_last_tick: self.stats.bytes,
            total_loaded: self.stats.total_loaded,
            total_unloaded: self.stats.total_unloaded,
            total_failed: self.stats.total_failed,
            proxy_mesh_reads: self.stats.proxy_mesh_reads,
            layers: self
                .streamer
                .layers()
                .filter(|(layer, _, _)| *layer != Layer::DEFAULT)
                .map(|(_, name, enabled)| LayerStatus {
                    name: name.to_owned(),
                    enabled,
                })
                .collect(),
            loaded_cells,
            diagnostics: self.diagnostics.iter().cloned().collect(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Reconciling partitions with the world
// ---------------------------------------------------------------------------------------

fn discover(world: &mut World) -> Vec<(String, Entity, WorldPartition)> {
    let mut query = world.query::<(Entity, &WorldPartition, Option<&AuthoredId>)>();
    let mut found: Vec<_> = query
        .iter(world)
        .map(|(entity, component, id)| {
            (
                id.map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone()),
                entity,
                component.clone(),
            )
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Opens new partitions, reopens changed ones and tears down vanished ones.
pub(super) fn reconcile(world: &mut World, rt: &mut StreamingRuntime) {
    let found = discover(world);
    let current = |id: &str, entity: Entity, manifest: &str| {
        found
            .iter()
            .any(|(fid, fe, c)| fid == id && *fe == entity && c.manifest == manifest)
    };
    let stale: Vec<String> = rt
        .partitions
        .iter()
        .filter(|(id, p)| !current(id, p.entity, &p.manifest_id))
        .map(|(id, _)| id.clone())
        .collect();
    for id in stale {
        if let Some(mut partition) = rt.partitions.remove(&id) {
            teardown(world, &mut partition);
        }
    }
    rt.broken.retain(|id, (e, m)| current(id, *e, m));
    rt.errors.retain(|id, _| rt.broken.contains_key(id));
    for (id, entity, component) in found {
        if rt.partitions.contains_key(&id)
            || rt.broken.contains_key(&id)
            || component.manifest.is_empty()
        {
            continue;
        }
        let opened = if rt.partitions.len() >= MAX_PARTITIONS {
            Err(Error::new(
                "streaming.partition.limit",
                format!("at most {MAX_PARTITIONS} partitions stream at once"),
            ))
        } else {
            Partition::open(world, &id, entity, &component)
        };
        match opened {
            Ok(partition) => {
                rt.partitions.insert(id, partition);
            }
            Err(error) => {
                let message = clip(error.to_string());
                world
                    .resource_mut::<RuntimeLog>()
                    .0
                    .push(format!("streaming {id}: {message}"));
                rt.broken
                    .insert(id.clone(), (entity, component.manifest.clone()));
                rt.errors.insert(id, message);
            }
        }
    }
}

/// Despawns everything a partition owns that is still alive.
fn teardown(world: &mut World, partition: &mut Partition) {
    let ids: Vec<CellId> = partition.cells.keys().copied().collect();
    for id in ids {
        despawn_cell(world, partition, id);
    }
    let proxies: Vec<CellId> = partition.proxies.keys().copied().collect();
    for id in proxies {
        remove_proxy(world, partition, id);
    }
}

/// Despawns an entity only when the lookup still points at the entity we spawned.
fn despawn_owned(world: &mut World, id: &str, entity: Entity) {
    let owned = world.resource::<EntityLookup>().0.get(id) == Some(&entity);
    if owned {
        world.resource_mut::<EntityLookup>().0.remove(id);
    }
    if owned || world.get::<StreamedCell>(entity).is_some() {
        let _ = world.try_despawn(entity);
    }
}

fn despawn_cell(world: &mut World, partition: &mut Partition, id: CellId) -> usize {
    let Some(cell) = partition.cells.get_mut(&id) else {
        return 0;
    };
    let spawned = std::mem::take(&mut cell.spawned);
    let count = spawned.len();
    for (entity_id, entity) in spawned {
        despawn_owned(world, &entity_id, entity);
    }
    count
}

// ---------------------------------------------------------------------------------------
// The per-tick system
// ---------------------------------------------------------------------------------------

/// `pre_simulation`: reconcile, then step every partition.
pub(super) fn run(world: &mut World) {
    let mut events = Vec::new();
    world.resource_scope(|world, mut rt: Mut<StreamingRuntime>| {
        reconcile(world, &mut rt);
        let ids: Vec<String> = rt.partitions.keys().cloned().collect();
        for id in ids {
            let Some(partition) = rt.partitions.get_mut(&id) else {
                continue;
            };
            let Some(component) = world.get::<WorldPartition>(partition.entity).cloned() else {
                continue;
            };
            if let Err(error) = component.check() {
                let message = error.to_string();
                if partition.diagnostics.back() != Some(&clip(message.clone())) {
                    partition.note(world, message);
                }
                continue;
            }
            step(world, partition, &component, &mut events);
        }
        publish_status(world, &rt);
    });
    emit(world, events);
}

pub(super) fn publish_status(world: &mut World, rt: &StreamingRuntime) {
    let mut map: BTreeMap<String, PartitionStatus> = rt
        .partitions
        .iter()
        .map(|(id, p)| (id.clone(), p.status()))
        .collect();
    for (id, message) in &rt.errors {
        let (_, manifest) = &rt.broken[id];
        map.insert(
            id.clone(),
            PartitionStatus {
                manifest: manifest.clone(),
                diagnostics: vec![message.clone()],
                ..PartitionStatus::default()
            },
        );
    }
    world.insert_resource(StreamingStatus(map));
}

pub(super) fn emit(world: &mut World, events: Vec<EngineEvent>) {
    if events.is_empty() {
        return;
    }
    if let Err(error) = crate::emit_events(world, events) {
        world
            .resource_mut::<RuntimeLog>()
            .0
            .push(format!("streaming: events dropped: {error}"));
    }
}

fn step(
    world: &mut World,
    partition: &mut Partition,
    component: &WorldPartition,
    events: &mut Vec<EngineEvent>,
) {
    partition.stats.started = 0;
    partition.stats.applied = 0;
    partition.stats.bytes = 0;
    let origin = origin_of(world);
    sync_config(partition, component);
    let sources = collect_sources(world, component, origin);
    partition.sources = sources.len();

    let plan = partition.streamer.plan(&sources);
    for id in &plan.cancel_loads {
        partition.running.remove(id);
        partition.ready.remove(id);
    }
    for id in &plan.start_unloads {
        unload(world, partition, *id, events);
    }
    for id in &plan.start_loads {
        begin_load(world, partition, *id, component.synchronous);
    }
    partition.stats.started = plan.start_loads.len();
    drain_results(partition);
    apply_ready(world, partition, component, origin, events);
    sync_proxies(world, partition, component, &sources, origin);
}

fn sync_config(partition: &mut Partition, component: &WorldPartition) {
    partition.streamer.set_limits(limits(component));
    for layer in &component.layers {
        let _ = partition.streamer.layer(&layer.name);
    }
    let layers: Vec<(Layer, String)> = partition
        .streamer
        .layers()
        .map(|(layer, name, _)| (layer, name.to_owned()))
        .collect();
    for (layer, name) in layers {
        partition
            .streamer
            .set_layer_enabled(layer, component.layer_enabled(&name));
    }
}

fn absolute_position(world: &World, entity: Entity, origin: [f64; 3]) -> Option<[f64; 3]> {
    let local = crate::transforms::world_transform(world, entity)
        .map(|t| t.position())
        .ok()
        .or_else(|| world.get::<Transform>(entity).map(|t| t.position))?;
    Some(std::array::from_fn(|i| origin[i] + f64::from(local[i])))
}

fn collect_sources(world: &mut World, component: &WorldPartition, origin: [f64; 3]) -> Vec<Source> {
    let mut sources = Vec::new();
    let mut query = world.query::<(Entity, &StreamingSource)>();
    let mut found: Vec<(Entity, StreamingSource)> =
        query.iter(world).map(|(e, s)| (e, s.clone())).collect();
    found.sort_by_key(|(entity, _)| entity.to_bits());
    for (entity, source) in found {
        if !source.enabled || source.check().is_err() {
            continue;
        }
        if let Some(position) = absolute_position(world, entity, origin) {
            sources.push(Source {
                position,
                priority: source.priority,
                load_radius: component.load_radius * source.radius_scale,
                unload_radius: component.unload_radius * source.radius_scale,
            });
        }
    }
    if component.camera_source {
        let mut query = world.query::<(Entity, &crate::rendering3d::Camera3D)>();
        let mut cameras: Vec<Entity> = query
            .iter(world)
            .filter(|(_, camera)| camera.active)
            .map(|(entity, _)| entity)
            .collect();
        cameras.sort_by_key(|entity| entity.to_bits());
        for entity in cameras {
            if let Some(position) = absolute_position(world, entity, origin) {
                sources.push(Source {
                    position,
                    priority: 0,
                    load_radius: component.load_radius,
                    unload_radius: component.unload_radius,
                });
            }
        }
    }
    sources
}

// ---------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------

fn read_cell(root: &Path, file: &str) -> std::result::Result<Scene, String> {
    crate::load_scene(root, Path::new(file)).map_err(|e| e.to_string())
}

fn begin_load(world: &mut World, partition: &mut Partition, id: CellId, synchronous: bool) {
    let root: PathBuf = world.resource::<ProjectResource>().0.root.clone();
    let Some(file) = partition.cells.get(&id).map(|c| c.file.clone()) else {
        return;
    };
    partition.next_generation += 1;
    let generation = partition.next_generation;
    partition.running.insert(id, generation);
    if synchronous {
        let scene = read_cell(&root, &file);
        partition.ready.insert(
            id,
            JobResult {
                id,
                generation,
                scene,
            },
        );
        return;
    }
    let tx = partition.tx.clone();
    let spawned = std::thread::Builder::new()
        .name("engine-streaming".into())
        .spawn(move || {
            let scene = catch_unwind(AssertUnwindSafe(|| read_cell(&root, &file)))
                .unwrap_or_else(|_| Err("cell reader panicked".into()));
            let _ = tx.send(JobResult {
                id,
                generation,
                scene,
            });
        });
    if spawned.is_err() {
        partition.ready.insert(
            id,
            JobResult {
                id,
                generation,
                scene: Err("could not start a reader thread".into()),
            },
        );
    }
}

fn drain_results(partition: &mut Partition) {
    let Ok(receiver) = partition.rx.lock() else {
        return;
    };
    while let Ok(result) = receiver.try_recv() {
        if partition.running.get(&result.id) == Some(&result.generation) {
            partition.ready.insert(result.id, result);
        }
    }
}

fn apply_ready(
    world: &mut World,
    partition: &mut Partition,
    component: &WorldPartition,
    origin: [f64; 3],
    events: &mut Vec<EngineEvent>,
) {
    let mut order: Vec<CellId> = partition.ready.keys().copied().collect();
    order.sort_by_key(|id| (partition.streamer.rank(*id), *id));
    let limits = partition.streamer.limits();
    let mut budget = TickBudget::new(
        limits.cells_per_tick.min(component.loads_per_tick as usize),
        limits.bytes_per_tick,
    );
    for id in order {
        let bytes = partition.streamer.bytes(id).unwrap_or(1);
        if !budget.try_take(bytes) {
            break;
        }
        let Some(result) = partition.ready.remove(&id) else {
            continue;
        };
        partition.running.remove(&id);
        let _ = complete(world, partition, id, result.scene, origin, events);
        partition.stats.bytes += bytes;
    }
}

/// Applies a finished read (or its failure) to a `Loading` cell.
pub(super) fn complete(
    world: &mut World,
    partition: &mut Partition,
    id: CellId,
    scene: std::result::Result<Scene, String>,
    origin: [f64; 3],
    events: &mut Vec<EngineEvent>,
) -> std::result::Result<(), String> {
    let outcome = scene.and_then(|scene| apply_cell(world, partition, id, scene, origin));
    let label = partition.cell_label(id);
    match outcome {
        Ok(()) => {
            let _ = partition.streamer.complete_load(id);
            partition.stats.applied += 1;
            partition.stats.total_loaded += 1;
            events.push(cell_event(partition, id, EVENT_CELL_LOADED, None));
            Ok(())
        }
        Err(message) => {
            let _ = partition.streamer.fail_load(id);
            partition.stats.total_failed += 1;
            partition.note(world, format!("cell {label}: {message}"));
            events.push(cell_event(partition, id, EVENT_CELL_FAILED, Some(&message)));
            Err(message)
        }
    }
}

fn cell_event(partition: &Partition, id: CellId, name: &str, error: Option<&str>) -> EngineEvent {
    let mut payload = json!({
        "partition": partition.id,
        "cell": id.key.0,
        "layer": partition.cells.get(&id).map(|c| c.layer.as_str()).unwrap_or_default(),
        "entities": partition.cells.get(&id).map_or(0, |c| c.spawned.len()),
    });
    if let Some(error) = error {
        payload["error"] = json!(clip(error.to_owned()));
    }
    EngineEvent {
        name: name.into(),
        source: Some(partition.entity),
        target: None,
        payload,
    }
}

/// Spawns a cell's entities: roots are shifted from cell-local to world-local space in f64.
fn apply_cell(
    world: &mut World,
    partition: &mut Partition,
    id: CellId,
    scene: Scene,
    origin: [f64; 3],
) -> std::result::Result<(), String> {
    if scene.entities.len() > MAX_CELL_ENTITIES {
        return Err(format!(
            "{} entities exceed the cell limit",
            scene.entities.len()
        ));
    }
    // The same semantic checks as project validation, so a cell holding an invalid feature
    // component (missing terrain asset, empty tileset, ...) fails locally and spawns nothing.
    {
        let project = world.resource::<ProjectResource>().0.clone();
        project
            .validate_feature_components(&scene)
            .map_err(|e| e.to_string())?;
    }
    {
        let lookup = world.resource::<EntityLookup>();
        let own: BTreeSet<&str> = scene.entities.iter().map(|e| e.id.as_str()).collect();
        for entity in &scene.entities {
            if lookup.0.contains_key(&entity.id) {
                return Err(format!(
                    "entity id `{}` already exists in the world",
                    entity.id
                ));
            }
            if let Some(parent) = &entity.parent
                && !own.contains(parent.as_str())
                && !lookup.0.contains_key(parent)
            {
                return Err(format!(
                    "entity `{}` has unknown parent `{parent}`",
                    entity.id
                ));
            }
        }
    }
    let cell_origin = partition.streamer.grid().origin(id.key);
    let shift: [f64; 3] = std::array::from_fn(|i| cell_origin[i] - origin[i]);
    let (layer, key) = (partition.cells[&id].layer.clone(), id.key.0);
    let mut spawned = Vec::with_capacity(scene.entities.len());
    let mut authored_all = Vec::with_capacity(scene.entities.len());
    for mut authored in scene.entities {
        if authored.parent.is_none()
            && let Some(transform) = authored.components.transform.as_mut()
        {
            for (position, shift) in transform.position.iter_mut().zip(shift) {
                *position = (f64::from(*position) + shift) as f32;
            }
        }
        let entity = crate::runtime::spawn(world, &authored);
        world.entity_mut(entity).insert(StreamedCell {
            partition: partition.id.clone(),
            key,
            layer: layer.clone(),
            proxy: false,
        });
        spawned.push((authored.id.clone(), entity));
        authored_all.push(authored);
    }
    if authored_all.iter().any(|e| !e.components.custom.is_empty()) {
        let catalog = world
            .resource::<ProjectResource>()
            .0
            .component_catalog
            .clone();
        let hydrate_scene = Scene {
            schema: 1,
            id: format!("streaming:{}", partition.id),
            entities: authored_all,
            source: PathBuf::new(),
        };
        if let Err(error) = crate::catalog::hydrate(world, &hydrate_scene, &catalog) {
            for (entity_id, entity) in spawned {
                despawn_owned(world, &entity_id, entity);
            }
            return Err(format!("component hydration failed: {error}"));
        }
    }
    if let Some(cell) = partition.cells.get_mut(&id) {
        cell.spawned = spawned;
    }
    Ok(())
}

fn unload(world: &mut World, partition: &mut Partition, id: CellId, events: &mut Vec<EngineEvent>) {
    let count = despawn_cell(world, partition, id);
    let _ = partition.streamer.complete_unload(id);
    partition.stats.total_unloaded += 1;
    let mut event = cell_event(partition, id, EVENT_CELL_UNLOADED, None);
    event.payload["entities"] = json!(count);
    events.push(event);
}

// ---------------------------------------------------------------------------------------
// HLOD proxies
// ---------------------------------------------------------------------------------------

fn remove_proxy(world: &mut World, partition: &mut Partition, id: CellId) {
    if let Some((entity_id, entity)) = partition.proxies.remove(&id) {
        despawn_owned(world, &entity_id, entity);
    }
}

fn spawn_proxy(
    world: &mut World,
    partition: &mut Partition,
    id: CellId,
    file: &str,
    origin: [f64; 3],
) {
    partition.mesh_clock += 1;
    let clock = partition.mesh_clock;
    let mesh = match partition.mesh_cache.get_mut(file) {
        Some((mesh, used)) => {
            *used = clock;
            mesh.clone()
        }
        None => {
            let root = world.resource::<ProjectResource>().0.root.clone();
            partition.stats.proxy_mesh_reads += 1;
            let loaded = crate::confined_file(&root, Path::new(file))
                .and_then(|path| crate::content::load_mesh(&path));
            match loaded {
                Ok(mesh) => {
                    let mesh = Arc::new(mesh);
                    evict_unshown_meshes(partition);
                    partition
                        .mesh_cache
                        .insert(file.to_owned(), (mesh.clone(), clock));
                    mesh
                }
                Err(error) => {
                    partition.bad_proxies.insert(id);
                    let label = partition.cell_label(id);
                    partition.note(world, format!("cell {label}: HLOD proxy `{file}`: {error}"));
                    return;
                }
            }
        }
    };
    let cell_origin = partition.streamer.grid().origin(id.key);
    let name = format!("hlod:{}:{}", partition.id, partition.cell_label(id));
    let transform = Transform {
        position: std::array::from_fn(|i| (cell_origin[i] - origin[i]) as f32),
        ..Transform::default()
    };
    let authored = SceneEntity {
        id: name.clone(),
        name: name.clone(),
        parent: None,
        components: Components {
            transform: Some(transform),
            mesh_renderer: Some(MeshRenderer {
                version: 1,
                mesh: String::new(),
                texture: None,
            }),
            ..Components::default()
        },
    };
    if world.resource::<EntityLookup>().0.contains_key(&name) {
        partition.bad_proxies.insert(id);
        return;
    }
    let entity = crate::runtime::spawn(world, &authored);
    let layer = partition.cells[&id].layer.clone();
    world.entity_mut(entity).insert((
        GeometryOverride(mesh),
        StreamedCell {
            partition: partition.id.clone(),
            key: id.key.0,
            layer,
            proxy: true,
        },
    ));
    partition.proxies.insert(id, (name, entity));
}

/// Makes room for one more cached proxy mesh by dropping least recently used entries, never
/// those of currently shown proxies (the cache may exceed its soft size only by shown proxies).
fn evict_unshown_meshes(partition: &mut Partition) {
    while partition.mesh_cache.len() >= MAX_MESH_CACHE {
        let shown: BTreeSet<&str> = partition
            .proxies
            .keys()
            .filter_map(|id| partition.cells.get(id)?.hlod.as_deref())
            .collect();
        let victim = partition
            .mesh_cache
            .iter()
            .filter(|(file, _)| !shown.contains(file.as_str()))
            .min_by_key(|(_, (_, used))| *used)
            .map(|(file, _)| file.clone());
        match victim {
            Some(file) => {
                partition.mesh_cache.remove(&file);
            }
            None => break,
        }
    }
}

/// Shows the proxies of cells that are not loaded (within `hlod_radius` of a source), at most
/// `hlod_max_proxies` of them, nearest to a source first, and hides the rest.
fn sync_proxies(
    world: &mut World,
    partition: &mut Partition,
    component: &WorldPartition,
    sources: &[Source],
    origin: [f64; 3],
) {
    let mut wanted: Vec<(f64, CellId, String)> = Vec::new();
    for (id, cell) in &partition.cells {
        let Some(file) = cell.hlod.clone() else {
            continue;
        };
        if !component.hlod
            || !partition.streamer.layer_enabled(id.layer)
            || partition.streamer.state(*id) == Some(CellState::Loaded)
            || partition.bad_proxies.contains(id)
        {
            continue;
        }
        let distance = sources
            .iter()
            .map(|s| partition.streamer.grid().distance(s.position, id.key))
            .fold(f64::INFINITY, f64::min);
        if component.hlod_radius > 0.0 && distance > component.hlod_radius {
            continue;
        }
        wanted.push((distance, *id, file));
    }
    wanted.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    wanted.truncate(component.hlod_max_proxies as usize);
    let keep: BTreeSet<CellId> = wanted.iter().map(|(_, id, _)| *id).collect();
    let stale: Vec<CellId> = partition
        .proxies
        .keys()
        .filter(|id| !keep.contains(id))
        .copied()
        .collect();
    for id in stale {
        remove_proxy(world, partition, id);
    }
    let mut spawns = 0;
    for (_, id, file) in wanted {
        if spawns >= HLOD_SPAWNS_PER_TICK {
            break;
        }
        if !partition.proxies.contains_key(&id) {
            spawns += 1;
            spawn_proxy(world, partition, id, &file, origin);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Entry points for the native API
// ---------------------------------------------------------------------------------------

/// Runs `f` with the runtime resource taken out of the world, after reconciling partitions.
pub(super) fn with_runtime<R>(
    world: &mut World,
    f: impl FnOnce(&mut World, &mut StreamingRuntime) -> R,
) -> R {
    world.resource_scope(|world, mut rt: Mut<StreamingRuntime>| {
        reconcile(world, &mut rt);
        let result = f(world, &mut rt);
        publish_status(world, &rt);
        result
    })
}

/// Reads and applies one cell right now, outside every budget.
pub(super) fn force_load(
    world: &mut World,
    partition: &mut Partition,
    id: CellId,
) -> Result<Vec<EngineEvent>> {
    let mut events = Vec::new();
    match partition.streamer.state(id) {
        Some(CellState::Loaded) => return Ok(events),
        Some(CellState::Unloaded) => {
            partition
                .streamer
                .force_begin_load(id)
                .map_err(|e| Error::new("streaming.cell.state", e.0))?;
        }
        Some(CellState::Loading) => {
            partition.running.remove(&id);
            partition.ready.remove(&id);
        }
        _ => {
            return Err(Error::new(
                "streaming.cell.state",
                "the cell is not in a loadable state",
            ));
        }
    }
    let root = world.resource::<ProjectResource>().0.root.clone();
    let file = partition.cells[&id].file.clone();
    let scene = read_cell(&root, &file);
    let origin = origin_of(world);
    let result = complete(world, partition, id, scene, origin, &mut events);
    remove_proxy(world, partition, id);
    match result {
        Ok(()) => Ok(events),
        Err(message) => {
            emit(world, events);
            Err(Error::new("streaming.cell.load", message))
        }
    }
}
