//! JSON-shaped physics queries and debug summaries for tools. [`query_scene`] and
//! [`debug_scene`] load a project's scene into a headless [`Runtime`], advance it a bounded
//! number of ticks and answer from the resulting physics state. No native scripts are
//! registered, so a scene whose behaviour needs game code reports the error the runtime
//! raises for it.
use super::super::invalid;
use super::{
    BodyKinds, DebugViewOptions, Dimension, Hits, QueryFilter, QueryPose, QueryShape, SensorFilter,
    contains_point, debug_view_with, layer_mask, overlap, project_point, ray_cast, ray_cast_all,
    shape_cast_all,
};
use crate::{AuthoredId, InputSnapshot, Project, Result, Runtime};
use bevy_ecs::prelude::*;
use glam::{Quat, Vec3};
use serde::Deserialize;
use serde_json::{Value, json};

/// Ticks a tool may advance before querying.
pub const MAX_QUERY_TICKS: u32 = 3600;
/// Largest `limit` a request may ask for.
pub const MAX_QUERY_LIMIT: usize = 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
pub enum Dim {
    #[serde(rename = "2d")]
    Two,
    #[default]
    #[serde(rename = "3d")]
    Three,
}
impl From<Dim> for Dimension {
    fn from(dim: Dim) -> Self {
        match dim {
            Dim::Two => Self::Two,
            Dim::Three => Self::Three,
        }
    }
}
/// A quaternion `[x, y, z, w]` or, for 2D queries, an angle about Z in radians.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RotationSpec {
    Angle(f32),
    Quat([f32; 4]),
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "shape", rename_all = "snake_case", deny_unknown_fields)]
pub enum ShapeSpec {
    Ball {
        radius: f32,
    },
    /// Two numbers in 2D, three in 3D.
    Box {
        half_extents: Vec<f32>,
    },
    Capsule {
        half_height: f32,
        radius: f32,
    },
    Cylinder {
        half_height: f32,
        radius: f32,
    },
    ConvexHull {
        points: Vec<Vec<f32>>,
    },
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct FilterSpec {
    /// Named `engine.PhysicsSettings` layers to include; combines with `mask` by OR.
    pub layers: Option<Vec<String>>,
    /// Raw collider layer membership mask.
    pub mask: Option<u32>,
    /// Authored entity ids never reported.
    pub exclude: Vec<String>,
    pub sensors: Option<String>,
    /// Subset of `fixed`, `dynamic`, `kinematic`.
    pub kinds: Option<Vec<String>>,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueryKind {
    Ray {
        origin: Vec<f32>,
        direction: Vec<f32>,
        #[serde(default = "default_distance")]
        max_distance: f32,
        /// Every hit instead of the closest.
        #[serde(default)]
        all: bool,
    },
    ShapeCast {
        #[serde(flatten)]
        shape: ShapeSpec,
        position: Vec<f32>,
        rotation: Option<RotationSpec>,
        direction: Vec<f32>,
        #[serde(default = "default_distance")]
        max_distance: f32,
    },
    Overlap {
        #[serde(flatten)]
        shape: ShapeSpec,
        position: Vec<f32>,
        rotation: Option<RotationSpec>,
    },
    /// Nearest collider, or with `contains` every collider containing the point.
    Point {
        point: Vec<f32>,
        #[serde(default)]
        contains: bool,
    },
}
fn default_distance() -> f32 {
    1000.0
}
fn default_limit() -> usize {
    64
}
/// One query against a headless scene.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct QueryRequest {
    #[serde(default)]
    pub dimension: Dim,
    #[serde(default)]
    pub filter: FilterSpec,
    /// Most hits returned by multi-hit queries (1..=1024).
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(flatten)]
    pub query: QueryKind,
}

