//! Stream D acceptance: authored acoustic and audio components, validation,
//! world extraction, authored emitters/listeners, lifecycle, status and hooks
//! (design record tests 10, 15 and 21).
//!
//! They run against the real `engine-acoustics` world builder, material presets and
//! CPU propagation service (synchronous mode, deterministic).
use bevy_ecs::prelude::*;
use engine_acoustics::{AcousticState, PropagationResult};
use engine_audio::{AudioControl, AudioEngine, Bus, VoiceState};
use engine_project::{
    ActionFrame, CustomValue, EngineEvent, Project, Result, Runtime, Transform,
    acoustics::{self, AcousticControl},
    audio::{self, AuthoredEmitter, EntityAudio},
    register_event_handler,
};
use serde_json::{Value, json};
use std::{
    fs,
    sync::{Arc, Mutex},
};

fn wav(frames: usize) -> Vec<u8> {
    let data = frames * 2;
    let mut out = Vec::new();
    out.extend(b"RIFF");
    out.extend((36 + data as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(8000u32.to_le_bytes());
    out.extend(16000u32.to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((data as u32).to_le_bytes());
    for i in 0..frames {
        out.extend((((i % 100) as i16 - 50) * 100).to_le_bytes());
    }
    out
}
/// A unit-square pair of triangles in the XY plane, centred on the origin.
const QUAD: &str = "CGMESH 1\nvertex -0.5 -0.5 0 0 0\nvertex 0.5 -0.5 0 1 0\nvertex 0.5 0.5 0 1 1\nvertex -0.5 -0.5 0 0 0\nvertex 0.5 0.5 0 1 1\nvertex -0.5 0.5 0 0 1\n";

fn data(data: Value) -> Value {
    json!({"version":1,"data":data})
}
fn entity(id: &str, position: [f32; 3], components: Value, custom: Value) -> Value {
    let mut all = components;
    all["Transform"] = json!({"position":position});
    all["custom"] = custom;
    json!({"id":id,"name":id,"components":all})
}
fn custom_only(id: &str, custom: Value) -> Value {
    json!({"id":id,"name":id,"components":{"custom":custom}})
}

fn write_project(entities: Vec<Value>, extra_assets: Vec<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("sfx.wav"), wav(800)).unwrap();
    fs::write(dir.path().join("quad.mesh"), QUAD).unwrap();
    fs::write(dir.path().join("white.ppm"), "P3\n1 1\n255\n255 255 255\n").unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    let mut assets = vec![
        json!({"id":"sfx","kind":"audio","path":"sfx.wav"}),
        json!({"id":"quad","kind":"mesh","path":"quad.mesh"}),
        json!({"id":"tex","kind":"texture","path":"white.ppm"}),
    ];
    assets.extend(extra_assets);
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(&json!({"schema":1,"name":"acoustics-test","startup_scene":"scene.json","assets":assets})).unwrap(),
    )
    .unwrap();
    dir
}
fn load(dir: &tempfile::TempDir) -> Result<Project> {
    Project::load(dir.path().join("project.json"))
}
fn runtime(dir: &tempfile::TempDir) -> Runtime {
    Runtime::new(load(dir).unwrap()).unwrap()
}
struct Audio {
    // Declared first so the runtime drops before the engine it controls.
    rt: Runtime,
    control: AudioControl,
    _engine: AudioEngine,
    _dir: tempfile::TempDir,
}
fn with_audio(dir: tempfile::TempDir) -> Audio {
    let engine = AudioEngine::offline(Default::default()).unwrap();
    let control = engine.control();
    let mut rt = runtime(&dir);
    rt.set_audio(control.clone()).unwrap();
    Audio {
        rt,
        control,
        _engine: engine,
        _dir: dir,
    }
}
fn tick(rt: &mut Runtime, dt: f32) {
    rt.tick(dt, ActionFrame::default()).unwrap();
}
fn code(error: engine_project::Error) -> String {
    error.diagnostics[0].code.clone()
}
fn mesh_lookup(
    dir: &tempfile::TempDir,
) -> impl Fn(&str) -> Option<Arc<engine_project::content::Mesh>> {
    let path = dir.path().join("quad.mesh");
    let mesh = Arc::new(engine_project::content::load_mesh(&path).unwrap());
    move |id| (id == "quad").then(|| mesh.clone())
}

// ---- catalog, hydration and validation (design test 10 and 21) ------------------------------

#[test]
fn all_acoustic_and_audio_components_are_in_the_catalog() {
    let catalog = engine_project::ComponentCatalog::new(vec![]).unwrap();
    for name in acoustics::NAMES {
        let d = &catalog.0[name];
        assert_eq!(d.version, 1);
        assert!(!d.data_only);
        // Defaults validate structurally; an emitter still needs a source at runtime.
        let value = CustomValue {
            version: 1,
            data: d.defaults.clone(),
        };
        let result = catalog.validate(name, &value);
        assert!(result.is_ok(), "{name}");
    }
    assert!(
        catalog.0[acoustics::MATERIAL]
            .requires
            .contains(&"Transform".to_owned())
    );
    assert!(catalog.0[acoustics::ENVIRONMENT].requires.is_empty());
}

#[test]
fn authored_components_hydrate_and_round_trip_through_snapshots() {
    let custom_env = json!({"engine.AcousticEnvironment":data(json!({"renderer":"stereo","seed":7,"quality":{"occlusion_rays":8},"geometry":{"default_material":"concrete"}}))});
    let dir = write_project(
        vec![
            custom_only("env", custom_env),
            entity(
                "wall",
                [0., 0., -5.],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({
                    "engine.AcousticMaterial":data(json!({"preset":"glass","absorption":[0.1,0.2,0.3,0.4],"transmission":[0.05,0.1,0.1,0.2]})),
                    "engine.AcousticGeometry":data(json!({"mode":"dynamic","mask":3})),
                }),
            ),
            entity(
                "room",
                [0., 0., 0.],
                json!({}),
                json!({"engine.AcousticVolume":data(json!({"half_extents":[5,3,4],"kind":"room","open_faces":["+y"]}))}),
            ),
            entity(
                "speaker",
                [1., 0., 0.],
                json!({}),
                json!({
                    "engine.AudioEmitter":data(json!({"clip":"sfx","looped":true,"bus":"music","gain":0.5})),
                    "engine.AcousticSource":data(json!({"reverb_send":0.5,"hero":true})),
                }),
            ),
            entity(
                "ears",
                [0., 0., 0.],
                json!({}),
                json!({"engine.AudioListener":data(json!({}))}),
            ),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    tick(&mut rt, 0.016);
    let snapshot = rt.snapshot();
    let custom = |id: &str, name: &str| -> Value {
        snapshot
            .scene
            .entities
            .iter()
            .find(|e| e.id == id)
            .unwrap()
            .components
            .custom[name]
            .data
            .clone()
    };
    let env = custom("env", acoustics::ENVIRONMENT);
    assert_eq!(env["renderer"], "stereo");
    assert_eq!(env["seed"], 7);
    assert_eq!(env["quality"]["occlusion_rays"], 8);
    assert_eq!(env["quality"]["reverb_rays"], 4096);
    assert_eq!(env["geometry"]["default_material"], "concrete");
    assert_eq!(env["enabled"], true);
    assert!(env.get("proxy2d").is_none(), "unset optionals are omitted");
    let material = custom("wall", acoustics::MATERIAL);
    assert_eq!(material["preset"], "glass");
    let transmission: Vec<f64> = material["transmission"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    for (got, want) in transmission.iter().zip([0.05, 0.1, 0.1, 0.2]) {
        assert!((got - want).abs() < 1e-6, "{transmission:?}");
    }
    assert!(material.get("scattering").is_none());
    assert_eq!(custom("wall", acoustics::GEOMETRY)["mode"], "dynamic");
    assert_eq!(
        custom("room", acoustics::VOLUME)["open_faces"],
        json!(["+y"])
    );
    assert_eq!(custom("speaker", acoustics::SOURCE)["hero"], true);
    assert_eq!(custom("speaker", acoustics::EMITTER)["bus"], "music");
    assert_eq!(custom("ears", acoustics::LISTENER)["active"], true);
}

fn invalid_cases() -> Vec<(&'static str, Value)> {
    vec![
        (acoustics::MATERIAL, json!({"absorption":[0.0,0.0,0.0,1.5]})),
        (
            acoustics::MATERIAL,
            json!({"transmission":[-0.1,0.0,0.0,0.0]}),
        ),
        (
            acoustics::MATERIAL,
            json!({"absorption":[0.1,0.1,0.1,0.1],"transmission":[0.2,0.0,0.0,0.0]}),
        ),
        (acoustics::MATERIAL, json!({"preset":"marble"})),
        (acoustics::MATERIAL, json!({"scattering":1.5})),
        (acoustics::MATERIAL, json!({"bogus":1})),
        (acoustics::GEOMETRY, json!({"mode":"moving"})),
        (acoustics::GEOMETRY, json!({"mask":0})),
        (acoustics::VOLUME, json!({"half_extents":[0,1,1]})),
        (acoustics::VOLUME, json!({"shape":"mesh"})),
        (acoustics::VOLUME, json!({"open_faces":["+y","+y"]})),
        (
            acoustics::VOLUME,
            json!({"kind":"solid","open_faces":["+y"]}),
        ),
        (acoustics::VOLUME, json!({"space":"proxy2d"})),
        (acoustics::SOURCE, json!({"doppler":5})),
        (acoustics::SOURCE, json!({"reverb_send":-1})),
        (acoustics::SOURCE, json!({"radius_m":50})),
        (acoustics::EMITTER, json!({"clip":"sfx","graph":"both"})),
        (acoustics::EMITTER, json!({"clip":"sfx","gain":2})),
        (acoustics::EMITTER, json!({"clip":"sfx","pitch":0.01})),
        (acoustics::EMITTER, json!({"clip":"sfx","bus":"master"})),
        (
            acoustics::EMITTER,
            json!({"clip":"sfx","spatial":{"min_distance":5,"max_distance":2}}),
        ),
        (acoustics::LISTENER, json!({"extra":true})),
        (acoustics::ENVIRONMENT, json!({"update_hz":0})),
        (acoustics::ENVIRONMENT, json!({"speed_of_sound":10})),
        (acoustics::ENVIRONMENT, json!({"backend":"vulkan"})),
        (
            acoustics::ENVIRONMENT,
            json!({"quality":{"reverb_rays":10}}),
        ),
        (
            acoustics::ENVIRONMENT,
            json!({"backend":"cpu","quality":{"reverb_rays":100000}}),
        ),
        (
            acoustics::ENVIRONMENT,
            json!({"quality":{"late_start_ms":3000}}),
        ),
        (
            acoustics::ENVIRONMENT,
            json!({"air":{"humidity_percent":150}}),
        ),
        (acoustics::ENVIRONMENT, json!({"geometry":{"mask":0}})),
        (
            acoustics::ENVIRONMENT,
            json!({"proxy2d":{"units_per_meter":0}}),
        ),
        (
            acoustics::ENVIRONMENT,
            json!({"proxy2d":{"enclosure":"bounds"}}),
        ),
        (
            acoustics::ENVIRONMENT,
            json!({"proxy2d":{"enclosure":"bounds","bounds":[5,0,1,1]}}),
        ),
        (
            acoustics::ENVIRONMENT,
            json!({"proxy2d":{"bounds":[0,0,1,1]}}),
        ),
        (
            acoustics::ENVIRONMENT,
            json!({"proxy2d":{"ear_height_m":9}}),
        ),
    ]
}

#[test]
fn invalid_component_values_are_rejected_with_a_code() {
    let catalog = engine_project::ComponentCatalog::new(vec![]).unwrap();
    for (name, payload) in invalid_cases() {
        let error = catalog
            .validate(
                name,
                &CustomValue {
                    version: 1,
                    data: payload.clone(),
                },
            )
            .expect_err(&format!("{name} {payload}"));
        let code = code(error);
        assert!(
            [
                "component.schema.invalid",
                "acoustics.component.invalid",
                "audio.emitter.invalid"
            ]
            .contains(&code.as_str()),
            "{name} {payload}: {code}"
        );
    }
    // A wrong component version is rejected too.
    assert!(
        catalog
            .validate(
                acoustics::LISTENER,
                &CustomValue {
                    version: 2,
                    data: json!({})
                }
            )
            .is_err()
    );
}

#[test]
fn valid_edge_values_are_accepted() {
    let catalog = engine_project::ComponentCatalog::new(vec![]).unwrap();
    for (name, payload) in [
        (
            acoustics::MATERIAL,
            json!({"absorption":[0,0,0,1],"transmission":[0,0,0,1]}),
        ),
        (
            acoustics::MATERIAL,
            json!({"preset":"absorber","scattering":1}),
        ),
        (
            acoustics::ENVIRONMENT,
            json!({"air":{"enabled":false},"proxy2d":{"plane":"top_down","units_per_meter":0.5,"enclosure":"bounds","bounds":[0,0,10,6],"open_sky":true}}),
        ),
        (
            acoustics::ENVIRONMENT,
            json!({"backend":"gpu_ray_query","quality":{"reverb_rays":1048576}}),
        ),
        (
            acoustics::VOLUME,
            json!({"space":"proxy2d","kind":"solid","half_extents":[2,1,1]}),
        ),
        (
            acoustics::EMITTER,
            json!({"clip":"sfx","play_on":"trigger:door:enter","stop_on":"stop"}),
        ),
    ] {
        let value = CustomValue {
            version: 1,
            data: payload.clone(),
        };
        catalog
            .validate(name, &value)
            .unwrap_or_else(|e| panic!("{name} {payload}: {e}"));
    }
}

#[test]
fn a_second_environment_is_rejected_by_scene_validation() {
    let env = || json!({"engine.AcousticEnvironment":data(json!({}))});
    let dir = write_project(
        vec![custom_only("a", env()), custom_only("b", env())],
        vec![],
    );
    assert_eq!(
        code(load(&dir).unwrap_err()),
        "acoustics.environment.multiple"
    );
    let one = write_project(vec![custom_only("a", env())], vec![]);
    assert!(load(&one).is_ok());
}

#[test]
fn asset_references_must_have_the_right_kind() {
    let emitter = |clip: &str| {
        entity(
            "e",
            [0.; 3],
            json!({}),
            json!({"engine.AudioEmitter":data(json!({"clip":clip}))}),
        )
    };
    assert!(load(&write_project(vec![emitter("sfx")], vec![])).is_ok());
    assert_eq!(
        code(load(&write_project(vec![emitter("tex")], vec![])).unwrap_err()),
        "project.asset.kind"
    );
    assert_eq!(
        code(load(&write_project(vec![emitter("nope")], vec![])).unwrap_err()),
        "project.asset.unknown"
    );
    let geometry = |mesh: &str| {
        entity(
            "g",
            [0.; 3],
            json!({}),
            json!({"engine.AcousticGeometry":data(json!({"mesh":mesh}))}),
        )
    };
    assert!(load(&write_project(vec![geometry("quad")], vec![])).is_ok());
    assert_eq!(
        code(load(&write_project(vec![geometry("sfx")], vec![])).unwrap_err()),
        "project.asset.kind"
    );
    let baked = |asset: &str| {
        custom_only(
            "env",
            json!({"engine.AcousticEnvironment":data(json!({"geometry":{"asset":asset}}))}),
        )
    };
    assert_eq!(
        code(load(&write_project(vec![baked("quad")], vec![])).unwrap_err()),
        "project.asset.kind"
    );
}

#[test]
fn acoustics_assets_are_a_validated_kind() {
    let write = |content: &str| {
        let dir = write_project(
            vec![custom_only(
                "env",
                json!({"engine.AcousticEnvironment":data(json!({"geometry":{"asset":"baked"}}))}),
            )],
            vec![json!({"id":"baked","kind":"acoustics","path":"baked.json"})],
        );
        fs::write(dir.path().join("baked.json"), content).unwrap();
        dir
    };
    assert!(load(&write(r#"{"version":1,"materials":[]}"#)).is_ok());
    assert_eq!(
        code(load(&write(r#"{"materials":[]}"#)).unwrap_err()),
        "acoustics.document.invalid"
    );
    assert!(load(&write("not json")).is_err());
    // The document is replayed through the world builder, so its rules apply.
    assert_eq!(
        code(
            load(&write(
                r#"{"version":1,"materials":[{"absorption":[2,0,0,0],"scattering":0,"transmission":[0,0,0,0]}]}"#
            ))
            .unwrap_err()
        ),
        "acoustics.material.invalid"
    );
    assert_eq!(
        code(load(&write(r#"{"version":2,"materials":[]}"#)).unwrap_err()),
        "acoustics.world.invalid"
    );
}

#[test]
fn scene_rules_mention_the_environment_limit() {
    assert!(
        engine_project::SCENE_RULES
            .iter()
            .any(|r| r.contains("AcousticEnvironment"))
    );
}

// ---- mixer and tracing configuration ---------------------------------------------------------

#[test]
fn defaults_choose_the_binaural_renderer_and_parametric_reverb() {
    let env = acoustics::AcousticEnvironment::default();
    let mixer = acoustics::mixer_environment(&env);
    assert_eq!(
        mixer.renderer,
        engine_audio::acoustic::SpatialRenderer::Binaural
    );
    assert_eq!(
        mixer.reverb,
        engine_audio::acoustic::ReverbRenderer::Parametric
    );
    assert_eq!(mixer.speed_of_sound, 343.0);
    assert!(mixer.air_np_per_m.iter().all(|v| *v > 0.0));
    let off = acoustics::AcousticEnvironment {
        reverb_source: acoustics::ReverbSource::Off,
        renderer: acoustics::Renderer::Stereo,
        ..env.clone()
    };
    let mixer = acoustics::mixer_environment(&off);
    assert_eq!(mixer.reverb, engine_audio::acoustic::ReverbRenderer::Off);
    assert_eq!(
        mixer.renderer,
        engine_audio::acoustic::SpatialRenderer::StereoPan
    );
    let settings = acoustics::propagation_settings(&env);
    assert_eq!(settings.er_rays, 256);
    assert_eq!(settings.histogram_bin_s, 0.001);
    assert_eq!(settings.late_start_s, 0.08);
    assert_eq!(settings.time_budget_us, 4000);
    let dry = acoustics::AcousticEnvironment {
        air: acoustics::Air {
            enabled: false,
            ..Default::default()
        },
        ..env
    };
    assert_eq!(acoustics::propagation_settings(&dry).air_np_per_m, [0.0; 4]);
}

#[test]
fn air_absorption_follows_iso_9613_1() {
    // ISO 9613-1 at 20 C, 50 % relative humidity, 101.325 kPa: about 4.7 dB/km at 1 kHz.
    let at = |f| acoustics::iso9613_db_per_m(f, 20.0, 50.0, 101.325) * 1000.0;
    assert!((3.5..6.0).contains(&at(1000.0)), "{}", at(1000.0));
    assert!(at(125.0) < at(500.0) && at(500.0) < at(2000.0) && at(2000.0) < at(8000.0));
    assert!((60.0..160.0).contains(&at(8000.0)), "{}", at(8000.0));
    // Dry air absorbs high frequencies far more than humid air at 8 kHz.
    assert!(
        acoustics::iso9613_db_per_m(8000.0, 20.0, 10.0, 101.325)
            > acoustics::iso9613_db_per_m(8000.0, 20.0, 70.0, 101.325)
    );
    let explicit = acoustics::Air {
        absorption_db_per_m: Some([0.0, 0.0, 8.685_89, 0.0]),
        ..Default::default()
    };
    let np = acoustics::air_np_per_m(&explicit);
    assert!((np[2] - 1.0).abs() < 1e-4 && np[0] == 0.0);
}

// ---- hysteresis ----------------------------------------------------------------------------

#[test]
fn occlusion_uses_hysteresis_and_reports_each_crossing_once() {
    let mut occlusion = acoustics::Occlusion::default();
    let mut crossings = vec![];
    for visible in [
        1.0, 0.6, 0.31, 0.29, 0.1, 0.5, 0.69, 0.71, 0.9, 0.5, 0.2, 0.9,
    ] {
        if let Some(state) = occlusion.update(5, visible) {
            crossings.push((visible, state));
        }
    }
    assert_eq!(
        crossings,
        vec![(0.29, true), (0.71, false), (0.2, true), (0.9, false)]
    );
    assert!(!occlusion.is_occluded(5));
    occlusion.update(9, 0.0);
    occlusion.retain(|id| id == 5);
    assert!(!occlusion.is_occluded(9));
    // A source first seen behind a wall reports once.
    assert_eq!(occlusion.update(11, 0.0), Some(true));
    assert_eq!(occlusion.update(11, 0.0), None);
}

// ---- world extraction (design tests 10 and 21) -----------------------------------------------

fn collect_world(rt: &Runtime, dir: &tempfile::TempDir) -> acoustics::Collected {
    let lookup = mesh_lookup(dir);
    acoustics::collect(rt.app().world(), &lookup)
        .unwrap()
        .unwrap()
}
fn env_entity(env: Value) -> Value {
    custom_only("env", json!({"engine.AcousticEnvironment":data(env)}))
}

#[test]
fn mesh_renderers_become_instances_with_world_transforms_and_materials() {
    let mut transformed = entity(
        "wall",
        [3., 1., -2.],
        json!({"MeshRenderer":{"mesh":"quad"}}),
        json!({"engine.AcousticMaterial":data(json!({"preset":"brick"}))}),
    );
    transformed["components"]["Transform"]["scale"] = json!([2., 4., 1.]);
    let dir = write_project(
        vec![
            env_entity(json!({"geometry":{"default_material":"concrete","mask":15}})),
            transformed,
            entity(
                "plain",
                [0., 0., 0.],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({}),
            ),
            entity(
                "hidden",
                [0., 0., 0.],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({"engine.AcousticGeometry":data(json!({"mode":"exclude"}))}),
            ),
            entity(
                "moving",
                [0., 0., 7.],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({"engine.AcousticGeometry":data(json!({"mode":"dynamic","mask":2}))}),
            ),
            entity("no-mesh", [0., 0., 0.], json!({}), json!({})),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    rt.start().unwrap();
    let world = collect_world(&rt, &dir);
    let keys: Vec<_> = world.instances.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(
        keys,
        ["moving", "plain", "wall"],
        "sorted by authored ID, exclude dropped"
    );
    let wall = &world.instances[2];
    // Row-major 3x4: scale on the diagonal, translation in the last column.
    assert_eq!(
        wall.transform,
        [2., 0., 0., 3., 0., 4., 0., 1., 0., 0., 1., -2.]
    );
    assert_eq!(
        wall.mask, 15,
        "environment mask applies without AcousticGeometry"
    );
    assert_eq!(world.instances[0].mask, 2);
    assert_eq!(world.instances[0].mobility, acoustics::Mobility::Dynamic);
    assert_eq!(world.instances[1].mobility, acoustics::Mobility::Static);
    // concrete is the default (material 0); brick is distinct; both are reused.
    assert_eq!(world.materials.len(), 2);
    assert_eq!(
        world.materials[world.instances[1].material].preset,
        acoustics::Preset::Concrete
    );
    assert_eq!(
        world.materials[wall.material].preset,
        acoustics::Preset::Brick
    );
    assert_eq!(world.meshes.len(), 1);
    assert!(world.footprints.is_empty() && world.proxy.is_none());
}

#[test]
fn render_meshes_can_be_disabled_and_an_acoustic_mesh_substitutes() {
    let dir = write_project(
        vec![
            env_entity(json!({"geometry":{"render_meshes":false}})),
            entity(
                "rendered",
                [0.; 3],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({}),
            ),
            entity(
                "proxy-mesh",
                [1., 0., 0.],
                json!({}),
                json!({"engine.AcousticGeometry":data(json!({"mesh":"quad"}))}),
            ),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    rt.start().unwrap();
    let world = collect_world(&rt, &dir);
    let keys: Vec<_> = world.instances.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(keys, ["proxy-mesh"]);
}

#[test]
fn volumes_are_collected_with_open_face_masks() {
    let dir = write_project(
        vec![
            env_entity(json!({})),
            entity(
                "hall",
                [10., 0., 0.],
                json!({}),
                json!({
                    "engine.AcousticVolume":data(json!({"half_extents":[5,3,4],"kind":"room","open_faces":["-x","+z"],"mobility":"dynamic"})),
                    "engine.AcousticMaterial":data(json!({"preset":"plaster"})),
                }),
            ),
            entity(
                "block",
                [0.; 3],
                json!({}),
                json!({"engine.AcousticVolume":data(json!({"kind":"solid"}))}),
            ),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    rt.start().unwrap();
    let world = collect_world(&rt, &dir);
    assert_eq!(world.volumes.len(), 2);
    let hall = world.volumes.iter().find(|v| v.key == "hall").unwrap();
    assert_eq!(hall.open_mask, 0b10_0001);
    assert_eq!(hall.transform[3], 10.0);
    assert_eq!(hall.mobility, acoustics::Mobility::Dynamic);
    assert_eq!(
        world.materials[hall.material].preset,
        acoustics::Preset::Plaster
    );
    let block = world.volumes.iter().find(|v| v.key == "block").unwrap();
    assert_eq!(block.kind, acoustics::VolumeKind::Solid);
}

#[test]
fn proxy_footprints_come_from_colliders_bodies_and_proxy_volumes() {
    let mut tilted = entity(
        "tilted",
        [4., 2., 0.],
        json!({"Collider2D":{"size":[6.0,2.0],"offset":[1.0,0.0]}}),
        json!({}),
    );
    tilted["components"]["Transform"]["rotation"] =
        json!(glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2).to_array());
    let dir = write_project(
        vec![
            env_entity(json!({"proxy2d":{"units_per_meter":2,"material":"brick"}})),
            tilted,
            entity(
                "trigger",
                [0.; 3],
                json!({"Collider2D":{"size":[1.0,1.0],"trigger":true}}),
                json!({}),
            ),
            entity(
                "ball",
                [1., 1., 0.],
                json!({}),
                json!({"engine.PhysicsBody2D":data(json!({"shape":"ball","radius":0.75,"motion":"dynamic"}))}),
            ),
            entity(
                "capsule",
                [2., 1., 0.],
                json!({}),
                json!({"engine.PhysicsBody2D":data(json!({"shape":"capsule","radius":0.25,"half_height":1.0,"motion":"fixed"}))}),
            ),
            entity(
                "sensor",
                [3., 1., 0.],
                json!({}),
                json!({"engine.PhysicsBody2D":data(json!({"material":{"sensor":true}}))}),
            ),
            entity(
                "panel",
                [8., 1., 0.],
                json!({}),
                json!({"engine.AcousticVolume":data(json!({"space":"proxy2d","kind":"solid","half_extents":[1.0,2.0,1.0]}))}),
            ),
            entity(
                "ignored-mesh",
                [0.; 3],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({}),
            ),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    rt.start().unwrap();
    let world = collect_world(&rt, &dir);
    let keys: Vec<_> = world.footprints.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["ball", "capsule", "panel", "tilted"]);
    let by = |key: &str| world.footprints.iter().find(|f| f.key == key).unwrap();
    // A 90 degree rotation sends the local offset (1, 0) to (0, 1) and keeps the box.
    match &by("tilted").shape {
        acoustics::FootprintShape::Rect {
            center,
            half_extents,
            rotation,
        } => {
            assert!(
                (center[0] - 4.0).abs() < 1e-4 && (center[1] - 3.0).abs() < 1e-4,
                "{center:?}"
            );
            assert!((half_extents[0] - 3.0).abs() < 1e-4 && (half_extents[1] - 1.0).abs() < 1e-4);
            assert!((rotation - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        by("ball").shape,
        acoustics::FootprintShape::Circle {
            center: [1., 1.],
            radius: 0.75
        }
    );
    assert_eq!(by("ball").mobility, acoustics::Mobility::Dynamic);
    assert_eq!(by("capsule").mobility, acoustics::Mobility::Static);
    assert!(matches!(
        by("capsule").shape,
        acoustics::FootprintShape::Capsule { half_height, radius, .. } if half_height == 1.0 && radius == 0.25
    ));
    assert_eq!(
        by("panel").shape,
        acoustics::FootprintShape::Rect {
            center: [8., 1.],
            half_extents: [1., 2.],
            rotation: 0.0
        }
    );
    // Footprints without a material use the proxy material.
    assert_eq!(
        world.materials[by("ball").material].preset,
        acoustics::Preset::Brick
    );
    assert!(world.instances.is_empty());
    assert!(world.warnings.iter().any(|w| w.contains("MeshRenderer")));
    let proxy = world.proxy.as_ref().unwrap();
    assert_eq!(proxy.settings.units_per_meter, 2.0);
    assert_eq!(
        rt.app().world().resource::<acoustics::AcousticUnits>().0,
        0.5,
        "voices measure 0.5 m per unit"
    );
}

#[test]
fn rich_physics_shapes_contribute_polygons_or_a_named_warning() {
    let body = |id: &str, body: Value| {
        entity(
            id,
            [10., 0., 0.],
            json!({}),
            json!({"engine.PhysicsBody2D":data(body)}),
        )
    };
    let dir = write_project(
        vec![
            env_entity(json!({"proxy2d":{"units_per_meter":1,"material":"brick"}})),
            body(
                "hull",
                json!({"shape":"convex_hull","motion":"fixed",
                    "points":[[0.,0.],[2.,0.],[2.,1.],[0.,1.],[1.,0.5]]}),
            ),
            body(
                "mesh",
                json!({"shape":"trimesh","motion":"fixed",
                    "vertices":[[0.,0.],[1.,0.],[1.,1.],[0.,1.]],
                    "indices":[[0,1,2],[0,2,3]]}),
            ),
            body(
                "pair",
                json!({"shape":"compound","motion":"fixed","parts":[
                    {"shape":"box","position":[-2.,0.],"half_extents":[0.5,0.5]},
                    {"shape":"ball","position":[2.,0.],"radius":0.5}]}),
            ),
            body(
                "ridge",
                json!({"shape":"heightfield","motion":"fixed","heights":[0.,1.,0.],"heightfield_scale":[1.,1.]}),
            ),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    rt.start().unwrap();
    let world = collect_world(&rt, &dir);
    let of = |key: &str| -> Vec<_> { world.footprints.iter().filter(|f| f.key == key).collect() };
    // The hull's interior point is dropped; its footprint is the translated rectangle.
    let hull = of("hull");
    assert_eq!(hull.len(), 1);
    match &hull[0].shape {
        acoustics::FootprintShape::Polygon { points } => {
            assert_eq!(points.len(), 4, "{points:?}");
            for p in points {
                assert!((10.0..=12.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1]));
            }
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(hull[0].mobility, acoustics::Mobility::Static);
    // Each trimesh triangle is a polygon footprint.
    assert_eq!(of("mesh").len(), 2);
    assert!(of("mesh").iter().all(
        |f| matches!(&f.shape, acoustics::FootprintShape::Polygon { points } if points.len() == 3)
    ));
    // Compound parts contribute their own footprints at their local offsets.
    let pair = of("pair");
    assert_eq!(pair.len(), 2, "box and ball parts");
    assert_eq!(
        pair[1].shape,
        acoustics::FootprintShape::Circle {
            center: [12., 0.],
            radius: 0.5
        }
    );
    // Shapes with no planar footprint are named, never dropped silently.
    assert!(of("ridge").is_empty());
    assert!(
        world
            .warnings
            .iter()
            .any(|w| w.starts_with("ridge:") && w.contains("heightfield")),
        "{:?}",
        world.warnings
    );
}

#[test]
fn oversized_2d_hulls_and_trimeshes_are_simplified_or_capped_with_warnings() {
    let circle: Vec<[f32; 2]> = (0..64)
        .map(|i| {
            let a = i as f32 / 64. * std::f32::consts::TAU;
            [a.cos(), a.sin()]
        })
        .collect();
    // An authored component holds at most 64 KiB, so one trimesh stays well under the 8192
    // footprint limit; several of them (here 4 x 2100 triangles) exceed it together.
    let (columns, rows) = (51usize, 22usize);
    let vertices: Vec<[f32; 2]> = (0..rows)
        .flat_map(|y| (0..columns).map(move |x| [x as f32, y as f32]))
        .collect();
    let mut indices = vec![];
    for y in 0..rows - 1 {
        for x in 0..columns - 1 {
            let a = (y * columns + x) as u32;
            let (b, c, d) = (a + 1, a + columns as u32, a + columns as u32 + 1);
            indices.push([a, b, d]);
            indices.push([a, d, c]);
        }
    }
    assert_eq!(indices.len(), 2100);
    let terrain = |name: &str, x: f32| {
        entity(
            name,
            [x, 0., 0.],
            json!({}),
            json!({"engine.PhysicsBody2D":data(json!({
                "shape":"trimesh","motion":"fixed",
                "vertices":vertices,"indices":indices}))}),
        )
    };
    let dir = write_project(
        vec![
            env_entity(json!({"proxy2d":{"units_per_meter":1,"material":"brick"}})),
            entity(
                "round",
                [0., 0., 0.],
                json!({}),
                json!({"engine.PhysicsBody2D":data(json!({
                    "shape":"convex_hull","motion":"fixed","points":circle}))}),
            ),
            terrain("t0", 100.),
            terrain("t1", 200.),
            terrain("t2", 300.),
            terrain("t3", 400.),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    rt.start().unwrap();
    let world = collect_world(&rt, &dir);
    let round: Vec<_> = world
        .footprints
        .iter()
        .filter(|f| f.key == "round")
        .collect();
    assert_eq!(round.len(), 1);
    match &round[0].shape {
        acoustics::FootprintShape::Polygon { points } => assert_eq!(points.len(), 32),
        other => panic!("{other:?}"),
    }
    assert!(
        world
            .warnings
            .iter()
            .any(|w| w.starts_with("round:") && w.contains("simplified from 64")),
        "{:?}",
        world.warnings
    );
    assert_eq!(
        world.footprints.len(),
        8192,
        "8401 footprints cut to the limit"
    );
    assert!(
        world
            .warnings
            .iter()
            .any(|w| w.contains("209 proxy2d footprints beyond the limit of 8192")),
        "{:?}",
        world.warnings
    );
    // The proxy world builds instead of failing the whole acoustic build.
    acoustics::build_world(&world).expect("oversized footprints no longer fail the build");
}

#[test]
fn signature_tracks_static_changes_but_not_dynamic_motion() {
    let dir = write_project(
        vec![
            env_entity(json!({})),
            entity(
                "floor",
                [0.; 3],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({}),
            ),
            entity(
                "door",
                [5., 0., 0.],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({"engine.AcousticGeometry":data(json!({"mode":"dynamic"}))}),
            ),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    rt.start().unwrap();
    let before = collect_world(&rt, &dir);
    let door = rt.entity("door").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<Transform>(door)
        .unwrap()
        .position = [9., 0., 0.];
    tick(&mut rt, 0.016);
    let moved = collect_world(&rt, &dir);
    assert_eq!(
        before.signature(),
        moved.signature(),
        "dynamic motion needs no rebuild"
    );
    assert_ne!(before.dynamic_transforms(), moved.dynamic_transforms());
    assert_eq!(moved.dynamic_transforms()["door"][3], 9.0);
    let floor = rt.entity("floor").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<Transform>(floor)
        .unwrap()
        .position = [0., 1., 0.];
    tick(&mut rt, 0.016);
    assert_ne!(before.signature(), collect_world(&rt, &dir).signature());
    // Removing an entity removes its geometry.
    rt.app_mut().world_mut().despawn(floor);
    tick(&mut rt, 0.016);
    let after = collect_world(&rt, &dir);
    assert_eq!(after.instances.len(), 1);
    assert_eq!(after.instances[0].key, "door");
}

#[test]
fn extraction_reports_invalid_meshes_and_a_missing_environment() {
    let dir = write_project(
        vec![entity(
            "a",
            [0.; 3],
            json!({"MeshRenderer":{"mesh":"quad"}}),
            json!({}),
        )],
        vec![],
    );
    let mut rt = runtime(&dir);
    rt.start().unwrap();
    let lookup = mesh_lookup(&dir);
    assert!(
        acoustics::collect(rt.app().world(), &lookup)
            .unwrap()
            .is_none()
    );
    let disabled = write_project(vec![env_entity(json!({"enabled":false}))], vec![]);
    let mut rt = runtime(&disabled);
    rt.start().unwrap();
    assert!(
        acoustics::collect(rt.app().world(), &lookup)
            .unwrap()
            .is_none()
    );
    // A mesh asset that the lookup cannot resolve is a coded error, not a partial world.
    let dir = write_project(
        vec![
            env_entity(json!({})),
            entity(
                "a",
                [0.; 3],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({}),
            ),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    rt.start().unwrap();
    let error = acoustics::collect(rt.app().world(), &|_| None)
        .err()
        .unwrap();
    assert_eq!(code(error), "acoustics.world.invalid");
}

// ---- lifecycle, status and events ------------------------------------------------------------

#[test]
fn status_is_absent_without_an_environment_and_disabled_when_switched_off() {
    let dir = write_project(vec![entity("a", [0.; 3], json!({}), json!({}))], vec![]);
    let mut rt = runtime(&dir);
    tick(&mut rt, 0.016);
    assert!(rt.acoustics_status().is_none());
    assert!(rt.snapshot().acoustics.is_none());
    assert!(
        serde_json::to_value(rt.snapshot())
            .unwrap()
            .get("acoustics")
            .is_none()
    );

    let off = write_project(
        vec![env_entity(json!({"enabled":false,"backend":"cpu"}))],
        vec![],
    );
    let mut rt = runtime(&off);
    tick(&mut rt, 0.016);
    let status = rt.acoustics_status().unwrap();
    assert_eq!(status.state, AcousticState::Disabled);
    assert_eq!(status.requested, engine_acoustics::BackendPreference::Cpu);
    assert!(
        rt.app()
            .world()
            .get_resource::<acoustics::AcousticUnits>()
            .is_none()
    );
}

#[test]
fn an_environment_populates_a_bounded_status_without_failing_the_runtime() {
    let dir = write_project(
        vec![
            env_entity(json!({"backend":"cpu"})),
            entity(
                "wall",
                [0., 0., -5.],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({}),
            ),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    for _ in 0..5 {
        tick(&mut rt, 0.016);
    }
    let snapshot = rt.snapshot();
    let status = snapshot.acoustics.as_ref().expect("environment present");
    assert_eq!(status.requested, engine_acoustics::BackendPreference::Cpu);
    // While engine-acoustics is a contract skeleton the service cannot be created;
    // the status then says so with the coded reason. With the real crates it is
    // active (CPU) once a world exists.
    match status.state {
        AcousticState::Failed => {
            let reason = status.fallback_reason.as_deref().unwrap();
            assert!(reason.starts_with("acoustics."), "{reason}");
            assert!(reason.len() <= 512);
        }
        AcousticState::Starting | AcousticState::Active | AcousticState::Fallback => {}
        AcousticState::Disabled => panic!("enabled environment reported disabled"),
    }
    let json = serde_json::to_vec(&snapshot.acoustics).unwrap();
    assert!(json.len() < 64 * 1024);
    assert!(status.sources.len() <= 32);
    // The control resource mirrors the snapshot status.
    let control = rt.app().world().resource::<AcousticControl>();
    assert_eq!(control.status.state, status.state);
}

#[test]
fn set_acoustics_is_configured_before_startup_only() {
    let dir = write_project(vec![env_entity(json!({}))], vec![]);
    let mut rt = runtime(&dir);
    rt.set_acoustics(vec![], engine_acoustics::ServiceMode::Threaded)
        .unwrap();
    tick(&mut rt, 0.016);
    let error = rt
        .set_acoustics(vec![], engine_acoustics::ServiceMode::Synchronous)
        .unwrap_err();
    assert_eq!(code(error), "runtime.started");
}

#[derive(Resource, Default)]
struct Seen(Vec<(String, Value)>);
fn record(world: &mut World, _: Entity, event: &EngineEvent) -> Result<()> {
    world
        .resource_mut::<Seen>()
        .0
        .push((event.name.clone(), event.payload.clone()));
    Ok(())
}
fn listening_entity() -> Value {
    let mut listener = entity("listener", [100., 100., 100.], json!({}), json!({}));
    listener["components"]["EventBindings"] = json!({"bindings":
        [{"event":"acoustics.failed","handler":"record"},
         {"event":"acoustics.world_rebuilt","handler":"record"},
         {"event":"acoustics.occlusion_changed","handler":"record"},
         {"event":"acoustics.backend_changed","handler":"record"}]});
    listener
}

#[test]
fn failures_are_reported_once_as_acoustics_failed_events() {
    let dir = write_project(
        vec![
            listening_entity(),
            env_entity(json!({})),
            entity(
                "wall",
                [0., 0., -5.],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({}),
            ),
        ],
        vec![],
    );
    let mut rt = runtime(&dir);
    rt.app_mut().world_mut().insert_resource(Seen::default());
    register_event_handler(rt.app_mut(), "record", record).unwrap();
    for _ in 0..6 {
        tick(&mut rt, 0.016);
    }
    let seen = rt.app().world().resource::<Seen>().0.clone();
    let failed: Vec<_> = seen
        .iter()
        .filter(|(n, _)| n == acoustics::EVENT_FAILED)
        .collect();
    let rebuilt = seen
        .iter()
        .filter(|(n, _)| n == acoustics::EVENT_WORLD_REBUILT)
        .count();
    // Whatever fails (skeleton service, missing presets) is reported exactly once.
    assert!(failed.len() <= 1, "{failed:?}");
    if let Some((_, payload)) = failed.first() {
        assert!(payload["code"].as_str().unwrap().starts_with("acoustics."));
        assert!(!payload["message"].as_str().unwrap().is_empty());
        assert_eq!(rebuilt, 0);
    } else {
        assert_eq!(
            rebuilt, 1,
            "a healthy service rebuilds the unchanged world once"
        );
    }
}

// ---- authored audio (root decision: AudioEmitter / AudioListener) ----------------------------

#[test]
fn audio_emitters_play_follow_their_entity_and_respect_their_options() {
    let mut a = with_audio(write_project(
        vec![
            entity(
                "speaker",
                [2., 0., 0.],
                json!({}),
                json!({"engine.AudioEmitter":data(json!({"clip":"sfx","looped":true,"bus":"dialogue","gain":0.5,"pitch":2,"spatial":{"min_distance":2,"max_distance":50,"attenuation":"linear"}}))}),
            ),
            entity(
                "music",
                [0.; 3],
                json!({}),
                json!({"engine.AudioEmitter":data(json!({"clip":"sfx","bus":"music","spatial":{}}))}),
            ),
            entity(
                "ui",
                [0.; 3],
                json!({}),
                json!({"engine.AudioEmitter":data(json!({"clip":"sfx","autoplay":false}))}),
            ),
        ],
        vec![],
    ));
    tick(&mut a.rt, 0.016);
    let speaker = a.rt.entity("speaker").unwrap();
    let world = a.rt.app().world();
    let handle = world.get::<AuthoredEmitter>(speaker).unwrap().source;
    let audio_of = |entity| world.get::<EntityAudio>(entity).unwrap();
    let options = audio_of(speaker).sources.options(handle).unwrap().clone();
    assert!(options.looped && options.spatial.enabled);
    assert_eq!(
        (options.bus, options.gain, options.pitch),
        (Bus::Dialogue, 0.5, 2.0)
    );
    assert_eq!(options.spatial.position, [2., 0., 0.]);
    assert_eq!(options.spatial.min_distance, 2.0);
    assert_eq!(
        options.spatial.attenuation,
        engine_audio::Attenuation::Linear
    );
    let voice = audio_of(speaker).sources.voice(handle).unwrap();
    assert_eq!(a.control.voice_state(voice), VoiceState::Playing);
    // Without autoplay the source exists but has no voice yet.
    let ui = a.rt.entity("ui").unwrap();
    let ui_handle =
        a.rt.app()
            .world()
            .get::<AuthoredEmitter>(ui)
            .unwrap()
            .source;
    assert!(
        a.rt.app()
            .world()
            .get::<EntityAudio>(ui)
            .unwrap()
            .sources
            .voice(ui_handle)
            .is_none()
    );
    // A spatial emitter follows its entity.
    a.rt.app_mut()
        .world_mut()
        .get_mut::<Transform>(speaker)
        .unwrap()
        .position = [7., 1., -3.];
    tick(&mut a.rt, 0.016);
    let position =
        a.rt.app()
            .world()
            .get::<EntityAudio>(speaker)
            .unwrap()
            .sources
            .options(handle)
            .unwrap()
            .spatial
            .position;
    assert_eq!(position, [7., 1., -3.]);
}

#[test]
fn emitter_events_start_and_stop_playback() {
    let mut a = with_audio(write_project(
        vec![entity(
            "bell",
            [0.; 3],
            json!({}),
            json!({"engine.AudioEmitter":data(json!({"clip":"sfx","autoplay":false,"play_on":"ring","stop_on":"hush"}))}),
        )],
        vec![],
    ));
    tick(&mut a.rt, 0.016);
    let bell = a.rt.entity("bell").unwrap();
    let handle =
        a.rt.app()
            .world()
            .get::<AuthoredEmitter>(bell)
            .unwrap()
            .source;
    let state = |a: &Audio| {
        a.rt.app()
            .world()
            .get::<EntityAudio>(bell)
            .unwrap()
            .sources
            .state(handle)
    };
    assert_eq!(state(&a), VoiceState::Stopped);
    assert_eq!(
        audio::notify(a.rt.app_mut().world_mut(), bell, "ring").unwrap(),
        1
    );
    assert_eq!(state(&a), VoiceState::Playing);
    assert_eq!(
        audio::notify(a.rt.app_mut().world_mut(), bell, "hush").unwrap(),
        1
    );
    assert_eq!(state(&a), VoiceState::Stopped);
}

#[test]
fn an_emitter_lazily_loads_its_clip_at_startup() {
    let dir = write_project(
        vec![entity(
            "e",
            [0.; 3],
            json!({}),
            json!({"engine.AudioEmitter":data(json!({"clip":"sfx"}))}),
        )],
        vec![],
    );
    let engine = AudioEngine::offline(Default::default()).unwrap();
    let mut rt = runtime(&dir);
    rt.set_audio(engine.control()).unwrap();
    // An empty cache is normal; startup loads only the referenced resident asset.
    rt.app_mut()
        .world_mut()
        .resource_mut::<audio::RuntimeAudio>()
        .clips
        .clear();
    rt.tick(0.016, ActionFrame::default()).unwrap();
    assert!(
        rt.app()
            .world()
            .resource::<audio::RuntimeAudio>()
            .clips
            .contains_key("sfx")
    );
}

#[test]
fn emitters_are_inert_without_configured_audio() {
    let dir = write_project(
        vec![entity(
            "e",
            [0.; 3],
            json!({}),
            json!({"engine.AudioEmitter":data(json!({"clip":"sfx"}))}),
        )],
        vec![],
    );
    let mut rt = runtime(&dir);
    tick(&mut rt, 0.016);
    let e = rt.entity("e").unwrap();
    assert!(rt.app().world().get::<AuthoredEmitter>(e).is_none());
}

#[test]
fn audio_listener_follows_position_and_rotation() {
    let mut turned = entity(
        "ears",
        [1., 2., 3.],
        json!({}),
        json!({"engine.AudioListener":data(json!({}))}),
    );
    // Yaw 90 degrees left: forward -Z becomes -X.
    turned["components"]["Transform"]["rotation"] =
        json!(glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2).to_array());
    let mut a = with_audio(write_project(
        vec![
            turned,
            entity(
                "zlast",
                [9., 9., 9.],
                json!({}),
                json!({"engine.AudioListener":data(json!({}))}),
            ),
            entity(
                "off",
                [8., 8., 8.],
                json!({}),
                json!({"engine.AudioListener":data(json!({"active":false}))}),
            ),
        ],
        vec![],
    ));
    tick(&mut a.rt, 0.016);
    let listener = a.control.listener().unwrap();
    assert_eq!(
        listener.position,
        [1., 2., 3.],
        "first active listener by authored ID wins"
    );
    assert!((listener.forward[0] + 1.0).abs() < 1e-5 && listener.forward[2].abs() < 1e-5);
    assert!((listener.up[1] - 1.0).abs() < 1e-5);
    let ears = a.rt.entity("ears").unwrap();
    {
        let mut t =
            a.rt.app_mut()
                .world_mut()
                .get_mut::<Transform>(ears)
                .unwrap();
        t.position = [4., 0., 0.];
        t.rotation = [0., 0., 0., 1.];
    }
    tick(&mut a.rt, 0.016);
    let listener = a.control.listener().unwrap();
    assert_eq!(listener.position, [4., 0., 0.]);
    assert!((listener.forward[2] + 1.0).abs() < 1e-5);
}

#[test]
fn a_native_listener_overrides_the_authored_one() {
    let mut a = with_audio(write_project(
        vec![
            entity(
                "ears",
                [1., 2., 3.],
                json!({}),
                json!({"engine.AudioListener":data(json!({}))}),
            ),
            entity("other", [5., 5., 5.], json!({}), json!({})),
        ],
        vec![],
    ));
    tick(&mut a.rt, 0.016);
    let other = a.rt.entity("other").unwrap();
    audio::listener(
        a.rt.app_mut().world_mut(),
        other,
        engine_audio::Listener::default(),
        Some(other),
    )
    .unwrap();
    tick(&mut a.rt, 0.016);
    assert_eq!(a.control.listener().unwrap().position, [5., 5., 5.]);
}

#[test]
fn acoustic_source_completes_voice_options_only_while_an_environment_is_enabled() {
    let build = |env: Option<Value>| {
        let mut entities = vec![entity(
            "speaker",
            [3., 0., 0.],
            json!({}),
            json!({
                "engine.AudioEmitter":data(json!({"clip":"sfx"})),
                "engine.AcousticSource":data(json!({"doppler":2.5,"reverb_send":0.25,"air_absorption":false,"propagation_delay":false})),
            }),
        )];
        if let Some(env) = env {
            entities.push(env_entity(env));
        }
        let mut a = with_audio(write_project(entities, vec![]));
        tick(&mut a.rt, 0.016);
        let speaker = a.rt.entity("speaker").unwrap();
        let handle =
            a.rt.app()
                .world()
                .get::<AuthoredEmitter>(speaker)
                .unwrap()
                .source;
        let options =
            a.rt.app()
                .world()
                .get::<EntityAudio>(speaker)
                .unwrap()
                .sources
                .options(handle)
                .unwrap()
                .clone();
        options.acoustics
    };
    let on = build(Some(json!({"proxy2d":{"units_per_meter":4}})));
    assert!(on.enabled && !on.air_absorption && !on.propagation_delay);
    assert_eq!(
        (on.doppler, on.reverb_send, on.metres_per_unit),
        (2.5, 0.25, 0.25)
    );
    let plain = build(Some(json!({})));
    assert_eq!(plain.metres_per_unit, 1.0);
    let none = build(None);
    assert_eq!(
        none,
        engine_audio::acoustic::VoiceAcoustics::default(),
        "legacy options without an environment"
    );
    let disabled = build(Some(json!({"enabled":false})));
    assert!(!disabled.enabled);
}

// ---- native hooks ----------------------------------------------------------------------------

struct Quiet;
impl engine_audio::acoustic::DspEffect for Quiet {
    fn process(&mut self, frames: &mut [[f32; 2]], _: &engine_audio::acoustic::EffectContext) {
        for frame in frames {
            *frame = [0.0; 2];
        }
    }
}

#[test]
fn bus_effects_are_installed_through_the_runtime_audio_control() {
    let mut a = with_audio(write_project(
        vec![entity("a", [0.; 3], json!({}), json!({}))],
        vec![],
    ));
    let world = a.rt.app_mut().world_mut();
    assert!(
        acoustics::set_bus_effect(world, Bus::Effects, Some(Box::new(Quiet)))
            .unwrap()
            .is_none()
    );
    let old = acoustics::set_bus_effect(world, Bus::Effects, None).unwrap();
    assert!(
        old.is_some(),
        "the previous effect is returned to be dropped off the audio thread"
    );
    let none = write_project(vec![], vec![]);
    let rt = runtime(&none);
    assert_eq!(
        code(
            acoustics::set_bus_effect(rt.app().world(), Bus::Music, None)
                .err()
                .unwrap()
        ),
        "audio.unavailable"
    );
}

#[test]
fn the_parameter_hook_can_be_set_and_cleared_at_any_time() {
    let dir = write_project(vec![env_entity(json!({}))], vec![]);
    let mut rt = runtime(&dir);
    rt.set_param_hook(Some(Box::new(|_: &PropagationResult, _: &mut _| {})));
    tick(&mut rt, 0.016);
    rt.set_param_hook(None);
    tick(&mut rt, 0.016);
}

// ---- end to end (need streams A and B) -------------------------------------------------------

type Captured = Arc<Mutex<Vec<(PropagationResult, engine_audio::acoustic::ReverbParams)>>>;
/// Records each result with the reverb of the frame the hook receives, which is
/// after the built-in `reverb_source` handling.
fn capture_hook() -> (Captured, engine_acoustics::ParamHook) {
    let results: Captured = Arc::new(Mutex::new(vec![]));
    let sink = results.clone();
    (
        results,
        Box::new(
            move |result: &PropagationResult, frame: &mut engine_audio::acoustic::AcousticFrame| {
                sink.lock().unwrap().push((result.clone(), frame.reverb));
            },
        ),
    )
}
fn room_entities(units_per_meter: f32, ceiling: bool, open_sky: bool) -> Vec<Value> {
    // A side-view room of Collider2D boxes: inner 10 x 6 units, depth 8 m.
    let collider = |id: &str, x: f32, y: f32, w: f32, h: f32| {
        entity(
            id,
            [x, y, 0.],
            json!({"Collider2D":{"size":[w,h]}}),
            json!({"engine.AcousticMaterial":data(json!({"preset":"concrete","scattering":1.0}))}),
        )
    };
    let mut entities = vec![
        env_entity(json!({
            "backend":"cpu","seed":3,
            "quality":{"reverb_rays":16384,"max_bounces":64,"histogram_length_s":4.0},
            // `depth_m` is in metres and does not follow `units_per_meter`, so doubling
            // the room (0.5 units per metre) doubles the depth as well.
            "proxy2d":{"plane":"side_view","units_per_meter":units_per_meter,"depth_m":8.0 / units_per_meter,"open_sky":open_sky,
                       "enclosure":"none","material":"concrete","floor_material":"concrete",
                       "ceiling_material":"concrete","side_material":"concrete"},
        })),
        collider("left", -5.5, 0., 1., 8.),
        collider("right", 5.5, 0., 1., 8.),
        collider("floor", 0., -3.5, 12., 1.),
        entity(
            "ears",
            [0., 0., 0.],
            json!({}),
            json!({"engine.AudioListener":data(json!({}))}),
        ),
        entity(
            "speaker",
            [2., 0., 0.],
            json!({}),
            json!({
                "engine.AudioEmitter":data(json!({"clip":"sfx","looped":true})),
                "engine.AcousticSource":data(json!({})),
            }),
        ),
    ];
    if ceiling {
        entities.push(collider("ceiling", 0., 3.5, 12., 1.));
    }
    entities
}
fn run_room(units_per_meter: f32, ceiling: bool, open_sky: bool) -> PropagationResult {
    let mut a = with_audio(write_project(
        room_entities(units_per_meter, ceiling, open_sky),
        vec![],
    ));
    let (results, hook) = capture_hook();
    a.rt.set_param_hook(Some(hook));
    for _ in 0..40 {
        tick(&mut a.rt, 1.0 / 30.0);
    }
    let status = a.rt.acoustics_status().unwrap();
    assert_eq!(status.state, AcousticState::Active, "{status:?}");
    assert_eq!(status.active, Some(engine_acoustics::BackendKind::Cpu));
    assert!(status.world.triangles > 0 && status.updates > 0);
    let last = results
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("a propagation result");
    last.0
}

#[test]
fn proxy_room_scales_its_reverb_with_units_per_meter() {
    let near = run_room(1.0, true, false);
    let far = run_room(0.5, true, false);
    let ratio = far.reverb.rt60_s[2] / near.reverb.rt60_s[2];
    assert!((ratio - 2.0).abs() < 0.3, "RT60 ratio {ratio}");
    let mfp = far.reverb.mean_free_path_m / near.reverb.mean_free_path_m;
    assert!((mfp - 2.0).abs() < 0.2, "mean free path ratio {mfp}");
}

#[test]
fn an_open_sky_proxy_lets_rays_escape_and_lowers_the_late_level() {
    let closed = run_room(1.0, true, false);
    let open = run_room(1.0, false, true);
    let escaped = open.listener_field.as_ref().unwrap().escaped;
    assert!(escaped > 0.3, "escaped {escaped}");
    assert!(closed.listener_field.as_ref().unwrap().escaped < escaped);
    // Late level falls by at least 6 dB (energy ratio 0.25) in the mid band.
    assert!(open.reverb.level[2] <= closed.reverb.level[2] * 0.25);
}

#[test]
fn a_moving_occluder_emits_one_event_per_crossing() {
    let mut listener = listening_entity();
    listener["components"]["Transform"]["position"] = json!([100., 100., 100.]);
    let dir = write_project(
        vec![
            listener,
            env_entity(json!({"backend":"cpu","update_hz":30,"quality":{"occlusion_rays":32}})),
            entity(
                "ears",
                [0., 0., 0.],
                json!({}),
                json!({"engine.AudioListener":data(json!({}))}),
            ),
            entity(
                "speaker",
                [0., 0., -10.],
                json!({}),
                json!({
                    "engine.AudioEmitter":data(json!({"clip":"sfx","looped":true})),
                    "engine.AcousticSource":data(json!({"radius_m":0.25})),
                }),
            ),
            // A 40 m square wall (quad scaled), parked far to the side.
            {
                let mut wall = entity(
                    "wall",
                    [500., 0., -5.],
                    json!({"MeshRenderer":{"mesh":"quad"}}),
                    json!({
                        "engine.AcousticGeometry":data(json!({"mode":"dynamic"})),
                        "engine.AcousticMaterial":data(json!({"preset":"concrete"})),
                    }),
                );
                wall["components"]["Transform"]["scale"] = json!([40., 40., 1.]);
                wall
            },
        ],
        vec![],
    );
    let engine = AudioEngine::offline(Default::default()).unwrap();
    let mut rt = runtime(&dir);
    rt.set_audio(engine.control()).unwrap();
    rt.app_mut().world_mut().insert_resource(Seen::default());
    register_event_handler(rt.app_mut(), "record", record).unwrap();
    let wall = rt.entity("wall").unwrap();
    let move_wall = |rt: &mut Runtime, x: f32| {
        rt.app_mut()
            .world_mut()
            .get_mut::<Transform>(wall)
            .unwrap()
            .position = [x, 0., -5.];
        for _ in 0..20 {
            tick(rt, 1.0 / 30.0);
        }
    };
    move_wall(&mut rt, 500.0);
    move_wall(&mut rt, 0.0);
    move_wall(&mut rt, 500.0);
    let seen = rt.app().world().resource::<Seen>().0.clone();
    let occlusion: Vec<_> = seen
        .iter()
        .filter(|(n, _)| n == acoustics::EVENT_OCCLUSION_CHANGED)
        .map(|(_, p)| p["occluded"].as_bool().unwrap())
        .collect();
    assert_eq!(occlusion, [true, false]);
}

#[test]
fn room_volumes_supply_the_late_reverb_when_the_source_is_volumes() {
    let dir = write_project(
        vec![
            env_entity(json!({"backend":"cpu","reverb_source":"volumes"})),
            entity(
                "hall",
                [0.; 3],
                json!({}),
                json!({
                    "engine.AcousticVolume":data(json!({"half_extents":[5,3,4],"kind":"room"})),
                    "engine.AcousticMaterial":data(json!({"preset":"concrete"})),
                }),
            ),
            entity(
                "ears",
                [0.; 3],
                json!({}),
                json!({"engine.AudioListener":data(json!({}))}),
            ),
        ],
        vec![],
    );
    // The room is 10 x 6 x 8 m (half extents 5, 3, 4).
    let mut a = with_audio(dir);
    let (results, hook) = capture_hook();
    a.rt.set_param_hook(Some(hook));
    for _ in 0..20 {
        tick(&mut a.rt, 1.0 / 30.0);
    }
    assert_eq!(
        a.rt.acoustics_status().unwrap().state,
        AcousticState::Active
    );
    let results = results.lock().unwrap();
    let (_, reverb) = results.last().expect("a propagation result");
    // The listener stands in the 10 x 6 x 8 m concrete room: Eyring RT60 per band.
    assert!(reverb.enabled && reverb.rt60_s.iter().all(|t| *t > 0.0));
    assert!(
        (reverb.mean_free_path_m - 5.106).abs() < 0.01,
        "{}",
        reverb.mean_free_path_m
    );
}

fn write_tone_graph(dir: &tempfile::TempDir) {
    let graph = json!({"version":1,"kind":"audio","nodes":[{"id":"value","node_type":"parameter","params":{"variable":"amplitude"}},{"id":"out","node_type":"output"}],"variables":[{"name":"amplitude","type":"float","default":0.25}],"edges":[{"from":{"node":"value","port":"signal"},"to":{"node":"out","port":"left"}},{"from":{"node":"value","port":"signal"},"to":{"node":"out","port":"right"}}]});
    fs::write(
        dir.path().join("tone.graph"),
        serde_json::to_vec(&graph).unwrap(),
    )
    .unwrap();
}
fn wait_audio(mut ready: impl FnMut() -> bool) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !ready() {
        assert!(std::time::Instant::now() < until, "producer timeout");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
#[test]
fn streamed_emitters_use_shared_events_seek_spatial_attachments_and_drop() {
    let dir = write_project(
        vec![entity(
            "speaker",
            [1., 0., 0.],
            json!({}),
            json!({"engine.AudioEmitter":data(json!({"clip":"encoded","streamed":true,"autoplay":false,"queue_frames":4,"play_on":"ring","stop_on":"hush"}))}),
        )],
        vec![json!({"id":"encoded","kind":"audio","path":"tone.mp3"})],
    );
    fs::write(
        dir.path().join("tone.mp3"),
        include_bytes!("../../engine-audio/tests/fixtures/tone.mp3"),
    )
    .unwrap();
    let mut engine = AudioEngine::offline(engine_audio::AudioConfig {
        sample_rate: 44100,
        max_voices: 2,
    })
    .unwrap();
    let control = engine.control();
    let mut rt = runtime(&dir);
    rt.set_audio(control.clone()).unwrap();
    tick(&mut rt, 0.016);
    assert!(
        rt.app()
            .world()
            .resource::<audio::RuntimeAudio>()
            .clips
            .is_empty()
    );
    let owner = rt.entity("speaker").unwrap();
    let source = rt
        .app()
        .world()
        .get::<AuthoredEmitter>(owner)
        .unwrap()
        .source;
    assert_eq!(control.voice_count(), 0);
    audio::notify(rt.app_mut().world_mut(), owner, "ring").unwrap();
    let voice = rt
        .app()
        .world()
        .get::<EntityAudio>(owner)
        .unwrap()
        .sources
        .voice(source)
        .unwrap();
    wait_audio(|| {
        control
            .stream_snapshot(voice)
            .unwrap()
            .unwrap()
            .queued_frames
            == 4
    });
    audio::control(
        rt.app_mut().world_mut(),
        owner,
        source,
        engine_audio::SourceAction::Pause,
    )
    .unwrap();
    engine.mix(&mut [0.; 8]).unwrap();
    assert_eq!(
        control
            .stream_snapshot(voice)
            .unwrap()
            .unwrap()
            .source_frame,
        0.
    );
    let generation = audio::seek_source(rt.app_mut().world_mut(), owner, source, 1000).unwrap();
    wait_audio(|| {
        control
            .stream_snapshot(voice)
            .unwrap()
            .unwrap()
            .queued_frames
            == 4
    });
    audio::control(
        rt.app_mut().world_mut(),
        owner,
        source,
        engine_audio::SourceAction::Resume,
    )
    .unwrap();
    let mut out = [0.; 2];
    engine.mix(&mut out).unwrap();
    assert_eq!(out[0], 0.);
    assert!(out[1].abs() > 0.001);
    assert_eq!(
        control.stream_snapshot(voice).unwrap().unwrap().generation,
        generation
    );
    rt.app_mut()
        .world_mut()
        .get_mut::<Transform>(owner)
        .unwrap()
        .position = [-1., 0., 0.];
    tick(&mut rt, 0.016);
    assert_eq!(
        rt.app()
            .world()
            .get::<EntityAudio>(owner)
            .unwrap()
            .sources
            .options(source)
            .unwrap()
            .spatial
            .position,
        [-1., 0., 0.]
    );
    audio::notify(rt.app_mut().world_mut(), owner, "hush").unwrap();
    assert_eq!(control.voice_count(), 0);
    audio::notify(rt.app_mut().world_mut(), owner, "ring").unwrap();
    assert_ne!(
        rt.app()
            .world()
            .get::<EntityAudio>(owner)
            .unwrap()
            .sources
            .voice(source),
        Some(voice)
    );
    rt.app_mut().world_mut().despawn(owner);
    assert_eq!(control.voice_count(), 0);
}
#[test]
fn procedural_emitters_apply_initial_controls_before_pcm_and_share_reset_and_owner_cleanup() {
    let dir = write_project(
        vec![entity(
            "speaker",
            [1., 0., 0.],
            json!({}),
            json!({"engine.AudioEmitter":data(json!({"graph":"tone","parameters":{"amplitude":0.5},"queue_frames":4,"duration_frames":64}))}),
        )],
        vec![json!({"id":"tone","kind":"graph","path":"tone.graph"})],
    );
    write_tone_graph(&dir);
    let mut engine = AudioEngine::offline(engine_audio::AudioConfig {
        sample_rate: 8000,
        max_voices: 1,
    })
    .unwrap();
    let control = engine.control();
    let mut rt = runtime(&dir);
    rt.set_audio(control.clone()).unwrap();
    tick(&mut rt, 0.016);
    let owner = rt.entity("speaker").unwrap();
    let source = rt
        .app()
        .world()
        .get::<AuthoredEmitter>(owner)
        .unwrap()
        .source;
    let voice = rt
        .app()
        .world()
        .get::<EntityAudio>(owner)
        .unwrap()
        .sources
        .voice(source)
        .unwrap();
    wait_audio(|| {
        control
            .stream_snapshot(voice)
            .unwrap()
            .unwrap()
            .queued_frames
            == 4
    });
    let mut out = [0.; 2];
    engine.mix(&mut out).unwrap();
    assert_eq!(out, [0., 0.5]);
    audio::set_parameters(
        rt.app_mut().world_mut(),
        owner,
        source,
        &[engine_audio::graph::ParameterUpdate {
            name: "amplitude",
            value: 0.75,
            ramp_frames: 0,
        }],
    )
    .unwrap();
    let generation = audio::seek_source(rt.app_mut().world_mut(), owner, source, 0).unwrap();
    wait_audio(|| {
        control
            .stream_snapshot(voice)
            .unwrap()
            .unwrap()
            .queued_frames
            == 4
    });
    engine.mix(&mut out).unwrap();
    assert_eq!(out, [0., 0.75]);
    assert_eq!(
        control.stream_snapshot(voice).unwrap().unwrap().generation,
        generation
    );
    rt.stop().unwrap();
    assert_eq!(control.voice_count(), 0);
}
#[test]
fn new_source_modes_are_enumerated_by_the_same_acoustic_service() {
    let dir = write_project(
        vec![
            env_entity(json!({"backend":"cpu","quality":{"occlusion_rays":4}})),
            entity(
                "mesh",
                [100., 0., 0.],
                json!({"MeshRenderer":{"mesh":"quad"}}),
                json!({}),
            ),
            entity(
                "file",
                [1., 0., 0.],
                json!({}),
                json!({"engine.AudioEmitter":data(json!({"clip":"encoded","streamed":true,"queue_frames":4})),"engine.AcousticSource":data(json!({}))}),
            ),
            entity(
                "graph",
                [-1., 0., 0.],
                json!({}),
                json!({"engine.AudioEmitter":data(json!({"graph":"tone","queue_frames":4})),"engine.AcousticSource":data(json!({}))}),
            ),
        ],
        vec![
            json!({"id":"encoded","kind":"audio","path":"tone.mp3"}),
            json!({"id":"tone","kind":"graph","path":"tone.graph"}),
        ],
    );
    write_tone_graph(&dir);
    fs::write(
        dir.path().join("tone.mp3"),
        include_bytes!("../../engine-audio/tests/fixtures/tone.mp3"),
    )
    .unwrap();
    let mut a = with_audio(dir);
    let (results, hook) = capture_hook();
    a.rt.set_param_hook(Some(hook));
    for _ in 0..8 {
        tick(&mut a.rt, 1. / 30.);
    }
    let expected: Vec<_> = ["file", "graph"]
        .iter()
        .map(|id| {
            let owner = a.rt.entity(id).unwrap();
            let h =
                a.rt.app()
                    .world()
                    .get::<AuthoredEmitter>(owner)
                    .unwrap()
                    .source;
            a.rt.app()
                .world()
                .get::<EntityAudio>(owner)
                .unwrap()
                .sources
                .voice(h)
                .unwrap()
                .get()
        })
        .collect();
    assert!(results.lock().unwrap().iter().any(|(r, _)| {
        expected
            .iter()
            .all(|id| r.sources.iter().any(|s| s.id == *id))
    }));
}
#[test]
fn failed_authored_graph_preparation_cleans_previously_started_streams() {
    let dir = write_project(
        vec![
            entity(
                "a_file",
                [0.; 3],
                json!({}),
                json!({"engine.AudioEmitter":data(json!({"clip":"encoded","streamed":true}))}),
            ),
            entity(
                "z_bad",
                [0.; 3],
                json!({}),
                json!({"engine.AudioEmitter":data(json!({"graph":"tone","parameters":{"not_declared":1.}}))}),
            ),
        ],
        vec![
            json!({"id":"encoded","kind":"audio","path":"tone.mp3"}),
            json!({"id":"tone","kind":"graph","path":"tone.graph"}),
        ],
    );
    write_tone_graph(&dir);
    fs::write(
        dir.path().join("tone.mp3"),
        include_bytes!("../../engine-audio/tests/fixtures/tone.mp3"),
    )
    .unwrap();
    let engine = AudioEngine::offline(Default::default()).unwrap();
    let control = engine.control();
    let mut rt = runtime(&dir);
    rt.set_audio(control.clone()).unwrap();
    assert_eq!(
        code(rt.tick(0.016, ActionFrame::default()).unwrap_err()),
        "audio.graph.parameter"
    );
    assert_eq!(control.voice_count(), 0);
}
#[test]
fn emitter_mode_validation_rejects_ambiguous_looped_and_unbounded_settings() {
    let catalog = engine_project::ComponentCatalog::new(vec![]).unwrap();
    for data in [
        json!({"clip":"a","graph":"b"}),
        json!({"graph":"b","streamed":true}),
        json!({"clip":"a","streamed":true,"looped":true}),
        json!({"graph":"b","looped":true}),
        json!({"clip":"a","parameters":{"x":1}}),
        json!({"graph":"b","queue_frames":1}),
        json!({"graph":"b","duration_frames":192000_u64*86400+1}),
    ] {
        assert!(
            catalog
                .validate(acoustics::EMITTER, &CustomValue { version: 1, data })
                .is_err()
        );
    }
}

#[test]
fn asynchronous_procedural_fault_is_reported_and_runtime_tears_down_its_voice() {
    let dir = write_project(
        vec![entity(
            "speaker",
            [1., 0., 0.],
            json!({}),
            json!({"engine.AudioEmitter":data(json!({"graph":"bad"}))}),
        )],
        vec![json!({"id":"bad","kind":"graph","path":"bad.graph"})],
    );
    fs::write(dir.path().join("bad.graph"),br#"{"version":1,"kind":"audio","nodes":[{"id":"osc","node_type":"oscillator","params":{"frequency":4000}},{"id":"out","node_type":"output"}],"edges":[{"from":{"node":"osc","port":"signal"},"to":{"node":"out","port":"left"}},{"from":{"node":"osc","port":"signal"},"to":{"node":"out","port":"right"}}]}"#).unwrap();
    let engine = AudioEngine::offline(engine_audio::AudioConfig {
        sample_rate: 8000,
        max_voices: 1,
    })
    .unwrap();
    let control = engine.control();
    let mut rt = runtime(&dir);
    rt.set_audio(control.clone()).unwrap();
    let mut error = None;
    for _ in 0..100 {
        match rt.tick(0.016, ActionFrame::default()) {
            Err(e) => {
                error = Some(e);
                break;
            }
            Ok(()) => std::thread::sleep(std::time::Duration::from_millis(1)),
        }
    }
    assert_eq!(
        code(error.expect("worker fault was silently ignored")),
        "audio.graph.frequency"
    );
    assert_eq!(control.voice_count(), 0);
}
