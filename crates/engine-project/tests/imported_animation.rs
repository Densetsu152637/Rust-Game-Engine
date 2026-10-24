use engine_assets::{ChannelValues, ImportLimits, ImportedChannel, ImportedClip, Interpolation};
use engine_project::simulation::{imported_clip, imported_skeleton, skin_palette};
use glam::Vec3;
#[test]
fn non_joint_ancestors_survive_sampling_and_mesh_local_skin_palette() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rig.gltf"),r#"{"asset":{"version":"2.0"},"nodes":[{"translation":[1,0,0],"children":[1]},{"translation":[2,0,0],"children":[2]},{"translation":[3,0,0]},{"translation":[10,0,0]}],"skins":[{"joints":[2]}],"scenes":[{"nodes":[0,3]}],"scene":0}"#).unwrap();
    let mut scene = engine_assets::import_gltf(
        dir.path(),
        std::path::Path::new("rig.gltf"),
        &ImportLimits::default(),
    )
    .unwrap();
    scene.clips.push(ImportedClip {
        name: Some("move intermediary".into()),
        duration: 1.,
        channels: vec![ImportedChannel {
            target_node: 1,
            interpolation: Interpolation::Linear,
            times: vec![0., 1.],
            values: ChannelValues::Translation(vec![[2., 0., 0.], [4., 0., 0.]]),
        }],
    });
    let skeleton = imported_skeleton(&scene).unwrap();
    let clip = imported_clip(&scene, 0).unwrap().into_clip().unwrap();
    let pose = clip.sample(0.5, false, skeleton.rest_pose()).unwrap();
    let globals = skeleton.global_matrices(&pose).unwrap();
    assert!((globals[2].transform_point3(Vec3::ZERO).x - 7.).abs() < 0.0001);
    let palette = skin_palette(&scene.skins[0], &globals, 3).unwrap();
    assert!((palette[0].transform_point3(Vec3::ZERO).x + 3.).abs() < 0.0001);
    scene.clips[0].channels[0].interpolation = Interpolation::CubicSpline;
    assert!(imported_clip(&scene, 0).is_err());
}
