//! Runtime water service: the simulation clock, compiled surfaces (Gerstner sets, FFT ocean
//! frames, river splines), shoreline ground from terrains, buoyancy impulses, the CPU fallback
//! surface meshes, the GPU render set and the native query API.
use super::component::{
    BuoyancyComponent, WaterBodyComponent, WaterKind, WaterRenderMode, WaveModel,
};
use crate::content::{Mesh, Vertex};
use crate::rendering3d::{Camera3D, MeshMaterial};
use crate::runtime::{AuthoredId, RuntimeLog};
use crate::simulation::physics::{self, Motion, PhysicsBody3D, Shape};
use crate::world::terrain::TerrainComponent;
use crate::{Error, GeometryOverride, MeshRenderer, Parent, Result, Transform};
use bevy_ecs::prelude::*;
use engine_terrain::Heightfield;
use engine_water::{
    BodyMotion, BuoyancyPoint, DragParams, GRAVITY, GerstnerSet, OceanSpectrum, RiverSpline,
    SurfaceSample, Underwater, WaterAt, WaterShape, WaterSurface, Waves, box_points,
    buoyancy_impulse,
};
use glam::{Mat4, Quat, Vec2, Vec3};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Most `engine.WaterBody` entities in one world.
pub const MAX_WATER_BODIES: usize = 64;
/// Most `engine.Buoyancy` bodies simulated per tick.
pub const MAX_BUOYANT_BODIES: usize = 1024;

fn invalid(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

/// Marker resource: the `world.water` GPU feature (`engine-world-render`) is installed, so
/// `render_mode: auto` water bodies publish themselves in [`WaterRenderSet`] instead of
/// spawning CPU surface meshes.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaterGpuRenderer;

/// The water simulation clock: advanced by every tick's `dt` in `pre_simulation`, so wave
/// phases, buoyancy, CPU meshes and the GPU feature all see the same time.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct WaterClock {
    /// Simulated seconds since the runtime started.
    pub time: f64,
    pub ticks: u64,
}

/// One GPU-path water body, as the `world.water` feature draws it.
#[derive(Clone, Debug)]
pub struct WaterRenderItem {
    pub owner: Entity,
    /// Authored id of the water entity.
    pub id: String,
    pub kind: WaterKind,
    /// The surface at the current clock (waves, ocean frame, river, optics, shoreline).
    pub surface: Arc<WaterSurface>,
    pub world_from_local: [[f32; 4]; 4],
    pub half_extent: [f32; 2],
    pub grid_resolution: u32,
    /// Authored opaque depth-only shadow policy.
    pub cast_shadows: bool,
    /// Per grid vertex `(amplitude factor, shoreline foam)`, row-major over
    /// `(grid_resolution + 1)^2` vertices; `None` without ground (open water).
    pub shore: Option<Arc<Vec<[f32; 2]>>>,
    /// Bumped whenever the surface definition (not only time) changed.
    pub revision: u64,
}

/// The GPU-path water bodies of this tick.
#[derive(Resource, Clone, Debug, Default)]
pub struct WaterRenderSet {
    pub items: Vec<WaterRenderItem>,
    /// The water clock (seconds, wave time is `time * time_scale` per body).
    pub time: f32,
}

/// Underwater state of the active `Camera3D` after the last tick.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct UnderwaterView {
    /// The water body the camera is in and its fog.
    pub water: Option<String>,
    pub underwater: Option<Underwater>,
}

/// Counts and notes for telemetry and tests.
#[derive(Clone, Debug, PartialEq)]
pub struct WaterStatus {
    pub kind: WaterKind,
    pub model: WaveModel,
    /// `cpu-mesh`, `gpu-feature` or `hidden`.
    pub render_path: &'static str,
    pub mesh_vertices: usize,
    pub mesh_triangles: usize,
    /// Wave time of the current surface (s).
    pub wave_time: f32,
    /// Gerstner: Lipschitz constant of the horizontal displacement and the steepness scale.
    pub lipschitz: Option<f32>,
    pub steepness_scale: Option<f32>,
    /// FFT: significant wave height of the spectrum (m).
    pub significant_height: Option<f32>,
    /// River: discharge (m^3/s) and length (m).
    pub discharge: Option<f32>,
    pub river_length: Option<f32>,
    pub shoreline_ground: bool,
    pub revision: u64,
    pub notes: Vec<String>,
}

/// What buoyancy did to one body in the last tick.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuoyancyStatus {
    pub points: usize,
    pub submerged_fraction: f32,
    pub submerged_volume: f32,
    pub buoyant_force: f32,
    pub notes: Vec<String>,
}

