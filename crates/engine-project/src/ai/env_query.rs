//! Environment queries in the runtime: the `engine.EnvironmentQueries` component
//! (the authored templates), a time-sliced job service and the world host (navmesh,
//! physics line of sight, perceived actors, smart object slots).
//!
//! Queries run from natives ([`run_query`] synchronously, [`submit_query`] and
//! [`take_query`] across ticks) and from trees through the built-in
//! `engine.run_query` native task.
use super::{
    perception::{PerceptionSensor, PerceptionStimulus, Perceptions},
    smart_object,
};
use crate::{
    ComponentDescriptor, Error, Result,
    runtime::{AuthoredId, ProjectResource},
    simulation::{
        NavigationAgent,
        physics::{Dimension, QueryFilter, SensorFilter, ray_cast},
    },
};
use bevy_ecs::prelude::*;
use engine_ai::{
    BbValue, Blackboard,
    env_query::{
        ActorSource, Item, Progress, QueryContext, QueryHost, QueryResult, QueryRun, QueryTemplate,
    },
    perception::AffiliationFilter,
};
use engine_core::EngineApp;
use engine_navigation::{NavMesh, NavPath, QuerySettings};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

pub const ENVIRONMENT_QUERIES: &str = "engine.EnvironmentQueries";
/// Templates one component may carry.
pub const MAX_TEMPLATES: usize = 32;
/// Queries in flight (or finished and not yet taken) at once.
pub const MAX_JOBS: usize = 64;
/// Evaluation units all running queries together may spend per tick.
pub const BUDGET_PER_TICK: usize = 4096;
/// Units a new query spends immediately, so cheap queries finish in the call.
const IMMEDIATE_BUDGET: usize = 256;
/// Ticks a finished, untaken result is kept.
const RESULT_TTL_TICKS: u64 = 600;
/// Recent queries listed in telemetry.
pub const STATUS_MAX_RECENT: usize = 8;
const MAX_MESHES: usize = 4;

/// Query templates available to the whole scene (names are unique scene-wide).
#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EnvironmentQueries {
    pub queries: Vec<QueryTemplate>,
}

impl EnvironmentQueries {
    pub(crate) fn check(&self) -> Result<()> {
        if self.queries.len() > MAX_TEMPLATES {
            return Err(Error::new(
                "env_query.value.invalid",
                format!("more than {MAX_TEMPLATES} query templates"),
            ));
        }
        let mut names = std::collections::BTreeSet::new();
        for template in &self.queries {
            template.validate().map_err(|m| {
                Error::new(
                    "env_query.value.invalid",
                    format!("query {:?}: {m}", template.name),
                )
            })?;
            if !names.insert(template.name.as_str()) {
                return Err(Error::new(
                    "env_query.duplicate",
                    format!("query name {:?} repeats", template.name),
                ));
            }
        }
        Ok(())
    }
}

/// Catalog descriptor.
pub fn descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: ENVIRONMENT_QUERIES.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "queries": {"type":"array","maxItems":MAX_TEMPLATES,
                "description":"Query templates: {name, generator, tests, select, project_to_navmesh, snap_distance, navmesh}. See docs/ai-environment-queries.md.",
                "items":{"type":"object"}}
        }}),
        defaults: serde_json::to_value(EnvironmentQueries::default()).expect("finite defaults"),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}

/// Handle of a submitted query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct QueryHandle(pub u64);

/// State of a submitted query.
#[derive(Clone, Debug, PartialEq)]
pub enum Poll {
    Pending,
    /// Finished; the job is released.
    Ready(QueryResult),
    /// The query could not run; the job is released.
    Failed(String),
    /// No such job (never submitted, cancelled, taken or expired).
    Unknown,
}

struct Job {
    querier: Entity,
    name: String,
    mesh: Option<Arc<NavMesh>>,
    run: QueryRun,
    outcome: Option<std::result::Result<QueryResult, String>>,
    finished_tick: u64,
}

