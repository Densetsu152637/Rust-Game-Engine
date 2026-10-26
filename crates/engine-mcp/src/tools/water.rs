//! Water queries: sample the simulated water of a scene (`water_sample`) and preview wave
//! spectra and Gerstner sets (`water_spectrum`). Water bodies are authored with
//! `component_set` (`engine.WaterBody`, `engine.Buoyancy`). Both tools are read-only and never
//! execute game code: `water_sample` runs the scene headless without native scripts, like
//! `physics_query`.
use super::{Spec, scene, tool};
use crate::{
    config::AccessLevel,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::world::water::{
    self as water, MAX_SAMPLE_POINTS, MAX_SAMPLE_TICKS, WATER_BODY, WaterBodyComponent, WaveModel,
};
use engine_water::{GRAVITY, GerstnerSet, GerstnerWave, OceanSettings, OceanSpectrum};
use serde_json::{Map, Value, json};
use std::f32::consts::TAU;

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "water_sample",
                title: "Sample scene water",
                description: "Load a project scene, run it headless for `ticks` ticks of `dt` seconds (at least one, so the water clock, waves and FFT frames exist; no game code runs and nothing is written), then report every engine.WaterBody (kind, wave model, Gerstner steepness bound, FFT significant wave height, river discharge, notes) and the water at up to 256 world points [x, y, z]: the body covering the point with the highest surface (or only `water`), surface height, depth below the surface (positive underwater), underwater flag with fog colour, density, transmittance and the fog factor over 10 m, surface normal, water velocity (river flow plus orbital motion), crest foam, shoreline foam and wave amplitude factor.",
                input: schema::object(
                    vec![
                        (
                            "scene",
                            schema::string(
                                "Project-relative scene path (see scene_list). Defaults to the startup scene.",
                            ),
                        ),
                        (
                            "points",
                            schema::array_of(
                                "World positions [x, y, z] to sample.",
                                json!({"type":"array","minItems":3,"maxItems":3,"items":{"type":"number"}}),
                                MAX_SAMPLE_POINTS,
                            ),
                        ),
                        (
                            "water",
                            schema::string("Only sample the water body with this authored id."),
                        ),
                        (
                            "ticks",
                            schema::integer(
                                "Ticks to simulate before sampling.",
                                1,
                                i64::from(MAX_SAMPLE_TICKS),
                                Some(1),
                            ),
                        ),
                        (
                            "dt",
                            json!({"type":"number","minimum":0.0001,"maximum":0.1,"default":0.016666668,"description":"Seconds per tick."}),
                        ),
                    ],
                    &["points"],
                ),
                output: schema::output(vec![
                    ("ticks", json!({"type":"integer"})),
                    ("time", json!({"type":"number"})),
                    ("bodies", json!({"type":"array"})),
                    ("samples", json!({"type":"array"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            sample,
        ),
        tool(
            Spec {
                name: "water_spectrum",
                title: "Preview water waves",
                description: "Wave statistics without running a scene. Exactly one source: `water` (an engine.WaterBody entity id in `scene`, using its waves), `ocean` (inline FFT ocean settings: resolution 16..512, patch_length, spectrum phillips|jonswap, wind_speed, wind_direction_degrees, amplitude, fetch, peak_enhancement, choppiness, small_wave_cutoff, depth, loop_period, seed) or `gerstner` (inline waves {direction_degrees, wavelength, amplitude, steepness, phase}, with optional `depth`). FFT: expected variance, significant wave height, continuum variance, peak wavelength and period, mean-square slope, energy by wavelength (16 log bins) and the realized frame at `time` (height range, RMS, folded fraction). Gerstner: per-wave wavenumber, angular frequency, phase speed and period from the dispersion relation, the steepness scale and the displacement Lipschitz bound (at most 0.9: loop-free).",
                input: schema::object(
                    vec![
                        (
                            "scene",
                            schema::string("Scene holding `water`. Defaults to the startup scene."),
                        ),
                        (
                            "water",
                            schema::string("Authored id of an engine.WaterBody entity."),
                        ),
                        (
                            "ocean",
                            json!({"type":"object","description":"Inline FFT ocean settings (engine.WaterBody waves.fft)."}),
                        ),
                        (
                            "gerstner",
                            schema::array_of(
                                "Inline Gerstner waves (engine.WaterBody waves.gerstner).",
                                json!({"type":"object"}),
                                engine_water::MAX_GERSTNER_WAVES,
                            ),
                        ),
                        (
                            "depth",
                            json!({"type":"number","minimum":0,"maximum":10000,"description":"Gerstner dispersion depth in metres (0 = deep)."}),
                        ),
                        (
                            "time",
                            json!({"type":"number","minimum":0,"maximum":100000,"default":0,"description":"Wave time of the FFT frame statistics."}),
                        ),
                    ],
                    &[],
                ),
                output: schema::output(vec![
                    ("source", json!({"type":"string"})),
                    ("model", json!({"type":"string"})),
                    ("fft", json!({"type":"object"})),
                    ("gerstner", json!({"type":"object"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            spectrum,
        ),
    ]
}

/// Prints every number that is exactly an `f32` as its shortest `f32` decimal (`0.1`, not
/// `0.10000000149011612`); the reports are built from `f32` model values.
fn shortest(value: &mut Value) {
    match value {
        Value::Number(n) => {
            if let Some(f) = n.as_f64().filter(|_| !n.is_i64() && !n.is_u64())
                && f64::from(f as f32) == f
                && let Ok(Value::Number(short)) = serde_json::from_str(&(f as f32).to_string())
            {
                *n = short;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(shortest),
        Value::Object(map) => map.values_mut().for_each(shortest),
        _ => {}
    }
}
fn report(mut value: Value) -> ToolOutcome {
    shortest(&mut value);
    Ok(ToolResult::new(value))
}

fn mapped(error: engine_water::Error) -> ToolFailure {
    ToolFailure::new(error.code, error.message)
}

fn with_scene(
    cx: &ToolContext<'_>,
    args: &Map<String, Value>,
) -> Result<scene::Loaded, ToolFailure> {
    let mut args = args.clone();
    if args.get("scene").is_none_or(Value::is_null) {
        let project = cx.project()?;
        args.insert("scene".into(), json!(super::slash(&project.startup_scene)));
    }
    scene::load(cx, &args)
}

fn sample(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let points: Vec<[f32; 3]> =
        serde_json::from_value(a.raw("points").cloned().unwrap_or(Value::Null))
            .map_err(|_| schema::invalid("points", "an array of [x, y, z] numbers"))?;
    if points.len() > MAX_SAMPLE_POINTS || points.iter().flatten().any(|v| !v.is_finite()) {
        return Err(schema::invalid(
            "points",
            "at most 256 finite [x, y, z] points",
        ));
    }
    let ticks = a.integer("ticks", 1, u64::from(MAX_SAMPLE_TICKS), 1)? as u32;
    let dt = match a.raw("dt").filter(|v| !v.is_null()) {
        None => 1.0 / 60.0,
        Some(value) => value
            .as_f64()
            .filter(|dt| (0.0001..=0.1).contains(dt))
            .ok_or_else(|| schema::invalid("dt", "a number from 0.0001 to 0.1"))?
            as f32,
    };
    let only = a.optional_string("water")?;
    let loaded = with_scene(cx, args)?;
    let mut project = loaded.project;
    project.scene = loaded.scene;
    let value =
        water::sample_scene(project, ticks, dt, &points, only).map_err(ToolFailure::from)?;
    report(super::clean(&value))
}

fn ocean_report(settings: &OceanSettings, time: f32) -> Result<Value, ToolFailure> {
    let spectrum = OceanSpectrum::new(settings, GRAVITY).map_err(mapped)?;
    let frame = spectrum.frame(time).map_err(mapped)?;
    let (lo, hi, mean_square, folded) = frame.height_stats();
    Ok(json!({
        "resolution": settings.resolution,
        "patch_length": settings.patch_length,
        "spectrum": settings.spectrum,
        "stats": spectrum.stats(),
        "frame": {
            "time": time,
            "height_range": [lo, hi],
            "rms_height": mean_square.sqrt(),
            "folded_fraction": folded,
        },
    }))
}

fn gerstner_report(waves: &[GerstnerWave], depth: f32) -> Result<Value, ToolFailure> {
    let depth = (depth > 0.0).then_some(depth);
    let set = GerstnerSet::new(waves, depth, GRAVITY).map_err(mapped)?;
    let rows: Vec<Value> = waves
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let (k, omega) = set.dispersion(i).unwrap_or((0.0, 0.0));
            json!({
                "wavelength": w.wavelength,
                "amplitude": w.amplitude,
                "steepness": w.steepness * set.steepness_scale(),
                "wavenumber": k,
                "angular_frequency": omega,
                "phase_speed": if k > 0.0 { omega / k } else { 0.0 },
                "period": if omega > 0.0 { TAU / omega } else { 0.0 },
            })
        })
        .collect();
    Ok(json!({
        "waves": rows,
        "steepness_scale": set.steepness_scale(),
        "lipschitz": set.lipschitz(),
        "max_height": set.max_height(),
        "depth": depth,
    }))
}

fn spectrum(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let time = match a.raw("time").filter(|v| !v.is_null()) {
        None => 0.0,
        Some(v) => v
            .as_f64()
            .filter(|t| (0.0..=100_000.0).contains(t))
            .ok_or_else(|| schema::invalid("time", "a number from 0 to 100000"))?
            as f32,
    };
    let depth = match a.raw("depth").filter(|v| !v.is_null()) {
        None => 0.0,
        Some(v) => v
            .as_f64()
            .filter(|d| (0.0..=10_000.0).contains(d))
            .ok_or_else(|| schema::invalid("depth", "a number from 0 to 10000"))?
            as f32,
    };
    let sources = ["water", "ocean", "gerstner"]
        .iter()
        .filter(|k| args.get(**k).is_some_and(|v| !v.is_null()))
        .count();
    if sources != 1 {
        return Err(schema::invalid(
            "water",
            "exactly one of water, ocean or gerstner",
        ));
    }
    if let Some(ocean) = args.get("ocean").filter(|v| !v.is_null()) {
        let settings: OceanSettings = serde_json::from_value(ocean.clone())
            .map_err(|e| schema::invalid("ocean", &format!("FFT ocean settings ({e})")))?;
        return report(json!({
            "source": "ocean",
            "model": "fft",
            "fft": ocean_report(&settings, time)?,
        }));
    }
    if let Some(waves) = args.get("gerstner").filter(|v| !v.is_null()) {
        let waves: Vec<GerstnerWave> = serde_json::from_value(waves.clone())
            .map_err(|e| schema::invalid("gerstner", &format!("Gerstner waves ({e})")))?;
        return report(json!({
            "source": "gerstner",
            "model": "gerstner",
            "gerstner": gerstner_report(&waves, depth)?,
        }));
    }
    let id = a.string("water")?;
    let loaded = with_scene(cx, args)?;
    let entity = loaded
        .scene
        .entities
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(|| schema::invalid("water", "the authored id of a scene entity"))?;
    let data = entity
        .components
        .custom
        .get(WATER_BODY)
        .ok_or_else(|| schema::invalid("water", "an entity with engine.WaterBody"))?;
    let body: WaterBodyComponent = serde_json::from_value(data.data.clone())
        .map_err(|e| schema::invalid("water", &format!("a valid engine.WaterBody ({e})")))?;
    let mut out = json!({"source": format!("{}#{id}", loaded.path), "model": body.waves.model});
    match body.waves.model {
        WaveModel::Fft => out["fft"] = ocean_report(&body.waves.fft, time)?,
        WaveModel::Gerstner => {
            out["gerstner"] = gerstner_report(&body.waves.gerstner, body.waves.depth)?
        }
        WaveModel::None => {}
    }
    report(out)
}
