#![cfg(not(target_arch = "wasm32"))]
use bevy_ecs::prelude::World;
use engine_project::ml;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
#[test]
fn native_ml_service_has_explicit_availability_and_real_results() {
    let mut world = World::new();
    assert!(!ml::available(&world));
    assert_eq!(
        ml::poll(&world).unwrap_err().diagnostics[0].code,
        "ml.runtime.unavailable"
    );
    ml::install(
        &mut world,
        ml::RuntimeConfig {
            workers: 1,
            max_jobs: 1,
        },
    )
    .unwrap();
    assert!(ml::available(&world));
    let model = Arc::new(
        ml::Model::load(
            &engine_ml::fixtures::double_row(),
            ml::LoadOptions::default(),
        )
        .unwrap(),
    );
    let id = ml::submit(
        &world,
        model,
        vec![ml::Tensor::row(&[3.0, 2.0, 1.0]).unwrap()],
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(done) = ml::poll(&world).unwrap() {
            assert_eq!(done.id, id);
            assert_eq!(done.result.unwrap()[0].as_f32().unwrap(), &[6.0, 4.0, 2.0]);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    ml::shutdown(&mut world);
    assert!(!ml::available(&world));
    assert!(!ml::backend_status().gpu_available);
}

#[test]
fn service_reinstall_rejects_old_job_handles() {
    let mut world = World::new();
    let config = ml::RuntimeConfig {
        workers: 1,
        max_jobs: 1,
    };
    ml::install(&mut world, config).unwrap();
    let model = Arc::new(
        ml::Model::load(
            &engine_ml::fixtures::double_row(),
            ml::LoadOptions::default(),
        )
        .unwrap(),
    );
    let stale = ml::submit(
        &world,
        model.clone(),
        vec![ml::Tensor::row(&[1.0, 2.0, 3.0]).unwrap()],
    )
    .unwrap();
    ml::shutdown(&mut world);
    ml::install(&mut world, config).unwrap();
    let current = ml::submit(
        &world,
        model,
        vec![ml::Tensor::row(&[1.0, 2.0, 3.0]).unwrap()],
    )
    .unwrap();
    assert_ne!(stale, current);
    assert_eq!(
        ml::cancel(&world, stale).unwrap_err().diagnostics[0].code,
        "ml.job.unknown"
    );
    ml::shutdown(&mut world);
}