/// One finished query, for telemetry.
#[derive(Clone, Debug, Serialize)]
pub struct RecentQuery {
    pub querier: String,
    pub query: String,
    pub items: usize,
    pub best_score: Option<f32>,
    pub best_position: Option<[f32; 3]>,
    pub generated: usize,
    pub rejected: usize,
    pub cost: usize,
    pub steps: u32,
}

/// The query job service.
#[derive(Resource, Default)]
pub struct EnvQueries {
    jobs: BTreeMap<u64, Job>,
    next: u64,
    meshes: BTreeMap<String, Arc<NavMesh>>,
    tick: u64,
    completed: u64,
    failed: u64,
    recent: VecDeque<RecentQuery>,
}

pub(super) fn register(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<EnvironmentQueries>(app, ENVIRONMENT_QUERIES, 1)?;
    app.world_mut().insert_resource(EnvQueries::default());
    Ok(())
}

fn authored(world: &World, entity: Entity) -> String {
    world
        .get::<AuthoredId>(entity)
        .map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone())
}

fn unavailable() -> Error {
    Error::new(
        "env_query.unavailable",
        "environment queries need a project runtime",
    )
}

/// The template called `name` in any `engine.EnvironmentQueries`, by entity order.
pub fn find_template(world: &mut World, name: &str) -> Option<Arc<QueryTemplate>> {
    let mut holders: Vec<(Entity, &EnvironmentQueries)> = world
        .query::<(Entity, &EnvironmentQueries)>()
        .iter(world)
        .collect();
    holders.sort_by_key(|(e, _)| *e);
    holders
        .into_iter()
        .flat_map(|(_, c)| c.queries.iter())
        .find(|t| t.name == name)
        .map(|t| Arc::new(t.clone()))
}

fn position_of(world: &World, entity: Entity) -> Option<[f32; 3]> {
    crate::world_transform(world, entity)
        .ok()
        .map(|t| t.position())
}

fn forward_of(world: &World, entity: Entity) -> [f32; 3] {
    let local = world
        .get::<PerceptionSensor>(entity)
        .map_or([0.0, 0.0, -1.0], |s| s.forward);
    crate::world_transform(world, entity)
        .ok()
        .map(|t| {
            glam::Mat4::from_cols_array_2d(&t.matrix)
                .transform_vector3(Vec3::from(local))
                .normalize_or_zero()
                .to_array()
        })
        .unwrap_or([0.0, 0.0, -1.0])
}

fn blackboard_of(world: &World, entity: Entity) -> Blackboard {
    world
        .get_resource::<super::BehaviorTrees>()
        .and_then(|t| t.blackboard(entity))
        .or_else(|| {
            world
                .get_resource::<super::StateTrees>()
                .and_then(|t| t.blackboard(entity))
        })
        .cloned()
        .unwrap_or_default()
}

/// The navmesh asset a query for `querier` should use.
fn navmesh_id(world: &World, querier: Entity, template: &QueryTemplate) -> Option<String> {
    template
        .navmesh
        .clone()
        .or_else(|| world.get::<NavigationAgent>(querier)?.navmesh.clone())
        .or_else(|| {
            world
                .get::<super::BehaviorTreeAgent>(querier)?
                .navmesh
                .clone()
        })
        .or_else(|| world.get::<super::StateTreeAgent>(querier)?.navmesh.clone())
}

struct WorldQueryHost<'a> {
    world: &'a World,
    querier: Entity,
    mesh: Option<Arc<NavMesh>>,
}