/// World-space answer of a water query.
#[derive(Clone, Debug, PartialEq)]
pub struct WaterQuery {
    pub entity: Entity,
    pub id: String,
    /// World height of the surface above the point.
    pub surface_height: f32,
    pub normal: Vec3,
    /// Water velocity (flow plus orbital motion), world space.
    pub velocity: Vec3,
    /// `surface_height - point.y` (positive below the surface).
    pub depth: f32,
    pub underwater: Option<Underwater>,
    pub crest_foam: f32,
    pub shore_foam: f32,
    pub amplitude: f32,
    pub residual: f32,
}

struct Body {
    component: WaterBodyComponent,
    id: String,
    spectrum: Option<Arc<OceanSpectrum>>,
    gerstner: Option<Arc<GerstnerSet>>,
    river: Option<Arc<RiverSpline>>,
    surface: Arc<WaterSurface>,
    world_from_local: Mat4,
    local_from_world: Mat4,
    revision: u64,
    mesh: Option<Entity>,
    mesh_counts: (usize, usize),
    render_path: &'static str,
    notes: Vec<String>,
    /// Wave time of the current surface in seconds (`clock * time_scale`), for telemetry; the
    /// waves themselves are evaluated from the f64 clock (see [`advance`]).
    wave_time: f32,
}

/// Runtime water state, one entry per `engine.WaterBody` entity.
#[derive(Resource, Default)]
pub struct Waters {
    bodies: BTreeMap<Entity, Body>,
    buoyancy: BTreeMap<Entity, BuoyancyStatus>,
    /// Centre of mass of every buoyant body after the previous tick, for its mean velocity.
    previous: BTreeMap<Entity, Vec3>,
    ground: Arc<Ground>,
    revisions: u64,
    /// Water bodies skipped by the last tick and why (more than [`MAX_WATER_BODIES`] bodies, a
    /// singular transform, an invalid wave setup). The rest of the world keeps ticking.
    failures: BTreeMap<Entity, String>,
}

/// Terrain heights snapshot for shoreline attenuation (world space).
#[derive(Default)]
struct Ground {
    terrains: Vec<(Arc<Heightfield>, Mat4, Mat4)>,
}
impl Ground {
    fn world_height(&self, x: f32, z: f32) -> Option<f32> {
        let mut best: Option<f32> = None;
        for (field, world_from_local, local_from_world) in &self.terrains {
            let local = local_from_world.transform_point3(Vec3::new(x, 0.0, z));
            if !field.contains(local.x, local.z) || field.is_hole_at(local.x, local.z) {
                continue;
            }
            let h = field.height_at(local.x, local.z);
            let y = world_from_local
                .transform_point3(Vec3::new(local.x, h, local.z))
                .y;
            best = Some(best.map_or(y, |b| b.max(y)));
        }
        best
    }
}

/// Marks the generated CPU surface mesh entity of a water body.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaterMesh {
    pub owner: Entity,
}

pub(super) fn install(world: &mut World) {
    world.insert_resource(Waters::default());
    world.insert_resource(WaterClock::default());
    world.insert_resource(WaterRenderSet::default());
    world.insert_resource(UnderwaterView::default());
}

fn authored_id(world: &World, entity: Entity) -> String {
    world
        .get::<AuthoredId>(entity)
        .map(|a| a.id.clone())
        .unwrap_or_else(|| format!("entity:{}", entity.to_bits()))
}

fn matrix(world: &World, entity: Entity) -> Result<Mat4> {
    Ok(Mat4::from_cols_array_2d(
        &crate::world_transform(world, entity)?.matrix,
    ))
}

fn snapshot_ground(world: &mut World) -> Arc<Ground> {
    let mut terrains: Vec<(String, Entity)> = world
        .iter_entities()
        .filter(|e| e.contains::<TerrainComponent>())
        .map(|e| (authored_id(world, e.id()), e.id()))
        .collect();
    terrains.sort();
    let mut ground = Ground::default();
    for (_, entity) in terrains {
        let Ok(field) = crate::world::terrain::heightfield(world, entity) else {
            continue;
        };
        let Ok(m) = matrix(world, entity) else {
            continue;
        };
        if m.determinant().abs() < 1.0e-12 {
            continue;
        }
        ground.terrains.push((field, m, m.inverse()));
    }
    Arc::new(ground)
}

/// Ground under water-local `(x, z)` in water-local height, from the world snapshot.
fn local_ground<'a>(ground: &'a Ground, body: &'a Body) -> impl Fn(f32, f32) -> Option<f32> + 'a {
    move |x, z| {
        let w = body.world_from_local.transform_point3(Vec3::new(x, 0.0, z));
        let g = ground.world_height(w.x, w.z)?;
        Some(
            body.local_from_world
                .transform_point3(Vec3::new(w.x, g, w.z))
                .y,
        )
    }
}

