//! Offline analysis of a scene's acoustic world: inspection, point queries and
//! baking (design record section 7.7). The MCP tools are thin wrappers over these
//! functions, so native code and tests share them. Nothing here starts scripts,
//! audio or the runtime service; queries always use the deterministic CPU tier.
use super::{
    air::air_np_per_m,
    collect::{self, BakeInclude, Collected, WorldBuild},
    components::{EnclosureMode, Plane},
    service::propagation_settings,
};
use crate::{Error, Project, Result, Runtime, Scene};
use engine_acoustics::{
    self as ac, AcousticWorldDocument, AcousticsService, BackendPreference, BandGains,
    ListenerPose, PropagationRequest, PropagationSettings, ServiceConfig, ServiceMode,
    SourceRequest,
};
use serde_json::{Value, json};

/// Contributing entities listed by [`inspect_scene`].
pub const MAX_CONTRIBUTORS: usize = 256;
/// Findings listed by [`inspect_scene`].
pub const MAX_FINDINGS: usize = 64;
/// Sources per [`query_scene`] call.
pub const MAX_QUERY_SOURCES: usize = 16;
/// Reflection paths returned per queried source.
pub const MAX_QUERY_REFLECTIONS: usize = 16;
/// Late-field rays of a query (the CPU tier's own limit).
pub const MAX_QUERY_REVERB_RAYS: u32 = 65_536;
/// A closed room that lets more than this fraction of traced energy escape is
/// reported as a geometry leak.
const LEAK_THRESHOLD: f32 = 0.005;
const LEAK_RAYS: u32 = 2048;
/// Largest serialized baked document (design record section 7.6).
pub const MAX_BAKED_DOCUMENT_BYTES: usize = ac::MAX_DOCUMENT_BYTES;

/// A scene hydrated into its acoustic world.
pub struct SceneAcoustics {
    pub collected: Collected,
    pub build: WorldBuild,
    /// Per-band air attenuation (amplitude Np/m) of the environment.
    pub air_np_per_m: BandGains,
}

fn missing() -> Error {
    Error::new(
        "acoustics.environment.missing",
        "The scene has no enabled engine.AcousticEnvironment",
    )
}

/// Collect the scene's geometry (without building the world).
fn collect_scene(project: &Project, scene: &Scene) -> Result<Option<Collected>> {
    let mut project = project.clone();
    project.scene = scene.clone();
    let mut runtime = Runtime::new_with_simulation(project, None)?;
    runtime.collect_acoustics()
}

/// Collect and build the scene's acoustic world. `Ok(None)` means the scene has
/// no enabled environment.
pub fn load_scene(project: &Project, scene: &Scene) -> Result<Option<SceneAcoustics>> {
    let Some(collected) = collect_scene(project, scene)? else {
        return Ok(None);
    };
    let build = collect::build_world(&collected)?;
    let air = air_np_per_m(&collected.environment.air);
    Ok(Some(SceneAcoustics {
        collected,
        build,
        air_np_per_m: air,
    }))
}

fn db(amplitude: f32) -> f32 {
    if amplitude <= 1e-6 {
        -120.0
    } else {
        (20.0 * amplitude.log10()).max(-120.0)
    }
}
fn bands_db(gains: BandGains) -> [f32; 4] {
    gains.map(db)
}
fn round(value: f32, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (f64::from(value) * scale).round() / scale
}
fn rounded(values: [f32; 4], places: i32) -> Vec<f64> {
    values.iter().map(|v| round(*v, places)).collect()
}
fn finite_or_null(values: [f32; 4], places: i32) -> Value {
    if values.iter().all(|v| v.is_finite()) {
        json!(rounded(values, places))
    } else {
        Value::Null
    }
}

fn environment_summary(c: &Collected) -> Value {
    let e = &c.environment;
    let mut value = json!({
        "enabled": e.enabled,
        "backend": serde_json::to_value(e.backend).unwrap_or(Value::Null),
        "renderer": serde_json::to_value(e.renderer).unwrap_or(Value::Null),
        "reverb": serde_json::to_value(e.reverb).unwrap_or(Value::Null),
        "reverb_source": serde_json::to_value(e.reverb_source).unwrap_or(Value::Null),
        "update_hz": e.update_hz,
        "reverb_update_hz": e.reverb_update_hz,
        "speed_of_sound": e.speed_of_sound,
        "seed": e.seed,
        "render_meshes": e.geometry.render_meshes,
    });
    if let Some(asset) = &e.geometry.asset {
        value["geometry_asset"] = json!(asset);
    }
    if let Some(p) = &e.proxy2d {
        value["proxy2d"] = json!({
            "plane": match p.plane { Plane::SideView => "side_view", Plane::TopDown => "top_down" },
            "units_per_meter": p.units_per_meter,
            "enclosure": match p.enclosure {
                EnclosureMode::None => "none",
                EnclosureMode::Auto => "auto",
                EnclosureMode::Bounds => "bounds",
            },
            "open_sky": p.open_sky,
        });
    }
    value
}