impl QueryHost for WorldQueryHost<'_> {
    fn project(&mut self, position: [f32; 3], snap: f32) -> Option<[f32; 3]> {
        let mesh = self.mesh.as_ref()?;
        let settings = QuerySettings {
            max_distance: snap,
            ..QuerySettings::default()
        };
        mesh.nearest(position, settings)
            .ok()
            .flatten()
            .map(|l| l.position)
    }

    fn path_length(&mut self, from: [f32; 3], to: [f32; 3]) -> Option<f32> {
        let mesh = self.mesh.as_ref()?;
        match mesh.find_path(from, to, QuerySettings::default()) {
            Ok(NavPath::Reached { points, .. }) => Some(
                points
                    .windows(2)
                    .map(|w| Vec3::from(w[0]).distance(Vec3::from(w[1])))
                    .sum(),
            ),
            _ => None,
        }
    }

    fn line_of_sight(&mut self, from: [f32; 3], to: [f32; 3]) -> bool {
        let origin = Vec3::from(from);
        let delta = Vec3::from(to) - origin;
        let distance = delta.length();
        if distance < 1e-3 {
            return true;
        }
        let filter = QueryFilter {
            exclude: vec![self.querier],
            sensors: SensorFilter::Exclude,
            ..QueryFilter::default()
        };
        // Stop a hair short so the surface an item rests on does not block it.
        !matches!(
            ray_cast(
                self.world,
                Dimension::Three,
                origin,
                delta / distance,
                (distance - 0.01).max(0.0),
                &filter
            ),
            Ok(Some(_))
        )
    }

    fn actors(&mut self, source: ActorSource, filter: AffiliationFilter, max: usize) -> Vec<Item> {
        let Some(perceptions) = self.world.get_resource::<Perceptions>() else {
            return Vec::new();
        };
        let mut items = match source {
            ActorSource::Perceived => {
                // One item per subject: the strongest record.
                let mut best: BTreeMap<u64, engine_ai::perception::Known> = BTreeMap::new();
                for known in perceptions.known(self.querier) {
                    if known.subject == engine_ai::perception::ANONYMOUS
                        || !filter.contains(known.affiliation)
                    {
                        continue;
                    }
                    let entry = best.entry(known.subject).or_insert(known);
                    if (known.sensed, known.strength) > (entry.sensed, entry.strength) {
                        *entry = known;
                    }
                }
                best.into_values()
                    .map(|k| Item {
                        position: k.position,
                        subject: Some(k.subject),
                        slot: None,
                    })
                    .collect::<Vec<_>>()
            }
            ActorSource::Stimuli => {
                let team = self
                    .world
                    .get::<PerceptionSensor>(self.querier)
                    .map_or(0, |s| s.team);
                let mut found: Vec<(Entity, u32)> = self
                    .world
                    .iter_entities()
                    .filter_map(|e| {
                        let s = e.get::<PerceptionStimulus>()?;
                        (s.enabled && e.id() != self.querier).then_some((e.id(), s.team))
                    })
                    .filter(|(_, other)| filter.contains(perceptions.affiliation(team, *other)))
                    .collect();
                found.sort_by_key(|(e, _)| *e);
                found
                    .into_iter()
                    .filter_map(|(e, _)| {
                        Some(Item {
                            position: position_of(self.world, e)?,
                            subject: Some(e.to_bits()),
                            slot: None,
                        })
                    })
                    .collect()
            }
        };
        items.truncate(max);
        items
    }

    fn smart_objects(&mut self, tags: &[String], free_only: bool, max: usize) -> Vec<Item> {
        smart_object::find_slots_in(self.world, tags, free_only)
            .into_iter()
            .take(max)
            .map(|s| Item {
                position: s.position,
                subject: Some(s.entity.to_bits()),
                slot: Some(s.slot.slot),
            })
            .collect()
    }
}

fn mesh_for(
    world: &World,
    service: &mut EnvQueries,
    querier: Entity,
    template: &QueryTemplate,
) -> Result<Option<Arc<NavMesh>>> {
    let Some(id) = navmesh_id(world, querier, template) else {
        if template.needs_navmesh() {
            return Err(Error::new(
                "env_query.navmesh.missing",
                format!(
                    "query {:?} needs a navmesh: set `navmesh` on the template or on the querier's navigation agent",
                    template.name
                ),
            ));
        }
        return Ok(None);
    };
    if !template.needs_navmesh() {
        return Ok(None);
    }
    if let Some(mesh) = service.meshes.get(&id) {
        return Ok(Some(Arc::clone(mesh)));
    }
    if service.meshes.len() >= MAX_MESHES {
        service.meshes.clear();
    }
    let project = &world.resource::<ProjectResource>().0;
    let mesh = crate::simulation::load_navmesh(project, &id)?;
    service.meshes.insert(id, Arc::clone(&mesh));
    Ok(Some(mesh))
}

