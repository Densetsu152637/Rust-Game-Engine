//! `water_*` tools and the `engine.WaterBody` / `engine.Buoyancy` components through
//! `component_set`.
mod common;
use common::*;
use serde_json::{Value, json};
use std::fs;

const SCENE: &str = "scenes/second.json";

/// The fixture's second scene replaced by a camera, an empty `lake` entity and a `raft`.
fn water_fixture() -> tempfile::TempDir {
    let dir = fixture();
    fs::write(
        dir.path().join(SCENE),
        json!({"schema":1,"scene_id":"scene:second","entities":[
            {"id":"lake","name":"Lake","components":{"Transform":{"position":[0,1,0]}}},
            {"id":"raft","name":"Raft","components":{"Transform":{"position":[2,3,0]}}},
            {"id":"eye","name":"Eye","components":{"Transform":{"position":[0,-2,0]},"Camera3D":{}}}
        ]})
        .to_string(),
    )
    .unwrap();
    dir
}

fn set(dir: &tempfile::TempDir, entity: &str, component: &str, data: Value) -> Value {
    let result = write(
        dir.path(),
        "component_set",
        json!({"scene":SCENE,"expected_revision":revision(dir.path(), SCENE),"entity":entity,
            "component":component,"value":{"version":1,"data":data}}),
    );
    assert_eq!(result["ok"], true, "{result}");
    result
}

#[test]
fn water_components_are_editable_through_component_set() {
    let dir = water_fixture();
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"engine.WaterBody"}),
    );
    let properties = &schema["components"][0]["schema"]["properties"]["data"]["properties"];
    for key in [
        "kind",
        "half_extent",
        "waves",
        "river",
        "optics",
        "shoreline",
        "density",
        "render_mode",
        "grid_resolution",
        "visible",
    ] {
        assert!(properties.get(key).is_some(), "{key}: {schema}");
    }
    let stored = set(
        &dir,
        "lake",
        "engine.WaterBody",
        json!({"kind":"lake","half_extent":[20,20],"waves":{"model":"none"},
            "optics":{"fog_density":0.3}}),
    );
    let water = &stored["entity"]["components"]["custom"]["engine.WaterBody"]["data"];
    assert_eq!(water["kind"], "lake");
    assert_eq!(water["optics"]["fog_density"], 0.3);
    let buoyancy = set(
        &dir,
        "raft",
        "engine.Buoyancy",
        json!({"linear_drag":3.0,"water":"lake"}),
    );
    assert_eq!(
        buoyancy["entity"]["components"]["custom"]["engine.Buoyancy"]["data"]["linear_drag"],
        3.0
    );
    // Invalid data is rejected by the schema or the water validation (steepness above 1,
    // amplitude above wavelength / 14, too many buoyancy points).
    for (entity, component, data) in [
        (
            "lake",
            "engine.WaterBody",
            json!({"waves":{"gerstner":[{"wavelength":10,"steepness":2}]}}),
        ),
        (
            "lake",
            "engine.WaterBody",
            json!({"waves":{"gerstner":[{"wavelength":10,"amplitude":5}]}}),
        ),
        ("lake", "engine.WaterBody", json!({"kind":"sea"})),
        (
            "raft",
            "engine.Buoyancy",
            json!({"points":vec![[0,0,0];65]}),
        ),
    ] {
        let rejected = write(
            dir.path(),
            "component_set",
            json!({"scene":SCENE,"expected_revision":revision(dir.path(), SCENE),"entity":entity,
                "component":component,"value":{"version":1,"data":data}}),
        );
        assert_eq!(rejected["ok"], false, "{data}: {rejected}");
    }
}