/// Shortest decimal rendering of an `f32` as a JSON number.
fn num(value: f32) -> Value {
    value
        .to_string()
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}
fn vec3(value: Vec3, dimension: Dimension) -> Value {
    match dimension {
        Dimension::Two => json!([num(value.x), num(value.y)]),
        Dimension::Three => json!([num(value.x), num(value.y), num(value.z)]),
    }
}
fn point(values: &[f32], field: &str, dimension: Dimension) -> Result<Vec3> {
    let expected = if dimension == Dimension::Two { 2 } else { 3 };
    if values.len() != expected || values.iter().any(|v| !v.is_finite() || v.abs() > 1e8) {
        return Err(invalid(format!(
            "{field} must be {expected} finite numbers"
        )));
    }
    Ok(Vec3::new(
        values[0],
        values[1],
        if expected == 3 { values[2] } else { 0. },
    ))
}
fn rotation(spec: Option<RotationSpec>, dimension: Dimension) -> Result<Quat> {
    let quat = match spec {
        None => Quat::IDENTITY,
        Some(RotationSpec::Angle(angle)) if angle.is_finite() => Quat::from_rotation_z(angle),
        Some(RotationSpec::Quat(q)) => {
            let quat = Quat::from_array(q);
            if !quat.is_finite() || quat.length_squared() < 1e-12 {
                return Err(invalid("rotation must be a nonzero finite quaternion"));
            }
            quat.normalize()
        }
        Some(RotationSpec::Angle(_)) => return Err(invalid("rotation angle must be finite")),
    };
    if dimension == Dimension::Two && (quat.x.abs() > 1e-4 || quat.y.abs() > 1e-4) {
        return Err(invalid("2D rotation must be about Z"));
    }
    Ok(quat)
}
fn shape(spec: &ShapeSpec, dimension: Dimension) -> Result<QueryShape> {
    let lift = |values: &[f32], field: &str| point(values, field, dimension);
    Ok(match spec {
        ShapeSpec::Ball { radius } => QueryShape::Ball { radius: *radius },
        ShapeSpec::Box { half_extents } => {
            let h = lift(half_extents, "half_extents")?;
            QueryShape::Box { half_extents: h }
        }
        ShapeSpec::Capsule {
            half_height,
            radius,
        } => QueryShape::Capsule {
            half_height: *half_height,
            radius: *radius,
        },
        ShapeSpec::Cylinder {
            half_height,
            radius,
        } => QueryShape::Cylinder {
            half_height: *half_height,
            radius: *radius,
        },
        ShapeSpec::ConvexHull { points } => QueryShape::ConvexHull {
            points: points
                .iter()
                .map(|p| lift(p, "points"))
                .collect::<Result<_>>()?,
        },
    })
}
fn entity_by_authored_id(world: &World, id: &str) -> Result<Entity> {
    world
        .iter_entities()
        .find(|e| e.get::<AuthoredId>().is_some_and(|a| a.id == id))
        .map(|e| e.id())
        .ok_or_else(|| invalid(format!("filter.exclude names unknown entity {id}")))
}
fn filter(world: &World, spec: &FilterSpec) -> Result<QueryFilter> {
    let mut result = QueryFilter::default();
    if spec.layers.is_some() || spec.mask.is_some() {
        let named = match &spec.layers {
            Some(names) => {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                layer_mask(world, &refs).map_err(|_| {
                    invalid(format!(
                        "filter.layers {names:?} names a layer missing from engine.PhysicsSettings.layers"
                    ))
                })?
            }
            None => 0,
        };
        result.mask = named | spec.mask.unwrap_or(0);
    }
    for id in &spec.exclude {
        result.exclude.push(entity_by_authored_id(world, id)?);
    }
    result.sensors = match spec.sensors.as_deref() {
        None | Some("include") => SensorFilter::Include,
        Some("exclude") => SensorFilter::Exclude,
        Some("only") => SensorFilter::Only,
        Some(_) => return Err(invalid("filter.sensors is include, exclude or only")),
    };
    if let Some(kinds) = &spec.kinds {
        let mut set = BodyKinds {
            fixed: false,
            dynamic: false,
            kinematic: false,
        };
        for kind in kinds {
            match kind.as_str() {
                "fixed" => set.fixed = true,
                "dynamic" => set.dynamic = true,
                "kinematic" => set.kinematic = true,
                _ => return Err(invalid("filter.kinds are fixed, dynamic or kinematic")),
            }
        }
        result.kinds = set;
    }
    Ok(result)
}
fn name(world: &World, entity: Entity) -> Value {
    json!(world.get::<AuthoredId>(entity).map(|a| a.id.clone()))
}
fn hits_json<T>(
    world: &World,
    dimension: Dimension,
    hits: &Hits<T>,
    row: impl Fn(&T) -> (Entity, Value),
) -> Value {
    let rows: Vec<Value> = hits
        .hits
        .iter()
        .map(|hit| {
            let (entity, mut fields) = row(hit);
            fields["entity"] = name(world, entity);
            fields["entity_bits"] = json!(entity.to_bits());
            fields
        })
        .collect();
    json!({"dimension":if dimension==Dimension::Two {"2d"} else {"3d"},
        "count":rows.len(),"truncated":hits.truncated,"hits":rows})
}