fn with_service<T>(
    world: &mut World,
    run: impl FnOnce(&mut World, &mut EnvQueries) -> T,
) -> Option<T> {
    let mut service = world.remove_resource::<EnvQueries>()?;
    let out = run(world, &mut service);
    world.insert_resource(service);
    Some(out)
}

fn record(world: &World, service: &mut EnvQueries, job: &Job, result: &QueryResult) {
    if service.recent.len() == STATUS_MAX_RECENT {
        service.recent.pop_front();
    }
    service.recent.push_back(RecentQuery {
        querier: authored(world, job.querier),
        query: job.name.clone(),
        items: result.items.len(),
        best_score: result.items.first().map(|s| s.score),
        best_position: result.items.first().map(|s| s.item.position),
        generated: result.generated,
        rejected: result.rejected,
        cost: result.cost,
        steps: result.steps,
    });
}

fn advance(world: &World, service: &mut EnvQueries, job: &mut Job, budget: usize) -> usize {
    let mut host = WorldQueryHost {
        world,
        querier: job.querier,
        mesh: job.mesh.clone(),
    };
    let before = job.run.cost();
    match job.run.step(budget, &mut host) {
        Progress::Running => {}
        Progress::Done(result) => {
            service.completed += 1;
            record(world, service, job, &result);
            job.outcome = Some(Ok(result));
            job.finished_tick = service.tick;
        }
    }
    job.run.cost().saturating_sub(before).max(1)
}

/// Starts a query for `querier` and runs its first slice. Take the result with
/// [`take_query`].
///
/// # Errors
/// An unknown query name, a missing target or navmesh, a bad template reference, or
/// too many queries in flight.
pub fn submit_query(
    world: &mut World,
    querier: Entity,
    name: &str,
    target: Option<[f32; 3]>,
) -> Result<QueryHandle> {
    let template = find_template(world, name).ok_or_else(|| {
        Error::new(
            "env_query.unknown",
            format!("no query template named {name:?}"),
        )
    })?;
    let position = position_of(world, querier).ok_or_else(|| {
        Error::new(
            "env_query.querier.invalid",
            "the querier needs a valid transform",
        )
    })?;
    let context = QueryContext {
        querier: position,
        forward: forward_of(world, querier),
        target,
        blackboard: blackboard_of(world, querier),
    };
    with_service(world, |world, service| -> Result<QueryHandle> {
        if service.jobs.len() >= MAX_JOBS {
            return Err(Error::new(
                "env_query.capacity",
                format!("more than {MAX_JOBS} queries in flight"),
            ));
        }
        let mesh = mesh_for(world, service, querier, &template)?;
        let run = QueryRun::new(template, context)
            .map_err(|e| Error::new("env_query.start", e.to_string()))?;
        let id = service.next;
        service.next += 1;
        let mut job = Job {
            querier,
            name: name.to_owned(),
            mesh,
            run,
            outcome: None,
            finished_tick: 0,
        };
        advance(world, service, &mut job, IMMEDIATE_BUDGET);
        service.jobs.insert(id, job);
        Ok(QueryHandle(id))
    })
    .ok_or_else(unavailable)?
}

/// Polls a submitted query; a finished one is returned once and released.
pub fn take_query(world: &mut World, handle: QueryHandle) -> Poll {
    with_service(world, |_, service| match service.jobs.get(&handle.0) {
        None => Poll::Unknown,
        Some(job) if job.outcome.is_none() => Poll::Pending,
        Some(_) => match service.jobs.remove(&handle.0).and_then(|j| j.outcome) {
            Some(Ok(result)) => Poll::Ready(result),
            Some(Err(message)) => Poll::Failed(message),
            None => Poll::Unknown,
        },
    })
    .unwrap_or(Poll::Unknown)
}

/// Drops a query (running or finished).
pub fn cancel_query(world: &mut World, handle: QueryHandle) {
    with_service(world, |_, service| {
        service.jobs.remove(&handle.0);
    });
}

