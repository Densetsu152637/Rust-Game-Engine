//! Headless water queries for tools: load a project's active scene, advance a bounded number
//! of ticks (no native scripts are registered, so no game code runs) and sample the water.
use super::component::{WaterBodyComponent, WaterKind, WaveModel};
use super::service::{status, water_at_filtered};
use crate::{Error, InputSnapshot, Project, Result, Runtime};
use bevy_ecs::prelude::*;
use glam::Vec3;
use serde::Serialize;

/// Most points one [`sample_scene`] call samples.
pub const MAX_SAMPLE_POINTS: usize = 256;
/// Most ticks one [`sample_scene`] call simulates.
pub const MAX_SAMPLE_TICKS: u32 = 3600;

fn invalid(message: impl Into<String>) -> Error {
    Error::new("water.query.invalid", message)
}

/// One water body in a [`SampleReport`].
#[derive(Clone, Debug, Serialize)]
pub struct SampledBody {
    pub id: String,
    pub kind: WaterKind,
    pub model: WaveModel,
    pub wave_time: f32,
    pub lipschitz: Option<f32>,
    pub steepness_scale: Option<f32>,
    pub significant_height: Option<f32>,
    pub discharge: Option<f32>,
    pub river_length: Option<f32>,
    pub shoreline_ground: bool,
    pub notes: Vec<String>,
}
/// Underwater fog of a sampled point.
#[derive(Clone, Debug, Serialize)]
pub struct SampledFog {
    pub color: [f32; 3],
    pub density: f32,
    pub transmittance: [f32; 3],
    /// `1 - exp(-density * 10 m)`.
    pub factor_10m: f32,
}
/// The water at one sampled point (`water: None` where there is none).
#[derive(Clone, Debug, Serialize)]
pub struct SampledPoint {
    pub point: [f32; 3],
    pub water: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<f32>,
    pub underwater: bool,
    pub fog: Option<SampledFog>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub velocity: Option<[f32; 3]>,
    pub crest_foam: f32,
    pub shore_foam: f32,
    pub amplitude: f32,
    pub residual: f32,
}
/// Result of [`sample_scene`].
#[derive(Clone, Debug, Serialize)]
pub struct SampleReport {
    pub ticks: u32,
    /// Water clock (s).
    pub time: f64,
    pub bodies: Vec<SampledBody>,
    pub samples: Vec<SampledPoint>,
}

/// Loads `project`'s active scene, runs `ticks` (1..=3600) ticks of `dt` seconds, then reports
/// every water body and the water at each world point (optionally only body `water`).
pub fn sample_scene(
    project: Project,
    ticks: u32,
    dt: f32,
    points: &[[f32; 3]],
    water: Option<&str>,
) -> Result<SampleReport> {
    if !(1..=MAX_SAMPLE_TICKS).contains(&ticks) {
        return Err(invalid("ticks must be 1..=3600"));
    }
    if !dt.is_finite() || !(0.0001..=0.1).contains(&dt) {
        return Err(invalid("dt must be 0.0001..=0.1 seconds"));
    }
    if points.len() > MAX_SAMPLE_POINTS || points.iter().flatten().any(|v| !v.is_finite()) {
        return Err(invalid("points: at most 256 finite [x, y, z] positions"));
    }
    let mut runtime = Runtime::new(project)?;
    let data = tempfile::tempdir()?;
    runtime.set_user_data_root(data.path().to_owned())?;
    runtime.start()?;
    let result = (|| -> Result<SampleReport> {
        for _ in 0..ticks {
            runtime.tick(dt, InputSnapshot::default())?;
        }
        let world = runtime.app().world();
        let mut bodies: Vec<(String, Entity)> = world
            .iter_entities()
            .filter(|e| e.contains::<WaterBodyComponent>())
            .filter_map(|e| {
                world
                    .get::<crate::runtime::AuthoredId>(e.id())
                    .map(|a| (a.id.clone(), e.id()))
            })
            .collect();
        bodies.sort();
        if let Some(id) = water
            && !bodies.iter().any(|(b, _)| b == id)
        {
            return Err(invalid(format!("no engine.WaterBody entity {id}")));
        }
        let listed = bodies
            .iter()
            .filter_map(|(id, e)| {
                let s = status(world, *e)?;
                Some(SampledBody {
                    id: id.clone(),
                    kind: s.kind,
                    model: s.model,
                    wave_time: s.wave_time,
                    lipschitz: s.lipschitz,
                    steepness_scale: s.steepness_scale,
                    significant_height: s.significant_height,
                    discharge: s.discharge,
                    river_length: s.river_length,
                    shoreline_ground: s.shoreline_ground,
                    notes: s.notes,
                })
            })
            .collect();
        let samples = points
            .iter()
            .map(|p| match water_at_filtered(world, Vec3::from(*p), water) {
                None => SampledPoint {
                    point: *p,
                    water: None,
                    surface_height: None,
                    depth: None,
                    underwater: false,
                    fog: None,
                    normal: None,
                    velocity: None,
                    crest_foam: 0.0,
                    shore_foam: 0.0,
                    amplitude: 0.0,
                    residual: 0.0,
                },
                Some(q) => SampledPoint {
                    point: *p,
                    water: Some(q.id),
                    surface_height: Some(q.surface_height),
                    depth: Some(q.depth),
                    underwater: q.underwater.is_some(),
                    fog: q.underwater.map(|u| SampledFog {
                        color: u.fog_color,
                        density: u.fog_density,
                        transmittance: u.transmittance,
                        factor_10m: u.fog_factor(10.0),
                    }),
                    normal: Some(q.normal.to_array()),
                    velocity: Some(q.velocity.to_array()),
                    crest_foam: q.crest_foam,
                    shore_foam: q.shore_foam,
                    amplitude: q.amplitude,
                    residual: q.residual,
                },
            })
            .collect();
        Ok(SampleReport {
            ticks,
            time: world.resource::<super::WaterClock>().time,
            bodies: listed,
            samples,
        })
    })();
    let stopped = runtime.stop();
    let value = result?;
    stopped?;
    Ok(value)
}