fn contributors(c: &Collected) -> (Vec<Value>, bool) {
    let mut all = Vec::new();
    for i in &c.instances {
        all.push(json!({
            "entity": i.key, "kind": "mesh", "mesh": i.mesh, "material": i.material,
            "mobility": format!("{:?}", i.mobility).to_lowercase(),
        }));
    }
    for f in &c.footprints {
        all.push(json!({
            "entity": f.key, "kind": "footprint", "material": f.material,
            "shape": match f.shape {
                collect::FootprintShape::Rect { .. } => "rect",
                collect::FootprintShape::Circle { .. } => "circle",
                collect::FootprintShape::Capsule { .. } => "capsule",
                collect::FootprintShape::Polygon { .. } => "polygon",
            },
        }));
    }
    for v in &c.volumes {
        all.push(json!({
            "entity": v.key, "kind": "volume", "material": v.material,
            "volume_kind": format!("{:?}", v.kind).to_lowercase(),
        }));
    }
    let truncated = all.len() > MAX_CONTRIBUTORS;
    all.truncate(MAX_CONTRIBUTORS);
    (all, truncated)
}

/// Energy escaping a closed room, traced from its centre on the CPU tier.
fn escaped_fraction(
    world: &ac::AcousticWorld,
    settings: &PropagationSettings,
    point: [f32; 3],
) -> Option<f32> {
    let trace = ac::TraceSettings {
        bin_s: 0.01,
        bins: 100,
        speed_of_sound: settings.speed_of_sound,
        air_np_per_m: settings.air_np_per_m,
    };
    let job = ac::EnergyJob {
        emitter: point,
        receiver: point,
        receiver_radius_m: settings.receiver_radius_m,
        rays: LEAK_RAYS,
        max_bounces: 16,
        seed: settings.seed,
    };
    ac::reference::cpu_reference_histogram(world, &trace, &job, 1)
        .ok()
        .map(|h| h.escaped)
}

/// What the scene's acoustic world contains, without executing game code:
/// statistics, bounds, materials, rooms, the proxy, contributing entities and
/// findings (ignored components, closed rooms that leak).
pub fn inspect_scene(project: &Project, scene: &Scene, include_entities: bool) -> Result<Value> {
    let Some(SceneAcoustics {
        collected,
        build,
        air_np_per_m: air,
    }) = load_scene(project, scene)?
    else {
        return Ok(json!({
            "scene": scene.id,
            "environment": Value::Null,
            "warnings": ["The scene has no enabled engine.AcousticEnvironment, so it has no acoustic world."],
        }));
    };
    let world = &build.world;
    let stats = world.stats();
    let mut warnings: Vec<String> = collected.warnings.clone();
    let settings = propagation_settings(&collected.environment);
    let mut rooms = Vec::new();
    for room in world.rooms() {
        let sabine = room.sabine_rt60(air).ok();
        let eyring = room.eyring_rt60(air).ok();
        rooms.push(json!({
            "volume_m3": round(room.volume_m3, 3),
            "surface_m2": round(room.surface_m2(), 3),
            "open_area_m2": round(room.open_area_m2, 3),
            "sabine_rt60_s": sabine.map_or(Value::Null, |v| finite_or_null(v, 3)),
            "eyring_rt60_s": eyring.map_or(Value::Null, |v| finite_or_null(v, 3)),
        }));
    }
    // Leak check of closed rooms, traced from each room's centre.
    for volume in &collected.volumes {
        if volume.kind != super::components::VolumeKind::Room || volume.open_mask != 0 {
            continue;
        }
        let centre = [
            volume.transform[3],
            volume.transform[7],
            volume.transform[11],
        ];
        if let Some(escaped) = escaped_fraction(world, &settings, centre)
            && escaped > LEAK_THRESHOLD
        {
            warnings.push(format!(
                "closed room {} escapes {:.0}% of traced energy: geometry leak",
                volume.key,
                f64::from(escaped) * 100.0
            ));
        }
    }
    let materials: Vec<Value> = world
        .materials()
        .iter()
        .enumerate()
        .map(|(index, m)| {
            json!({
                "index": index,
                "absorption": rounded(m.absorption, 4),
                "scattering": round(m.scattering, 4),
                "transmission": rounded(m.transmission, 5),
            })
        })
        .collect();
    let proxy = build.proxy.as_ref().map(|p| {
        json!({
            "metres_per_unit": p.metres_per_unit(),
            "air": serde_json::to_value(p.air()).unwrap_or(Value::Null),
        })
    });
    let mut result = json!({
        "scene": scene.id,
        "environment": environment_summary(&collected),
        "world": serde_json::to_value(stats).unwrap_or(Value::Null),
        "bounds": stats.bounds,
        "air_np_per_m": rounded(air, 6),
        "materials": materials,
        "rooms": rooms,
        "dynamic_instances": build.dynamic.len(),
    });
    if let Some(proxy) = proxy {
        result["proxy"] = proxy;
    }
    if include_entities {
        let (list, truncated) = contributors(&collected);
        result["contributors"] = json!(list);
        result["contributors_truncated"] = json!(truncated);
    }
    warnings.truncate(MAX_FINDINGS);
    result["warnings"] = json!(warnings);
    Ok(result)
}