/// Wave time in f64 seconds. It stays f64 until the waves reduce it to a phase: an f32 clock
/// loses a wave's phase after a few hours.
fn wave_clock(clock: &WaterClock, component: &WaterBodyComponent) -> f64 {
    clock.time * f64::from(component.waves.time_scale)
}

fn build(component: &WaterBodyComponent, id: String, revision: u64) -> Result<Body> {
    component.validate()?;
    let mapped = |e: engine_water::Error| invalid(e.code, format!("{id}: {}", e.message));
    let depth = (component.waves.depth > 0.0).then_some(component.waves.depth);
    let gerstner = match component.waves.model {
        WaveModel::Gerstner if !component.waves.gerstner.is_empty() => Some(Arc::new(
            GerstnerSet::new(&component.waves.gerstner, depth, GRAVITY).map_err(mapped)?,
        )),
        _ => None,
    };
    let spectrum = match component.waves.model {
        WaveModel::Fft => Some(Arc::new(
            OceanSpectrum::new(&component.waves.fft, GRAVITY).map_err(mapped)?,
        )),
        _ => None,
    };
    let river = match component.kind {
        WaterKind::River => Some(Arc::new(
            RiverSpline::new(&component.river).map_err(mapped)?,
        )),
        _ => None,
    };
    let half = Vec2::from(component.half_extent);
    let shape = match (&component.kind, &river) {
        (WaterKind::River, Some(river)) => WaterShape::River(river.clone()),
        (WaterKind::Lake, _) => WaterShape::Lake { half_extent: half },
        _ => WaterShape::Ocean { half_extent: half },
    };
    let surface = Arc::new(WaterSurface {
        shape,
        waves: Waves::Flat,
        optics: component.optics.clone(),
        shoreline: component.shoreline.settings(),
        time: 0.0,
    });
    Ok(Body {
        component: component.clone(),
        id,
        spectrum,
        gerstner,
        river,
        surface,
        world_from_local: Mat4::IDENTITY,
        local_from_world: Mat4::IDENTITY,
        revision,
        mesh: None,
        mesh_counts: (0, 0),
        render_path: "hidden",
        notes: Vec::new(),
        wave_time: 0.0,
    })
}

/// Evaluates the waves at the clock. Gerstner phases and FFT mode phases are reduced to one
/// turn in f64 (`omega * t` mod 2 pi), so the surface after ten hours equals the surface at
/// the same phase early in the run; only the reduced phase is converted to f32.
fn advance(body: &mut Body, clock: &WaterClock) -> Result<()> {
    let t = wave_clock(clock, &body.component);
    let (waves, time) = if let Some(set) = &body.gerstner {
        // Baked: the set's phases hold `phase - omega t`, so the surface reads time 0.
        (Waves::Gerstner(Arc::new(set.at_time(t))), 0.0)
    } else if let Some(spectrum) = &body.spectrum {
        let frame = spectrum
            .frame_f64(t)
            .map_err(|e| invalid(e.code, e.message))?;
        let time = frame.time();
        (Waves::Ocean(Arc::new(frame)), time)
    } else {
        (Waves::Flat, 0.0)
    };
    body.wave_time = t as f32;
    let surface = Arc::make_mut(&mut body.surface);
    surface.waves = waves;
    surface.time = time;
    Ok(())
}

/// Why `entity` was skipped by the last tick (more than [`MAX_WATER_BODIES`] water bodies, a
/// singular transform, an invalid wave setup), if it was. Skipped bodies do not fail the tick.
pub fn failure(world: &World, entity: Entity) -> Option<String> {
    world
        .get_resource::<Waters>()?
        .failures
        .get(&entity)
        .cloned()
}

/// Builds or refreshes one water body for this tick.
fn update_body(world: &mut World, clock: &WaterClock, id: String, entity: Entity) -> Result<()> {
    let component = world
        .get::<WaterBodyComponent>(entity)
        .cloned()
        .expect("filtered");
    let m = matrix(world, entity)?;
    if !m.is_finite() || m.determinant().abs() < 1.0e-12 {
        return Err(invalid(
            "water.transform",
            format!("{id}: singular transform"),
        ));
    }
    let mut waters = world.resource_mut::<Waters>();
    let rebuild = waters
        .bodies
        .get(&entity)
        .is_none_or(|b| b.component != component || b.id != id);
    if rebuild {
        waters.revisions += 1;
        let revision = waters.revisions;
        let mut body = build(&component, id, revision)?;
        if let Some(old) = waters.bodies.remove(&entity) {
            body.mesh = old.mesh;
        }
        waters.bodies.insert(entity, body);
    }
    let body = waters.bodies.get_mut(&entity).expect("built");
    body.world_from_local = m;
    body.local_from_world = m.inverse();
    advance(body, clock)
}