/// A lake authored through component_set, sampled after simulation: the floating raft settles
/// at the surface, a point below the surface is underwater with the authored fog, a point
/// outside the lake has no water.
#[test]
fn water_sample_reports_surfaces_underwater_fog_and_buoyancy() {
    let dir = water_fixture();
    set(
        &dir,
        "lake",
        "engine.WaterBody",
        json!({"kind":"lake","half_extent":[20,20],"waves":{"model":"none"},
            "optics":{"fog_color":[0.1,0.2,0.3],"fog_density":0.5}}),
    );
    set(
        &dir,
        "raft",
        "engine.PhysicsBody3D",
        json!({"material":{"density":0.5}}),
    );
    set(&dir, "raft", "engine.Buoyancy", json!({"linear_drag":2.0}));
    let result = data(
        dir.path(),
        "water_sample",
        json!({"scene":SCENE,"ticks":300,"dt":0.02,
            "points":[[2,0,0],[0,4,0],[50,0,0],[0,1,5]]}),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["ticks"], 300);
    assert!((result["time"].as_f64().unwrap() - 6.0).abs() < 1.0e-3);
    let bodies = result["bodies"].as_array().unwrap();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["id"], "lake");
    assert_eq!(bodies[0]["kind"], "lake");
    let samples = result["samples"].as_array().unwrap();
    let below = &samples[0];
    assert_eq!(below["water"], "lake");
    assert_eq!(below["surface_height"], 1.0);
    assert_eq!(below["depth"], 1.0);
    assert_eq!(below["underwater"], true);
    assert_eq!(below["fog"]["color"], json!([0.1, 0.2, 0.3]));
    assert_eq!(below["fog"]["density"], 0.5);
    let factor = below["fog"]["factor_10m"].as_f64().unwrap();
    assert!((factor - (1.0 - (-5.0f64).exp())).abs() < 1.0e-5);
    assert_eq!(samples[1]["underwater"], false);
    assert_eq!(samples[1]["depth"], -3.0);
    assert_eq!(samples[2]["water"], Value::Null);
    assert!(samples[2].get("surface_height").is_none());
    assert_eq!(samples[3]["normal"], json!([0, 1, 0]));
    // Only one named body; unknown names fail.
    let only = data(
        dir.path(),
        "water_sample",
        json!({"scene":SCENE,"water":"pond","points":[[0,0,0]]}),
    );
    assert_eq!(error_code(&only), "water.query.invalid");
    // Bounds: too many points.
    let many: Vec<[f32; 3]> = vec![[0.0; 3]; 257];
    let rejected = data(
        dir.path(),
        "water_sample",
        json!({"scene":SCENE,"points":many}),
    );
    assert_eq!(rejected["ok"], false);
    // The raft floats where the lake's surface is: the physics query sees it near y = 1.
    let hits = data(
        dir.path(),
        "physics_query",
        json!({"scene":SCENE,"ticks":300,"dt":0.02,"kind":"ray","origin":[2,10,0],"direction":[0,-1,0]}),
    );
    assert_eq!(hits["ok"], true, "{hits}");
    let top = hits["hits"][0]["point"][1].as_f64().unwrap();
    assert!((top - 1.5).abs() < 0.05, "raft top at {top}: {hits}");
}

#[test]
fn water_spectrum_previews_ffts_and_gerstner_sets() {
    let dir = water_fixture();
    let ocean = data(
        dir.path(),
        "water_spectrum",
        json!({"ocean":{"resolution":64,"patch_length":200,"wind_speed":10,"seed":3},"time":5}),
    );
    assert_eq!(ocean["ok"], true, "{ocean}");
    assert_eq!(ocean["model"], "fft");
    let stats = &ocean["fft"]["stats"];
    let hs = stats["significant_height"].as_f64().unwrap();
    let variance = stats["variance"].as_f64().unwrap();
    assert!((hs - 4.0 * variance.sqrt()).abs() < 1.0e-3);
    // Phillips at 10 m/s: Hs near 4 sqrt(0.0081 * (100 / 9.81)^2 / 4) = 1.83 m.
    assert!((hs - 1.83).abs() < 0.2, "{hs}");
    assert_eq!(stats["energy_by_wavelength"].as_array().unwrap().len(), 16);
    assert!(ocean["fft"]["frame"]["rms_height"].as_f64().unwrap() > 0.1);
    let waves = data(
        dir.path(),
        "water_spectrum",
        json!({"gerstner":[{"wavelength":10,"amplitude":0.5,"steepness":0.8},
            {"wavelength":5,"amplitude":0.2,"steepness":0.8,"direction_degrees":90}]}),
    );
    assert_eq!(waves["ok"], true, "{waves}");
    let g = &waves["gerstner"];
    assert!((g["lipschitz"].as_f64().unwrap() - 0.9).abs() < 1.0e-5);
    let first = &g["waves"][0];
    // Deep water: c = sqrt(g L / 2 pi) = 3.951 m/s for L = 10 m.
    assert!((first["phase_speed"].as_f64().unwrap() - 3.951).abs() < 0.01);
    // From an authored body (default waves are Gerstner).
    set(&dir, "lake", "engine.WaterBody", json!({"kind":"ocean"}));
    let authored = data(
        dir.path(),
        "water_spectrum",
        json!({"scene":SCENE,"water":"lake"}),
    );
    assert_eq!(authored["ok"], true, "{authored}");
    assert_eq!(authored["model"], "gerstner");
    assert_eq!(authored["gerstner"]["waves"].as_array().unwrap().len(), 3);
    // Exactly one source.
    let none = data(dir.path(), "water_spectrum", json!({}));
    assert_eq!(none["ok"], false);
    let invalid = data(
        dir.path(),
        "water_spectrum",
        json!({"ocean":{"resolution":100}}),
    );
    assert_eq!(error_code(&invalid), "water.ocean.invalid");
}