/// Runs `request` against `world`'s synced physics bodies.
pub fn run_query(world: &World, request: &QueryRequest) -> Result<Value> {
    let dimension = Dimension::from(request.dimension);
    if !(1..=MAX_QUERY_LIMIT).contains(&request.limit) {
        return Err(invalid("limit must be 1..=1024"));
    }
    let filter = filter(world, &request.filter)?;
    let limit = request.limit;
    match &request.query {
        QueryKind::Ray {
            origin,
            direction,
            max_distance,
            all,
        } => {
            let origin = point(origin, "origin", dimension)?;
            let direction = point(direction, "direction", dimension)?;
            let hits = if *all {
                ray_cast_all(
                    world,
                    dimension,
                    origin,
                    direction,
                    *max_distance,
                    &filter,
                    limit,
                )?
            } else {
                let hit = ray_cast(world, dimension, origin, direction, *max_distance, &filter)?;
                Hits {
                    hits: hit.into_iter().collect(),
                    truncated: false,
                }
            };
            Ok(hits_json(world, dimension, &hits, |h| {
                (
                    h.entity,
                    json!({"distance":num(h.distance),"point":vec3(h.point,dimension),
                        "normal":vec3(h.normal,dimension)}),
                )
            }))
        }
        QueryKind::ShapeCast {
            shape: spec,
            position,
            rotation: spin,
            direction,
            max_distance,
        } => {
            let pose = QueryPose {
                position: point(position, "position", dimension)?,
                rotation: rotation(*spin, dimension)?,
            };
            let hits = shape_cast_all(
                world,
                dimension,
                &shape(spec, dimension)?,
                pose,
                point(direction, "direction", dimension)?,
                *max_distance,
                &filter,
                limit,
            )?;
            Ok(hits_json(world, dimension, &hits, |h| {
                (
                    h.entity,
                    json!({"distance":num(h.distance),"point":vec3(h.point,dimension),
                        "normal":vec3(h.normal,dimension),
                        "shape_point":vec3(h.shape_point,dimension),"penetrating":h.penetrating}),
                )
            }))
        }
        QueryKind::Overlap {
            shape: spec,
            position,
            rotation: spin,
        } => {
            let pose = QueryPose {
                position: point(position, "position", dimension)?,
                rotation: rotation(*spin, dimension)?,
            };
            let hits = overlap(
                world,
                dimension,
                &shape(spec, dimension)?,
                pose,
                &filter,
                limit,
            )?;
            Ok(hits_json(world, dimension, &hits, |h| {
                (h.entity, json!({"distance":num(h.distance)}))
            }))
        }
        QueryKind::Point {
            point: target,
            contains,
        } => {
            let target = point(target, "point", dimension)?;
            let hits = if *contains {
                contains_point(world, dimension, target, &filter, limit)?
            } else {
                Hits {
                    hits: project_point(world, dimension, target, &filter)?
                        .into_iter()
                        .collect(),
                    truncated: false,
                }
            };
            Ok(hits_json(world, dimension, &hits, |h| {
                (
                    h.entity,
                    json!({"distance":num(h.distance),"point":vec3(h.point,dimension),
                        "inside":h.inside}),
                )
            }))
        }
    }
}