/// Advances the clock, rebuilds changed water bodies, refreshes the shoreline ground and
/// applies buoyancy impulses. Runs from `world::pre_simulation`, before the physics step.
pub fn step_water(world: &mut World, dt: f32) -> Result<()> {
    if !world.contains_resource::<Waters>() {
        return Ok(());
    }
    {
        let mut clock = world.resource_mut::<WaterClock>();
        clock.time += f64::from(dt.max(0.0));
        clock.ticks += 1;
    }
    let clock = *world.resource::<WaterClock>();
    let mut owners: Vec<(String, Entity)> = world
        .iter_entities()
        .filter(|e| e.contains::<WaterBodyComponent>())
        .map(|e| (authored_id(world, e.id()), e.id()))
        .collect();
    owners.sort();
    // Bodies past the cap are skipped (and dropped if they were served before), not an error
    // for the whole tick.
    let mut failures = BTreeMap::new();
    if owners.len() > MAX_WATER_BODIES {
        for (id, entity) in owners.split_off(MAX_WATER_BODIES) {
            failures.insert(
                entity,
                format!(
                    "water.capacity: At most {MAX_WATER_BODIES} engine.WaterBody entities; \
                     `{id}` is ignored"
                ),
            );
        }
    }
    // Drop bodies whose component went away.
    let live: Vec<Entity> = owners.iter().map(|(_, e)| *e).collect();
    let stale: Vec<Entity> = world
        .resource::<Waters>()
        .bodies
        .keys()
        .filter(|e| !live.contains(e))
        .copied()
        .collect();
    for entity in stale {
        if let Some(body) = world.resource_mut::<Waters>().bodies.remove(&entity)
            && let Some(mesh) = body.mesh
        {
            world.despawn(mesh);
        }
    }
    let ground = snapshot_ground(world);
    for (id, entity) in owners {
        if let Err(error) = update_body(world, &clock, id, entity) {
            failures.insert(entity, error.to_string());
            // A body that cannot be updated is not queried with stale state either.
            if let Some(body) = world.resource_mut::<Waters>().bodies.remove(&entity)
                && let Some(mesh) = body.mesh
            {
                world.despawn(mesh);
            }
        }
    }
    let fresh: Vec<String> = {
        let previous = &world.resource::<Waters>().failures;
        failures
            .iter()
            .filter(|(entity, message)| previous.get(*entity) != Some(message))
            .map(|(entity, message)| format!("water entity:{}: {message}", entity.to_bits()))
            .collect()
    };
    if let Some(mut log) = world.get_resource_mut::<RuntimeLog>() {
        log.0.extend(fresh);
    }
    {
        let mut waters = world.resource_mut::<Waters>();
        waters.ground = ground;
        waters.failures = failures;
    }
    apply_buoyancy(world, dt)
}

// ---- queries ------------------------------------------------------------------------------

fn query_body(ground: &Ground, entity: Entity, body: &Body, point: Vec3) -> Option<WaterQuery> {
    let local = body.local_from_world.transform_point3(point);
    let sampler = local_ground(ground, body);
    let sample: SurfaceSample = body.surface.sample(Vec2::new(local.x, local.z), &sampler)?;
    let surface =
        body.world_from_local
            .transform_point3(Vec3::new(local.x, sample.height, local.z));
    let depth = surface.y - point.y;
    let underwater = (depth > 0.0).then(|| Underwater {
        depth,
        fog_color: body.surface.optics.fog_color,
        fog_density: body.surface.optics.fog_density,
        transmittance: body.surface.optics.transmittance(depth),
    });
    Some(WaterQuery {
        entity,
        id: body.id.clone(),
        surface_height: surface.y,
        normal: body
            .world_from_local
            .transform_vector3(sample.normal)
            .normalize_or(Vec3::Y),
        velocity: body.world_from_local.transform_vector3(sample.velocity),
        depth,
        underwater,
        crest_foam: sample.crest_foam,
        shore_foam: sample.attenuation.foam,
        amplitude: sample.attenuation.amplitude,
        residual: sample.residual,
    })
}

/// The water at world `point`: among the bodies whose surface covers the point horizontally
/// (optionally only the one with authored id `only`), the one with the highest surface.
pub fn water_at_filtered(world: &World, point: Vec3, only: Option<&str>) -> Option<WaterQuery> {
    let waters = world.get_resource::<Waters>()?;
    let mut best: Option<WaterQuery> = None;
    for (entity, body) in &waters.bodies {
        if only.is_some_and(|id| id != body.id) {
            continue;
        }
        if let Some(q) = query_body(&waters.ground, *entity, body, point)
            && best
                .as_ref()
                .is_none_or(|b| q.surface_height > b.surface_height)
        {
            best = Some(q);
        }
    }
    best
}

