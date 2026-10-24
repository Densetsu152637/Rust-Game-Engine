use engine_project::{Project, Runtime, rendering3d::*};
use serde_json::json;
use std::fs;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("mesh.cgmesh"),
        "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("material.wgsl"),
        "custom shader source snapshot",
    )
    .unwrap();
    fs::write(dir.path().join("scene.json"),serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":[
        {"id":"camera","name":"Camera","components":{"Transform":{"position":[0,0,4]},"Camera3D":{}}},
        {"id":"parent","name":"Parent","components":{"Transform":{"position":[1,0,0],"rotation":[0,0,0.70710677,0.70710677],"scale":[2,2,2]}}},
        {"id":"mesh","name":"Mesh","parent":"parent","components":{"Transform":{"position":[1,0,0]},"MeshRenderer":{"mesh":"mesh-asset"},"MeshMaterial":{"shader":"shader-asset","tint":[0.2,0.4,0.8,1.0]}}}
    ]})).unwrap()).unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"rendering-test","startup_scene":"scene.json","assets":[
                {"id":"mesh-asset","kind":"mesh","path":"mesh.cgmesh"},
                {"id":"shader-asset","kind":"shader","path":"material.wgsl"}
            ]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}
#[test]
fn authored_3d_extracts_hierarchy_material_and_camera_and_snapshots_them() {
    let dir = fixture();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert!(frame.sprites.draws.is_empty());
    assert_eq!(frame.meshes.len(), 1);
    assert_eq!(
        frame.camera3d.as_ref().unwrap().world[3],
        [0.0, 0.0, 4.0, 1.0]
    );
    let mesh = &frame.meshes[0];
    assert!((mesh.model[3][0] - 1.0).abs() < 1e-5 && (mesh.model[3][1] - 2.0).abs() < 1e-5);
    assert_eq!(
        mesh.shader.as_deref(),
        Some("custom shader source snapshot")
    );
    assert_eq!(mesh.material.tint, [0.2, 0.4, 0.8, 1.0]);
    let snapshot = runtime.try_snapshot().unwrap();
    assert!(
        snapshot
            .scene
            .entities
            .iter()
            .any(|e| e.components.camera3d.is_some())
    );
    assert!(
        snapshot
            .scene
            .entities
            .iter()
            .any(|e| e.components.material.is_some())
    );
    let camera = runtime.entity("camera").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(camera)
        .remove::<Camera3D>();
    let frame = runtime.extract_scene_frame().unwrap();
    assert!(frame.camera3d.is_none() && frame.meshes.is_empty());
    assert_eq!(frame.sprites.draws.len(), 1);
    assert_eq!(
        frame.materials2d["mesh:mesh"].shader.as_deref(),
        Some("custom shader source snapshot")
    );
}
#[test]
fn invalid_camera_and_material_asset_kinds_fail_before_runtime() {
    let dir = fixture();
    let mut project = Project::load(dir.path().join("project.json")).unwrap();
    project.scene.entities[0]
        .components
        .camera3d
        .as_mut()
        .unwrap()
        .projection = Projection3D::Perspective {
        fov_y_radians: 1.0,
        near: 0.0,
        far: 100.0,
    };
    assert!(project.validate_scene(&project.scene).is_err());
    project.scene.entities[0].components.camera3d = Some(Camera3D::default());
    project.scene.entities[2]
        .components
        .material
        .as_mut()
        .unwrap()
        .shader = Some("mesh-asset".into());
    assert!(
        project
            .validate_scene(&project.scene)
            .unwrap_err()
            .to_string()
            .contains("must be shader")
    );
}
#[test]
fn pbr_and_shadow_controls_validate_and_round_trip_through_authored_components() {
    let dir = fixture();
    let mut project = Project::load(dir.path().join("project.json")).unwrap();
    let material = project.scene.entities[2]
        .components
        .material
        .as_mut()
        .unwrap();
    material.metallic = 0.8;
    material.roughness = 0.25;
    material.cast_shadows = false;
    let camera = project.scene.entities[0]
        .components
        .camera3d
        .as_mut()
        .unwrap();
    camera.lighting.exposure = 0.5;
    let mut sun = engine_project::lights::DirectionalLight::default();
    sun.shadows.enabled = true;
    sun.shadows.resolution = 512;
    sun.lux = 4.0;
    project.scene.entities[0].components.directional_light = Some(sun);
    project.validate_scene(&project.scene).unwrap();
    let runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    assert_eq!(frame.meshes[0].material.metallic, 0.8);
    assert!(!frame.meshes[0].material.cast_shadows);
    assert_eq!(frame.camera3d.unwrap().camera.lighting.exposure, 0.5);
    let engine_project::lights::SceneLight::Directional(light) = &frame.lights[0].light else {
        panic!("{:?}", frame.lights);
    };
    assert_eq!(light.shadows.resolution, 512);
    let snapshot = runtime.try_snapshot().unwrap();
    let value = serde_json::to_value(&snapshot.scene.entities[0].components).unwrap();
    assert_eq!(value["DirectionalLight"]["shadows"]["enabled"], true);
    assert_eq!(value["Camera3D"]["lighting"]["exposure"], 0.5);
}
#[test]
fn invalid_pbr_light_and_shadow_values_are_rejected_at_boundary() {
    for value in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        let material = MeshMaterial {
            metallic: value,
            ..Default::default()
        };
        assert!(material.validate().is_err());
        let material = MeshMaterial {
            roughness: value,
            ..Default::default()
        };
        assert!(material.validate().is_err());
    }
    for resolution in [0, 63, 65, 8192] {
        let mut light = engine_project::lights::DirectionalLight::default();
        light.shadows.resolution = resolution;
        assert!(light.validate().is_err());
    }
    for exposure in [0.0, -1.0, f32::NAN, 1e6] {
        let mut camera = Camera3D::default();
        camera.lighting.exposure = exposure;
        assert!(camera.validate().is_err());
    }
    let mut lighting = SceneLighting::default();
    lighting.validate().unwrap();
    lighting.ambient = f32::NAN;
    assert!(lighting.validate().is_err());
    let dir = fixture();
    let mut project = Project::load(dir.path().join("project.json")).unwrap();
    project.scene.entities[2]
        .components
        .material
        .as_mut()
        .unwrap()
        .normal_texture = Some("mesh-asset".into());
    assert!(
        project
            .validate_scene(&project.scene)
            .unwrap_err()
            .to_string()
            .contains("must be texture")
    );
}
#[test]
fn authored_upscaler_round_trips_and_rejects_unknown_values() {
    let camera: Camera3D = serde_json::from_value(
        json!({"upscaler":{"mode":"dlss_sr","quality":"ultra_performance"}}),
    )
    .unwrap();
    assert_eq!(camera.upscaler.mode, UpscalerMode::DlssSr);
    assert_eq!(camera.upscaler.quality, UpscalerQuality::UltraPerformance);
    camera.validate().unwrap();
    let value = serde_json::to_value(&camera).unwrap();
    assert_eq!(
        value["upscaler"],
        json!({"mode":"dlss_sr","quality":"ultra_performance"})
    );
    let defaulted: Camera3D = serde_json::from_value(json!({})).unwrap();
    assert_eq!(defaulted.upscaler, UpscalerSettings::default());
    assert_eq!(defaulted.upscaler.mode, UpscalerMode::None);
    assert_eq!(defaulted.upscaler.quality, UpscalerQuality::Quality);
    let dlaa: UpscalerSettings = serde_json::from_value(json!({"quality":"dlaa"})).unwrap();
    assert_eq!(dlaa.quality, UpscalerQuality::Dlaa);
    for bad in [
        json!({"upscaler":{"mode":"dlss5"}}),
        json!({"upscaler":{"mode":"DLSS_SR"}}),
        json!({"upscaler":{"quality":"ultra"}}),
        json!({"upscaler":{"mode":"none","sharpness":1}}),
        json!({"upscaler":"dlss_sr"}),
    ] {
        assert!(
            serde_json::from_value::<Camera3D>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}
#[test]
fn native_override_wins_over_the_authored_request_and_is_reported() {
    let dir = fixture();
    let mut project = Project::load(dir.path().join("project.json")).unwrap();
    project.scene.entities[0]
        .components
        .camera3d
        .as_mut()
        .unwrap()
        .upscaler
        .mode = UpscalerMode::DlssSr;
    let mut runtime = Runtime::new(project).unwrap();
    let frame = runtime.extract_scene_frame().unwrap();
    let authored = frame.camera3d.unwrap().camera.upscaler;
    assert_eq!(authored.mode, UpscalerMode::DlssSr);
    // Before any renderer presents, the request is visible but not claimed.
    let status = runtime.try_snapshot().unwrap().upscaler.unwrap();
    assert_eq!(status.state, UpscalerState::NotRendered);
    assert_eq!(status.requested, authored);
    assert_eq!(status.evaluated_frames, 0);
    let value = serde_json::to_value(&status).unwrap();
    assert_eq!(value["state"], "not_rendered");
    assert_eq!(value["requested"]["mode"], "dlss_sr");
    assert!(value.get("fallback_reason").is_none());
    let override_settings = UpscalerSettings {
        mode: UpscalerMode::DlssSr,
        quality: UpscalerQuality::Dlaa,
    };
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UpscalerControl>()
        .override_settings = Some(override_settings);
    let frame = runtime.extract_scene_frame().unwrap();
    assert_eq!(frame.camera3d.unwrap().camera.upscaler, override_settings);
    assert_eq!(
        runtime.try_snapshot().unwrap().upscaler.unwrap().requested,
        override_settings
    );
    // A host-written status is reported as-is.
    let mut written = UpscalerStatus {
        requested: override_settings,
        state: UpscalerState::Fallback,
        fallback_reason: Some(bounded_upscaler_reason(&"x".repeat(2000))),
        ..Default::default()
    };
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UpscalerControl>()
        .status = written.clone();
    let reported = runtime.try_snapshot().unwrap().upscaler.unwrap();
    assert_eq!(
        reported.fallback_reason.as_ref().unwrap().len(),
        MAX_UPSCALER_REASON_BYTES
    );
    written.fallback_reason = reported.fallback_reason.clone();
    assert_eq!(reported, written);
}
#[test]
fn scenes_without_a_3d_camera_report_no_upscaler() {
    let dir = fixture();
    let mut project = Project::load(dir.path().join("project.json")).unwrap();
    project.scene.entities[0].components.camera3d = None;
    let mut runtime = Runtime::new(project).unwrap();
    assert!(runtime.try_snapshot().unwrap().upscaler.is_none());
    // A host writes a native status for 2D-only frames; it is still not reported.
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UpscalerControl>()
        .status
        .state = UpscalerState::Native;
    assert!(runtime.upscaler_status().is_none());
}
#[test]
fn authored_frame_generation_is_validated_documented_and_overridable() {
    let camera: Camera3D =
        serde_json::from_value(json!({"frame_generation":{"mode":"dlss_fg","multiplier":4}}))
            .unwrap();
    camera.validate().unwrap();
    assert_eq!(camera.frame_generation.mode, FrameGenerationMode::DlssFg);
    assert_eq!(
        serde_json::to_value(&camera).unwrap()["frame_generation"],
        json!({"mode":"dlss_fg","multiplier":4})
    );
    let defaulted: Camera3D = serde_json::from_value(json!({})).unwrap();
    assert_eq!(
        defaulted.frame_generation,
        FrameGenerationSettings::default()
    );
    assert_eq!(defaulted.frame_generation.multiplier, 2);
    for multiplier in [0, 1, 5, 8] {
        let camera: Camera3D = serde_json::from_value(
            json!({"frame_generation":{"mode":"dlss_fg","multiplier":multiplier}}),
        )
        .unwrap();
        assert!(camera.validate().is_err(), "{multiplier}");
    }
    for bad in [
        json!({"frame_generation":{"mode":"dlss_mfg"}}),
        json!({"frame_generation":{"multiplier":2.5}}),
        json!({"frame_generation":{"mode":"off","pacing":1}}),
    ] {
        assert!(
            serde_json::from_value::<Camera3D>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    // The schema served by MCP component_schema documents the same bounds.
    let doc = engine_project::schema::component_docs()
        .into_iter()
        .find(|d| d.name == "Camera3D")
        .unwrap();
    let schema = &doc.schema["properties"]["frame_generation"]["properties"];
    assert_eq!(schema["mode"]["enum"], json!(["off", "dlss_fg"]));
    assert_eq!(
        (
            schema["multiplier"]["minimum"].clone(),
            schema["multiplier"]["maximum"].clone()
        ),
        (json!(2), json!(4))
    );

    let dir = fixture();
    let mut project = Project::load(dir.path().join("project.json")).unwrap();
    project.scene.entities[0]
        .components
        .camera3d
        .as_mut()
        .unwrap()
        .frame_generation
        .mode = FrameGenerationMode::DlssFg;
    let mut bad_scene = project.scene.clone();
    bad_scene.entities[0]
        .components
        .camera3d
        .as_mut()
        .unwrap()
        .frame_generation
        .multiplier = 9;
    assert!(project.validate_scene(&bad_scene).is_err());
    let mut runtime = Runtime::new(project).unwrap();
    let status = runtime.try_snapshot().unwrap().frame_generation.unwrap();
    assert_eq!(status.state, FrameGenerationState::NotRendered);
    assert_eq!(status.requested.mode, FrameGenerationMode::DlssFg);
    assert_eq!(status.generated_frames, 0);
    let value = serde_json::to_value(&status).unwrap();
    assert_eq!(value["state"], "not_rendered");
    assert!(value.get("multiplier").is_none());
    let world = runtime.app_mut().world_mut();
    world
        .resource_mut::<FrameGenerationControl>()
        .override_settings = Some(FrameGenerationSettings {
        mode: FrameGenerationMode::DlssFg,
        multiplier: 3,
    });
    let frame = runtime.extract_scene_frame().unwrap();
    assert_eq!(
        frame.camera3d.unwrap().camera.frame_generation.multiplier,
        3
    );
    // An invalid native override is ignored rather than reaching the renderer.
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<FrameGenerationControl>()
        .override_settings = Some(FrameGenerationSettings {
        mode: FrameGenerationMode::DlssFg,
        multiplier: 7,
    });
    let frame = runtime.extract_scene_frame().unwrap();
    assert_eq!(
        frame.camera3d.unwrap().camera.frame_generation.multiplier,
        2
    );
}