/// Starts the project's scene headless and advances it `ticks` times of `dt` seconds.
fn simulate(project: Project, ticks: u32, dt: f32) -> Result<(Runtime, tempfile::TempDir)> {
    if ticks > MAX_QUERY_TICKS {
        return Err(invalid("ticks must be 0..=3600"));
    }
    if !dt.is_finite() || !(0.0001..=0.1).contains(&dt) {
        return Err(invalid("dt must be 0.0001..=0.1 seconds"));
    }
    let mut runtime = Runtime::new(project)?;
    // Keep persistence away from the real user data directory.
    let data = tempfile::tempdir()?;
    runtime.set_user_data_root(data.path().to_owned())?;
    runtime.start()?;
    for _ in 0..ticks {
        runtime.tick(dt, InputSnapshot::default())?;
    }
    Ok((runtime, data))
}
/// Loads `project`'s active scene, advances `ticks` ticks of `dt`, then answers `request`.
pub fn query_scene(project: Project, ticks: u32, dt: f32, request: &QueryRequest) -> Result<Value> {
    let (mut runtime, _data) = simulate(project, ticks, dt)?;
    let result = run_query(runtime.app().world(), request);
    let stopped = runtime.stop();
    let mut value = result?;
    stopped?;
    value["ticks"] = json!(ticks);
    Ok(value)
}
/// Loads `project`'s active scene, advances `ticks` ticks of `dt`, then summarises its
/// physics debug view: totals, state counts, up to `options.max_items` colliders, contacts
/// and anchors, and the size of the full line list.
pub fn debug_scene(
    project: Project,
    ticks: u32,
    dt: f32,
    options: &DebugViewOptions,
) -> Result<Value> {
    let (mut runtime, _data) = simulate(project, ticks, dt)?;
    let result = summarize(runtime.app().world(), options);
    let stopped = runtime.stop();
    let mut value = result?;
    stopped?;
    value["ticks"] = json!(ticks);
    Ok(value)
}
fn summarize(world: &World, options: &DebugViewOptions) -> Result<Value> {
    // Count over the whole world, then cut the listed items to the caller's cap.
    let full = DebugViewOptions {
        max_items: engine_physics::dim3::MAX_DEBUG_ITEMS,
        ..*options
    };
    let view = debug_view_with(world, &full)?;
    let owner = |id: u64| super::body_entity(id).map_or(Value::Null, |entity| name(world, entity));
    let shape_name = |shape: &dyn std::fmt::Debug| {
        let text = format!("{shape:?}");
        text.split([' ', '{', '('])
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    let mut states = std::collections::BTreeMap::<&str, usize>::new();
    let mut colliders = Vec::new();
    for c in &view.two.colliders {
        *states.entry(c.state.key()).or_default() += 1;
        colliders.push(json!({"entity":owner(c.id),"dimension":"2d","state":c.state.key(),
            "shape":shape_name(&c.shape),"position":c.position.map(num),"rotation":num(c.rotation)}));
    }
    for c in &view.three.colliders {
        *states.entry(c.state.key()).or_default() += 1;
        colliders.push(json!({"entity":owner(c.id),"dimension":"3d","state":c.state.key(),
            "shape":shape_name(&c.shape),"position":c.position.map(num),"rotation":c.rotation.map(num)}));
    }
    let mut contacts = Vec::new();
    for c in &view.two.contacts {
        contacts.push(
            json!({"dimension":"2d","first":owner(c.first),"second":owner(c.second),
            "point":c.point.map(num),"normal":c.normal.map(num),"depth":num(c.depth)}),
        );
    }
    for c in &view.three.contacts {
        contacts.push(
            json!({"dimension":"3d","first":owner(c.first),"second":owner(c.second),
            "point":c.point.map(num),"normal":c.normal.map(num),"depth":num(c.depth)}),
        );
    }
    let mut anchors = Vec::new();
    for a in &view.two.anchors {
        anchors.push(
            json!({"dimension":"2d","owner":owner(a.owner),"kind":a.kind,
            "position":a.position.map(num)}),
        );
    }
    for a in &view.three.anchors {
        anchors.push(
            json!({"dimension":"3d","owner":owner(a.owner),"kind":a.kind,
            "position":a.position.map(num)}),
        );
    }
    let truncated = view.truncated()
        || [colliders.len(), contacts.len(), anchors.len()]
            .iter()
            .any(|n| *n > options.max_items);
    colliders.truncate(options.max_items);
    contacts.truncate(options.max_items);
    anchors.truncate(options.max_items);
    let lines = view.lines(usize::MAX / 2);
    Ok(json!({
        "totals":{
            "colliders":view.two.totals.colliders + view.three.totals.colliders,
            "contacts":view.two.totals.contacts + view.three.totals.contacts,
            "anchors":view.two.totals.anchors + view.three.totals.anchors,
        },
        "states":states,
        "line_segments":lines.segments.len(),
        "truncated":truncated,
        "max_items":options.max_items,
        "colliders":colliders,
        "contacts":contacts,
        "anchors":anchors,
    }))
}