/// The water at world `point` (highest covering surface), or `None` where there is none.
pub fn water_at(world: &World, point: Vec3) -> Option<WaterQuery> {
    water_at_filtered(world, point, None)
}

/// Underwater state of world `point`, with the water body's id.
pub fn underwater_at(world: &World, point: Vec3) -> Option<(String, Underwater)> {
    let q = water_at(world, point)?;
    Some((q.id, q.underwater?))
}

/// The current surface of a water entity (local frame), for custom queries.
pub fn surface(world: &World, entity: Entity) -> Option<Arc<WaterSurface>> {
    Some(
        world
            .get_resource::<Waters>()?
            .bodies
            .get(&entity)?
            .surface
            .clone(),
    )
}

/// Status of a water entity; `None` before its first tick.
pub fn status(world: &World, entity: Entity) -> Option<WaterStatus> {
    let waters = world.get_resource::<Waters>()?;
    let body = waters.bodies.get(&entity)?;
    Some(WaterStatus {
        kind: body.component.kind,
        model: body.component.waves.model,
        render_path: body.render_path,
        mesh_vertices: body.mesh_counts.0,
        mesh_triangles: body.mesh_counts.1,
        wave_time: body.wave_time,
        lipschitz: body.gerstner.as_ref().map(|g| g.lipschitz()),
        steepness_scale: body.gerstner.as_ref().map(|g| g.steepness_scale()),
        significant_height: body.spectrum.as_ref().map(|s| s.stats().significant_height),
        discharge: body.river.as_ref().map(|r| r.discharge()),
        river_length: body.river.as_ref().map(|r| r.length()),
        shoreline_ground: body.component.shoreline.use_terrain
            && !waters.ground.terrains.is_empty(),
        revision: body.revision,
        notes: body.notes.clone(),
    })
}

/// What buoyancy did to `entity` in the last tick.
pub fn buoyancy_status(world: &World, entity: Entity) -> Option<BuoyancyStatus> {
    world
        .get_resource::<Waters>()?
        .buoyancy
        .get(&entity)
        .cloned()
}

// ---- buoyancy -----------------------------------------------------------------------------

/// Local sample points, total volume and slab height from the authored component and the
/// collider shape.
fn sample_layout(
    component: &BuoyancyComponent,
    body: &PhysicsBody3D,
) -> std::result::Result<(Vec<Vec3>, f32, f32), String> {
    let pi = std::f32::consts::PI;
    let bounds = match body.shape {
        Shape::Box => Some((
            Vec3::from(body.half_extents),
            8.0 * body.half_extents.iter().product::<f32>(),
        )),
        Shape::Ball => Some((
            Vec3::splat(body.radius),
            4.0 / 3.0 * pi * body.radius.powi(3),
        )),
        Shape::Capsule => Some((
            Vec3::new(body.radius, body.half_height + body.radius, body.radius),
            pi * body.radius.powi(2) * 2.0 * body.half_height
                + 4.0 / 3.0 * pi * body.radius.powi(3),
        )),
        Shape::Cylinder => Some((
            Vec3::new(body.radius, body.half_height, body.radius),
            pi * body.radius.powi(2) * 2.0 * body.half_height,
        )),
        _ => None,
    };
    let volume = if component.volume > 0.0 {
        component.volume
    } else {
        bounds
            .map(|(_, v)| v)
            .ok_or("volume 0 needs a box, ball, capsule or cylinder collider")?
    };
    if component.points.is_empty() {
        let (half, _) =
            bounds.ok_or("empty points need a box, ball, capsule or cylinder collider")?;
        let (points, height) = box_points(half, [2, 2, 2]);
        let height = if component.point_height > 0.0 {
            component.point_height
        } else {
            height
        };
        return Ok((points, volume, height));
    }
    let height = if component.point_height > 0.0 {
        component.point_height
    } else {
        bounds.map_or(0.5, |(half, _)| half.y)
    };
    Ok((
        component.points.iter().map(|p| Vec3::from(*p)).collect(),
        volume,
        height,
    ))
}

