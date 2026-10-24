use engine_core::{AppState, EngineApp, Stage};
use engine_ecs::bevy_ecs::prelude::*;
#[derive(Resource, Default)]
struct Value(u32);
#[test]
fn fallible_host_boundary_runs_after_update_before_extract() {
    let mut app = EngineApp::new();
    app.world_mut().init_resource::<Value>();
    app.schedule_mut(Stage::Update)
        .add_systems(|mut value: ResMut<Value>| value.0 = 3);
    app.schedule_mut(Stage::Extract)
        .add_systems(|value: Res<Value>| assert_eq!(value.0, 7));
    app.tick_with_post_update(0.02, |world| {
        assert_eq!(world.resource::<Value>().0, 3);
        world.resource_mut::<Value>().0 = 7;
        Ok(())
    })
    .unwrap();
    assert_eq!(app.profile().frames[0].stages[1].stage, Stage::PostUpdate);
}
#[test]
fn host_failure_returns_without_extraction_or_render() {
    let mut app = EngineApp::new();
    app.schedule_mut(Stage::Extract)
        .add_systems(|| panic!("extraction must not run"));
    assert!(
        app.tick_with_post_update(0.02, |_| anyhow::bail!("service failure"))
            .is_err()
    );
    assert_eq!(app.state(), AppState::Failed);
    let profile = app.profile();
    assert!(!profile.frames[0].succeeded);
    assert_eq!(profile.frames[0].stages.len(), 2);
    app.shutdown().unwrap();
}