/// Runs a query to completion now. Prefer [`submit_query`] for big queries.
///
/// # Errors
/// As [`submit_query`].
pub fn run_query(
    world: &mut World,
    querier: Entity,
    name: &str,
    target: Option<[f32; 3]>,
) -> Result<QueryResult> {
    let handle = submit_query(world, querier, name, target)?;
    // Spend the rest of the query synchronously.
    with_service(world, |world, service| {
        if let Some(mut job) = service.jobs.remove(&handle.0) {
            while job.outcome.is_none() {
                advance(world, service, &mut job, usize::MAX);
            }
            service.jobs.insert(handle.0, job);
        }
    });
    match take_query(world, handle) {
        Poll::Ready(result) => Ok(result),
        Poll::Failed(message) => Err(Error::new("env_query.failed", message)),
        _ => Err(Error::new("env_query.failed", "the query vanished")),
    }
}

/// Spends this tick's budget on running queries (oldest first) and expires results
/// nobody took.
pub fn step(world: &mut World) -> Result<()> {
    with_service(world, |world, service| {
        service.tick += 1;
        let mut budget = BUDGET_PER_TICK;
        let ids: Vec<u64> = service.jobs.keys().copied().collect();
        for id in ids {
            let Some(mut job) = service.jobs.remove(&id) else {
                continue;
            };
            if job.outcome.is_none() && world.get_entity(job.querier).is_err() {
                service.failed += 1;
                job.outcome = Some(Err("the querier no longer exists".to_owned()));
                job.finished_tick = service.tick;
            }
            while job.outcome.is_none() && budget > 0 {
                budget = budget.saturating_sub(advance(world, service, &mut job, budget));
            }
            let expired = job.outcome.is_some()
                && service.tick.saturating_sub(job.finished_tick) > RESULT_TTL_TICKS;
            if !expired {
                service.jobs.insert(id, job);
            }
        }
    });
    Ok(())
}

/// Telemetry of the query service.
#[derive(Clone, Debug, Serialize)]
pub struct EnvQueryStatus {
    pub templates: usize,
    pub running: usize,
    pub finished_untaken: usize,
    pub completed: u64,
    pub failed: u64,
    pub recent: Vec<RecentQuery>,
}

/// Bounded telemetry; `None` without templates or queries.
pub fn status(world: &World) -> Option<EnvQueryStatus> {
    let service = world.get_resource::<EnvQueries>()?;
    let templates: usize = world
        .iter_entities()
        .filter_map(|e| e.get::<EnvironmentQueries>().map(|c| c.queries.len()))
        .sum();
    if templates == 0 && service.completed == 0 && service.jobs.is_empty() {
        return None;
    }
    Some(EnvQueryStatus {
        templates,
        running: service
            .jobs
            .values()
            .filter(|j| j.outcome.is_none())
            .count(),
        finished_untaken: service
            .jobs
            .values()
            .filter(|j| j.outcome.is_some())
            .count(),
        completed: service.completed,
        failed: service.failed,
        recent: service.recent.iter().cloned().collect(),
    })
}

/// Writes the best item of a finished query into blackboard keys: `position_key`
/// (`vec3`) and `entity_key` (`entity`, the subject's authored id). Clears the keys
/// when there is no item. Returns whether an item exists.
pub fn write_best(
    world: &World,
    result: &QueryResult,
    board: &mut Blackboard,
    position_key: Option<&str>,
    entity_key: Option<&str>,
) -> bool {
    let best = result.items.first();
    if let Some(key) = position_key {
        let _ = match best {
            Some(s) => board.set(key, BbValue::Vec3(s.item.position)).map(|_| ()),
            None => board.clear(key).map(|_| ()),
        };
    }
    if let Some(key) = entity_key {
        let subject = best
            .and_then(|s| s.item.subject)
            .map(Entity::from_bits)
            .filter(|e| world.get_entity(*e).is_ok());
        let _ = match subject {
            Some(e) => board
                .set(key, BbValue::Entity(authored(world, e)))
                .map(|_| ()),
            None => board.clear(key).map(|_| ()),
        };
    }
    best.is_some()
}