fn apply_buoyancy(world: &mut World, dt: f32) -> Result<()> {
    let mut bodies: Vec<(String, Entity)> = world
        .iter_entities()
        .filter(|e| e.contains::<BuoyancyComponent>())
        .map(|e| (authored_id(world, e.id()), e.id()))
        .collect();
    bodies.sort();
    bodies.truncate(MAX_BUOYANT_BODIES);
    let mut statuses = BTreeMap::new();
    let mut impulses = Vec::new();
    let mut positions = BTreeMap::new();
    {
        let world_ref: &World = world;
        let previous = &world_ref.resource::<Waters>().previous;
        let gravity = physics::physics_worlds(world_ref)
            .map(|(_, three)| Vec3::from(three.gravity()))
            .unwrap_or(Vec3::new(0.0, -GRAVITY, 0.0));
        for (_, entity) in bodies {
            let component = world_ref
                .get::<BuoyancyComponent>(entity)
                .cloned()
                .expect("filtered");
            let mut status = BuoyancyStatus::default();
            let Some(collider) = world_ref.get::<PhysicsBody3D>(entity) else {
                status
                    .notes
                    .push("engine.Buoyancy needs an engine.PhysicsBody3D".into());
                statuses.insert(entity, status);
                continue;
            };
            if collider.motion != Motion::Dynamic {
                status.notes.push("only dynamic bodies float".into());
                statuses.insert(entity, status);
                continue;
            }
            let (local, volume, height) = match sample_layout(&component, collider) {
                Ok(layout) => layout,
                Err(note) => {
                    status.notes.push(note);
                    statuses.insert(entity, status);
                    continue;
                }
            };
            let Ok(state) = physics::body_state(world_ref, entity) else {
                status
                    .notes
                    .push("the physics body is not synced yet".into());
                statuses.insert(entity, status);
                continue;
            };
            let mass = physics::physics_worlds(world_ref)
                .ok()
                .and_then(|(_, three)| three.mass(physics::body_id(entity)))
                .unwrap_or(0.0);
            let rotation: Quat = state.rotation;
            let share = volume / local.len() as f32;
            let points: Vec<BuoyancyPoint> = local
                .iter()
                .map(|p| BuoyancyPoint {
                    position: state.position + rotation * *p,
                    volume: share,
                    height,
                })
                .collect();
            // The physics step integrates gravity after these impulses, so the velocity read
            // here is biased by the last substeps' gravity; the mean velocity over the last tick
            // (from the position change) is not. Teleports fall back to the reported value.
            let linear_velocity = previous
                .get(&entity)
                .map(|p| (state.position - *p) / dt.max(1.0e-6))
                .filter(|v| v.distance(state.linear_velocity) <= 0.5 + 4.0 * gravity.length() * dt)
                .unwrap_or(state.linear_velocity);
            positions.insert(entity, state.position);
            let motion = BodyMotion {
                center: state.position,
                linear_velocity,
                angular_velocity: state.angular_velocity,
                mass,
            };
            let params = DragParams {
                linear_drag: component.linear_drag,
                angular_drag: component.angular_drag,
                flow_drag: component.flow_drag,
                gravity,
            };
            let only = (!component.water.is_empty()).then_some(component.water.as_str());
            let impulse = buoyancy_impulse(&points, &motion, &params, dt, |p| {
                let q = water_at_filtered(world_ref, p, only)?;
                let density = world_ref
                    .get::<WaterBodyComponent>(q.entity)
                    .map_or(1.0, |w| w.density);
                Some(WaterAt {
                    surface: q.surface_height,
                    velocity: q.velocity,
                    density,
                })
            });
            status.points = points.len();
            status.submerged_fraction = impulse.submerged_fraction;
            status.submerged_volume = impulse.submerged_volume;
            status.buoyant_force = impulse.buoyant_force;
            statuses.insert(entity, status);
            if impulse.submerged_volume > 0.0 {
                impulses.push((entity, impulse));
            }
        }
    }
    for (entity, impulse) in impulses {
        // A body whose impulse cannot be applied (for example removed this tick) is noted,
        // not a reason to fail the tick.
        let mut applied = Ok(());
        if impulse.linear.is_finite() && impulse.linear != Vec3::ZERO {
            applied = physics::apply_impulse(world, entity, impulse.linear);
        }
        if applied.is_ok() && impulse.angular.is_finite() && impulse.angular != Vec3::ZERO {
            applied = physics::apply_angular_impulse(world, entity, impulse.angular);
        }
        if let (Err(error), Some(status)) = (applied, statuses.get_mut(&entity)) {
            status.notes.push(format!("buoyancy impulse: {error}"));
        }
    }
    let mut waters = world.resource_mut::<Waters>();
    waters.buoyancy = statuses;
    waters.previous = positions;
    Ok(())
}

// ---- rendering ----------------------------------------------------------------------------

fn active_camera_position(world: &World) -> Option<Vec3> {
    let mut cameras: Vec<(String, Entity)> = world
        .iter_entities()
        .filter(|e| e.get::<Camera3D>().is_some_and(|c| c.active))
        .map(|e| (authored_id(world, e.id()), e.id()))
        .collect();
    cameras.sort();
    let (_, camera) = cameras.into_iter().next()?;
    crate::world_transform(world, camera)
        .ok()
        .map(|t| Vec3::from(t.position()))
}