/// One point source of a query, in scene units.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
pub struct QuerySource {
    pub position: [f32; 3],
    #[serde(default)]
    pub radius_m: Option<f32>,
}
/// The listener of a query, in scene units; orientation defaults to facing -Z.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
pub struct QueryListener {
    pub position: [f32; 3],
    #[serde(default)]
    pub forward: Option<[f32; 3]>,
    #[serde(default)]
    pub up: Option<[f32; 3]>,
}
/// Optional overrides of the environment's tracing quality.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryQuality {
    pub occlusion_rays: Option<u16>,
    pub reflection_order: Option<u8>,
    pub max_reflections: Option<u8>,
    pub er_rays: Option<u16>,
    pub reverb_rays: Option<u32>,
    pub max_bounces: Option<u16>,
}
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct QueryRequest {
    pub listener: QueryListener,
    pub sources: Vec<QuerySource>,
    #[serde(default)]
    pub quality: QueryQuality,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new("acoustics.request.invalid", message)
}
fn from_engine(error: ac::AcousticsError) -> Error {
    Error::new(error.code, error.message)
}

fn apply_quality(settings: &mut PropagationSettings, q: &QueryQuality) -> Result<()> {
    if let Some(v) = q.occlusion_rays {
        settings.occlusion_rays = v;
    }
    if let Some(v) = q.reflection_order {
        settings.reflection_order = v;
    }
    if let Some(v) = q.max_reflections {
        settings.max_reflections = v;
    }
    if let Some(v) = q.er_rays {
        settings.er_rays = v;
    }
    if let Some(v) = q.reverb_rays {
        if v > MAX_QUERY_REVERB_RAYS {
            return Err(invalid(format!(
                "quality.reverb_rays is at most {MAX_QUERY_REVERB_RAYS}"
            )));
        }
        settings.reverb_rays = v;
    }
    if let Some(v) = q.max_bounces {
        settings.max_bounces = v;
    }
    ac::validate_settings(settings).map_err(from_engine)
}