/// Per-vertex `(amplitude, foam)` over the body's grid, or `None` without ground under it.
fn shore_grid(ground: &Ground, body: &Body) -> Option<Arc<Vec<[f32; 2]>>> {
    if !body.component.shoreline.use_terrain
        || !body.component.shoreline.enabled
        || ground.terrains.is_empty()
        || body.component.kind == WaterKind::River
    {
        return None;
    }
    let n = body.component.grid_resolution;
    let half = Vec2::from(body.component.half_extent);
    let sampler = local_ground(ground, body);
    let settings = body.component.shoreline.settings();
    let mut any = false;
    let grid = (0..=n)
        .flat_map(|j| (0..=n).map(move |i| (i, j)))
        .map(|(i, j)| {
            let p = grid_point(half, n, i, j);
            let depth = sampler(p.x, p.y).map(|g| -g);
            any |= depth.is_some();
            let a = settings.attenuation(depth);
            [a.amplitude, a.foam]
        })
        .collect();
    any.then(|| Arc::new(grid))
}

/// Rest position of grid vertex `(i, j)` (local XZ).
pub fn grid_point(half: Vec2, n: u32, i: u32, j: u32) -> Vec2 {
    Vec2::new(
        -half.x + 2.0 * half.x * i as f32 / n as f32,
        -half.y + 2.0 * half.y * j as f32 / n as f32,
    )
}

fn surface_mesh(body: &Body, shore: Option<&[[f32; 2]]>) -> Mesh {
    let optics = &body.surface.optics;
    let shallow = Vec3::from(optics.shallow_color);
    let deep = Vec3::from(optics.deep_color);
    let foam_color = Vec3::from(optics.foam_color);
    let color = |foam: f32, shallowness: f32| {
        let c = deep.lerp(shallow, shallowness.clamp(0.0, 1.0));
        let c = c.lerp(foam_color, (foam * optics.foam_intensity).clamp(0.0, 1.0));
        [c.x, c.y, c.z, 1.0]
    };
    if let WaterShape::River(river) = &body.surface.shape {
        let across = (body.component.grid_resolution / 8).clamp(1, 32);
        let (positions, uvs, indices) = river.ribbon(across);
        return Mesh {
            normals: vec![[0.0, 1.0, 0.0]; positions.len()],
            vertices: positions
                .iter()
                .zip(&uvs)
                .map(|(p, uv)| Vertex {
                    position: p.to_array(),
                    uv: *uv,
                    color: color(0.0, 0.6),
                })
                .collect(),
            indices,
        };
    }
    let n = body.component.grid_resolution;
    let half = Vec2::from(body.component.half_extent);
    let t = body.surface.time;
    let side = n + 1;
    let mut vertices = Vec::with_capacity((side * side) as usize);
    let mut normals = Vec::with_capacity(vertices.capacity());
    for j in 0..side {
        for i in 0..side {
            let x0 = grid_point(half, n, i, j);
            let [scale, shore_foam] = shore.map_or([1.0, 0.0], |s| s[(j * side + i) as usize]);
            let (offset, normal, det) = match &body.surface.waves {
                Waves::Flat => (Vec3::ZERO, Vec3::Y, 1.0),
                Waves::Gerstner(set) => {
                    let p = set.evaluate(x0, t, scale);
                    let j = glam::Mat2::IDENTITY + p.jacobian;
                    (p.offset, p.normal(), j.determinant())
                }
                Waves::Ocean(frame) => {
                    let p = frame.at_rest(x0);
                    (
                        p.offset * scale,
                        Vec3::Y.lerp(p.normal, scale).normalize_or(Vec3::Y),
                        1.0 + (p.determinant - 1.0) * scale,
                    )
                }
            };
            let foam = optics.crest_foam(det).max(shore_foam);
            vertices.push(Vertex {
                position: [x0.x + offset.x, offset.y, x0.y + offset.z],
                uv: [i as f32 / n as f32, j as f32 / n as f32],
                color: color(foam, 1.0 - scale),
            });
            normals.push(normal.to_array());
        }
    }
    let mut indices = Vec::with_capacity((n * n * 6) as usize);
    for j in 0..n {
        for i in 0..n {
            let a = j * side + i;
            let (b, c) = (a + side, a + 1);
            indices.extend_from_slice(&[a, b, c, b, b + 1, c]);
        }
    }
    Mesh {
        normals,
        vertices,
        indices,
    }
}

/// Chooses each body's render path, rebuilds CPU surface meshes, publishes GPU-path bodies in
/// [`WaterRenderSet`] and updates [`UnderwaterView`] for the active camera. Runs from
/// `world::post_transforms`.
pub fn update_water_render(world: &mut World) -> Result<()> {
    if !world.contains_resource::<Waters>() {
        return Ok(());
    }
    let gpu_installed = world.contains_resource::<WaterGpuRenderer>();
    let clock = *world.resource::<WaterClock>();
    let mut set = WaterRenderSet {
        items: Vec::new(),
        time: clock.time as f32,
    };
    let owners: Vec<Entity> = world.resource::<Waters>().bodies.keys().copied().collect();
    let ground = world.resource::<Waters>().ground.clone();
    for entity in owners {
        let Some(mut body) = world.resource_mut::<Waters>().bodies.remove(&entity) else {
            continue;
        };
        let gpu = match body.component.render_mode {
            WaterRenderMode::Cpu => false,
            WaterRenderMode::Gpu => true,
            WaterRenderMode::Auto => gpu_installed,
        };
        body.notes.clear();
        if body.component.render_mode == WaterRenderMode::Gpu && !gpu_installed {
            body.notes.push(
                "render_mode gpu but no water GPU renderer is installed \
                 (engine_world_render::install); nothing draws"
                    .into(),
            );
        }
        if body.component.waves.model == WaveModel::Gerstner && body.gerstner.is_none() {
            body.notes
                .push("wave model gerstner without waves: the surface is flat".into());
        }
        if let Some(g) = &body.gerstner
            && g.steepness_scale() < 1.0
        {
            body.notes.push(format!(
                "Gerstner steepness sum scaled by {:.3} to keep the surface loop-free",
                g.steepness_scale()
            ));
        }
        let shore = shore_grid(&ground, &body);
        let visible = body.component.visible;
        if !visible || gpu {
            if let Some(mesh) = body.mesh.take() {
                world.despawn(mesh);
            }
            body.mesh_counts = (0, 0);
            body.render_path = if visible { "gpu-feature" } else { "hidden" };
            if visible {
                set.items.push(WaterRenderItem {
                    owner: entity,
                    id: body.id.clone(),
                    kind: body.component.kind,
                    surface: body.surface.clone(),
                    world_from_local: body.world_from_local.to_cols_array_2d(),
                    half_extent: body.component.half_extent,
                    grid_resolution: body.component.grid_resolution,
                    cast_shadows: body.component.cast_shadows,
                    shore,
                    revision: body.revision,
                });
            }
        } else {
            body.render_path = "cpu-mesh";
            if body.component.cast_shadows {
                body.notes.push(
                    "cast_shadows requires the GPU water renderer; transparent CPU surface meshes cannot write the engine shadow atlas"
                        .into(),
                );
            }
            let mesh = surface_mesh(&body, shore.as_deref().map(|v| v.as_slice()));
            body.mesh_counts = (mesh.vertices.len(), mesh.indices.len() / 3);
            let geometry = GeometryOverride(Arc::new(mesh));
            let optics = &body.component.optics;
            let material = MeshMaterial {
                tint: [1.0, 1.0, 1.0, 0.75],
                transparent: true,
                roughness: optics.roughness,
                metallic: 0.0,
                cast_shadows: false,
                ..MeshMaterial::default()
            };
            // The mesh is parented by authored id: a body without a registered one (it would
            // fall back to `entity:<bits>`) cannot own a mesh without a dangling `Parent`.
            let registered = world
                .get::<AuthoredId>(entity)
                .is_some_and(|a| crate::services::find_entity(world, &a.id) == Some(entity));
            match body.mesh.filter(|m| world.get_entity(*m).is_ok()) {
                _ if !registered => {
                    if let Some(mesh) = body.mesh.take() {
                        world.despawn(mesh);
                    }
                    body.mesh_counts = (0, 0);
                    body.render_path = "hidden";
                    body.notes.push(
                        "the CPU surface mesh needs an authored ID registered in the entity \
                         lookup; nothing draws"
                            .into(),
                    );
                }
                Some(mesh) => {
                    world.entity_mut(mesh).insert((geometry, material));
                }
                None => {
                    let name = format!("water:{}/surface", body.id);
                    let spawned = world
                        .spawn((
                            AuthoredId {
                                id: name.clone(),
                                name,
                            },
                            Transform::default(),
                            Parent(body.id.clone()),
                            MeshRenderer {
                                version: 1,
                                mesh: String::new(),
                                texture: None,
                            },
                            geometry,
                            material,
                            WaterMesh { owner: entity },
                        ))
                        .id();
                    body.mesh = Some(spawned);
                }
            }
        }
        world.resource_mut::<Waters>().bodies.insert(entity, body);
    }
    world.insert_resource(set);
    let view = match active_camera_position(world) {
        Some(camera) => match water_at(world, camera) {
            Some(q) => UnderwaterView {
                underwater: q.underwater,
                water: q.underwater.map(|_| q.id),
            },
            None => UnderwaterView::default(),
        },
        None => UnderwaterView::default(),
    };
    world.insert_resource(view);
    Ok(())
}