/// Occlusion, early reflections and the reverb estimate from `listener` to each
/// source on the CPU tier. Positions are scene units; with a 2D proxy they are
/// mapped like the runtime does. Two identical calls give identical results.
pub fn query_scene(project: &Project, scene: &Scene, request: &QueryRequest) -> Result<Value> {
    if request.sources.is_empty() || request.sources.len() > MAX_QUERY_SOURCES {
        return Err(invalid(format!(
            "sources needs 1-{MAX_QUERY_SOURCES} entries"
        )));
    }
    let loaded = load_scene(project, scene)?.ok_or_else(missing)?;
    let mut settings = propagation_settings(&loaded.collected.environment);
    apply_quality(&mut settings, &request.quality)?;
    let proxy = loaded.build.proxy.clone();
    let map = |p: [f32; 3]| proxy.as_ref().map_or(p, |m| m.to_acoustic(p));
    let mut pose = ListenerPose {
        position: request.listener.position,
        forward: request.listener.forward.unwrap_or([0.0, 0.0, -1.0]),
        up: request.listener.up.unwrap_or([0.0, 1.0, 0.0]),
    };
    if let Some(m) = &proxy {
        pose = m.pose_to_acoustic(pose);
    }
    let sources: Vec<SourceRequest> = request
        .sources
        .iter()
        .enumerate()
        .map(|(i, s)| SourceRequest {
            id: i as u64,
            voice: None,
            position: map(s.position),
            radius_m: s.radius_m.unwrap_or(0.25),
            occlusion: true,
            reflections: true,
            reverb: true,
            hero: false,
            priority: 1.0,
        })
        .collect();
    let world = loaded.build.world;
    let room = world.room_at(pose.position).cloned();
    // The service needs a publisher; a private offline mixer hosts it. Nothing is
    // audible and no device is opened.
    let engine = engine_audio::AudioEngine::offline(Default::default())
        .map_err(|e| Error::new(e.code, e.message))?;
    let publisher = engine
        .control()
        .acoustic_publisher()
        .map_err(|e| Error::new(e.code, e.message))?;
    let mut service = AcousticsService::new(
        ServiceConfig {
            preference: BackendPreference::Cpu,
            mode: ServiceMode::Synchronous,
            update_hz: loaded.collected.environment.update_hz,
            reverb_update_hz: loaded.collected.environment.reverb_update_hz,
            settings,
            backends: vec![],
        },
        publisher,
    )
    .map_err(from_engine)?;
    service.set_world(world);
    service
        .submit(PropagationRequest {
            sequence: 1,
            listener: pose,
            sources,
            settings,
        })
        .map_err(from_engine)?;
    let result = service.step_blocking().map_err(from_engine)?.clone();
    service.shutdown();

    let per_source: Vec<Value> = result
        .sources
        .iter()
        .map(|s| {
            let reflections: Vec<Value> = s
                .reflections
                .iter()
                .take(MAX_QUERY_REFLECTIONS)
                .map(|r| {
                    json!({
                        "order": r.order,
                        "delay_ms": round(r.delay_s * 1000.0, 3),
                        "length_m": round(r.length_m, 3),
                        "gain_db": rounded(bands_db(r.gains), 2),
                        "direction": rounded3(r.direction),
                    })
                })
                .collect();
            json!({
                "index": s.id,
                "visible_fraction": round(s.direct.visible_fraction, 4),
                "distance_m": round(s.direct.distance_m, 3),
                "direct_gain_db": rounded(bands_db(s.direct.gains), 2),
                "direction": rounded3(s.direct.direction),
                "reflections": reflections,
            })
        })
        .collect();
    let rev = &result.reverb;
    let mut reverb = json!({
        "enabled": rev.enabled,
        "rt60_s": finite_or_null(rev.rt60_s, 3),
        "level_db": rounded(bands_db(rev.level.map(f32::sqrt)), 2),
        "predelay_ms": round(rev.predelay_s * 1000.0, 2),
        "mean_free_path_m": round(rev.mean_free_path_m, 3),
    });
    if let Some(field) = &result.listener_field {
        reverb["escaped_fraction"] = json!(round(field.escaped, 4));
    }
    let mut out = json!({
        "scene": scene.id,
        "backend": "cpu",
        "sources": per_source,
        "reverb": reverb,
    });
    if let Some(room) = room {
        let air = loaded.air_np_per_m;
        out["room_estimate"] = json!({
            "volume_m3": round(room.volume_m3, 3),
            "surface_m2": round(room.surface_m2(), 3),
            "sabine_rt60_s": room.sabine_rt60(air).map_or(Value::Null, |v| finite_or_null(v, 3)),
            "eyring_rt60_s": room.eyring_rt60(air).map_or(Value::Null, |v| finite_or_null(v, 3)),
        });
    }
    Ok(out)
}
fn rounded3(v: [f32; 3]) -> Vec<f64> {
    v.iter().map(|x| round(*x, 4)).collect()
}

/// The scene's acoustic source geometry as a sealed `AcousticWorldDocument`,
/// checked by replaying it through the builder (every limit applies). Returns the
/// document and the triangle count of the rebuilt world.
pub fn bake_scene(
    project: &Project,
    scene: &Scene,
    include: BakeInclude,
) -> Result<(AcousticWorldDocument, u32)> {
    let collected = collect_scene(project, scene)?.ok_or_else(missing)?;
    let document = collect::to_document(&collected, include, Some(scene.id.clone()))?;
    let world = document
        .build(ac::WorldLimits::default())
        .map_err(from_engine)?;
    Ok((document, world.stats().triangles))
}

/// Parse a baked `"acoustics"` asset and replay it through the builder under the
/// default limits (the BVH is rebuilt, never trusted). Errors name the asset.
pub fn validate_document(asset: &str, bytes: &[u8]) -> Result<AcousticWorldDocument> {
    let document: AcousticWorldDocument = serde_json::from_slice(bytes).map_err(|e| {
        Error::new(
            "acoustics.document.invalid",
            format!("Acoustics asset {asset} is not a world document: {e}"),
        )
    })?;
    document.build(ac::WorldLimits::default()).map_err(|e| {
        Error::new(
            e.code,
            format!("Acoustics asset {asset} does not build: {}", e.message),
        )
    })?;
    Ok(document)
}
