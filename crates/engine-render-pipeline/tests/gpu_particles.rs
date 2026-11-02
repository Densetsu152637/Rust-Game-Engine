//! GPU readback tests of `engine.particles`. Run with
//! `cargo test -p engine-render-pipeline --test gpu_particles -- --ignored --test-threads=1`.
//!
//! Simulation tests read the pools back and compare them with the seeded CPU
//! `ParticleReference`: counters and slot liveness exactly, positions and
//! velocities within `REFERENCE_TOLERANCE`. Rendering tests use an unlit grey
//! floor (y = 0) seen by an orthographic top view, 8 pixels per metre at the
//! default 64 x 64 / 8 m view; screen right is +X and screen down is +Z.

use anyhow::Result;
use engine_render_pipeline::*;
use engine_render_wgpu::WgpuOptions;
use glam::{Mat4, Vec3, Vec4};
use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

const GREY: f32 = 0.5;

fn gpu() -> Result<GpuContext> {
    GpuContext::headless_with_options(&WgpuOptions::default())
}

/// A horizontal square (`half` metres) facing +Y.
fn quad(gpu: &GpuContext, half: f32) -> Arc<GpuMesh> {
    let vertex = |x, z| EngineVertex {
        position: [x, 0.0, z],
        uv: [0.0; 2],
        color: [1.0; 4],
        normal: [0.0, 1.0, 0.0],
    };
    GpuMesh::from_engine_vertices(
        &gpu.device,
        "particle test quad",
        &[
            vertex(-half, -half),
            vertex(half, -half),
            vertex(half, half),
            vertex(-half, half),
        ],
        &[0, 3, 2, 0, 2, 1],
    )
    .unwrap()
}

fn unlit<'a>(id: &'a str, mesh: &Arc<GpuMesh>, world: Mat4, tint: [f32; 3]) -> SceneMesh<'a> {
    SceneMesh {
        id,
        mesh: mesh.clone(),
        world_from_local: world,
        shader: None,
        params: MaterialParams {
            tint: [tint[0], tint[1], tint[2], 1.0],
            unlit: true,
            ..Default::default()
        },
        base_color: None,
        metallic_roughness: None,
        normal_map: None,
        lightmap: None,
    }
}

/// An orthographic camera looking straight down from 10 m, 8 m across.
fn top_view(id: ViewId, size: u32) -> ViewData {
    let extent = 8.0;
    let projection = glam::camera::rh::proj::directx::orthographic(
        -extent * 0.5,
        extent * 0.5,
        -extent * 0.5,
        extent * 0.5,
        0.1,
        20.0,
    );
    let world_from_view =
        Mat4::from_translation(Vec3::Y * 10.0) * Mat4::from_rotation_x(-FRAC_PI_2);
    ViewData::new(id, world_from_view, projection, [size; 2], [size; 2]).unwrap()
}

fn target(gpu: &GpuContext, size: u32) -> OutputTarget {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("particle test target"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    OutputTarget {
        view: texture.create_view(&Default::default()),
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        size: [size; 2],
        viewport: Viewport::full([size; 2]),
    }
}

struct Rig {
    gpu: GpuContext,
    pipeline: ScenePipeline,
    floor: Arc<GpuMesh>,
}

impl Rig {
    fn new() -> Result<Self> {
        let gpu = gpu()?;
        let pipeline = ScenePipeline::new(gpu.clone())?;
        let floor = quad(&gpu, 20.0);
        Ok(Self {
            gpu,
            pipeline,
            floor,
        })
    }

    /// Render the floor plus `extra` meshes into view `id` at `size`.
    fn render(
        &mut self,
        id: ViewId,
        size: u32,
        extra: &[SceneMesh<'_>],
    ) -> Result<(HdrImage, ViewReport)> {
        let view = top_view(id, size);
        let mut meshes = vec![unlit("floor", &self.floor, Mat4::IDENTITY, [GREY; 3])];
        meshes.extend(extra.iter().cloned());
        let output = target(&self.gpu, size);
        let report = self.pipeline.render_view(
            &SceneFrame {
                view: &view,
                meshes: &meshes,
                lights: &LightList::default(),
                world: None,
            },
            &output,
            &mut NoHost,
        )?;
        Ok((self.pipeline.read_scene_hdr(id)?, report))
    }

    fn tick(
        &mut self,
        id: u64,
        dt: f32,
        emitters: &[ParticleEmitter],
    ) -> Result<ParticleTickReport> {
        self.pipeline
            .advance_particles(ParticleTick { id, dt }, emitters)
    }
}

/// A busy emitter whose pool overflows its 64 slots, so the reference
/// comparison covers spawning, motion, drag, kills and ring-slot drops.
fn fountain(id: &str, seed: u32) -> ParticleEmitter {
    ParticleEmitter {
        id: id.to_owned(),
        position: Vec3::new(0.5, 1.0, -0.25),
        direction: Vec3::new(0.2, 1.0, 0.1),
        capacity: 64,
        spawn_rate: 200.0,
        lifetime: [0.3, 0.6],
        speed: [1.0, 4.0],
        spread: 0.4,
        spawn_extent: 0.1,
        gravity: Vec3::new(0.0, -2.0, 0.0),
        drag: 0.5,
        seed,
        ..Default::default()
    }
}

/// One stationary red particle (half size 1 m) at height `y` over the origin.
fn dot(y: f32) -> ParticleEmitter {
    ParticleEmitter {
        id: "dot".to_owned(),
        position: Vec3::new(0.0, y, 0.0),
        capacity: 4,
        spawn_rate: 4.0,
        lifetime: [100.0, 100.0],
        speed: [0.0, 0.0],
        spread: 0.0,
        gravity: Vec3::ZERO,
        size: [1.0, 1.0],
        start_color: Vec4::new(1.0, 0.0, 0.0, 1.0),
        end_color: Vec4::new(1.0, 0.0, 0.0, 1.0),
        ..Default::default()
    }
}

#[track_caller]
fn assert_matches_reference(
    pipeline: &ScenePipeline,
    id: &str,
    reference: &ParticleReference,
    label: &str,
) {
    let readback = pipeline.read_particles(id).unwrap().expect("live emitter");
    assert_eq!(
        readback.counters, reference.counters,
        "{label}: alive, spawned, dropped and killed counts are exact"
    );
    assert_eq!(
        readback.spawn_attempts,
        reference.cursor.attempts(),
        "{label}"
    );
    assert_eq!(
        readback.rate_limited,
        reference.cursor.rate_limited(),
        "{label}"
    );
    assert_eq!(
        readback.particles.len(),
        reference.particles.len(),
        "{label}"
    );
    assert_eq!(
        readback.alive_slots(),
        reference.counters.alive,
        "{label}: alive count equals the alive slots"
    );
    for (slot, (gpu, cpu)) in readback
        .particles
        .iter()
        .zip(&reference.particles)
        .enumerate()
    {
        assert_eq!(
            gpu.is_alive(),
            cpu.is_alive(),
            "{label}: slot {slot} liveness"
        );
        assert!(
            gpu.matches(cpu),
            "{label}: slot {slot} differs beyond the tolerance\n gpu {gpu:?}\n cpu {cpu:?}"
        );
    }
}

fn changed_pixels(a: &HdrImage, b: &HdrImage) -> usize {
    a.pixels
        .iter()
        .zip(&b.pixels)
        .filter(|(x, y)| x != y)
        .count()
}

fn close(a: [f32; 4], b: [f32; 4], tolerance: f32) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tolerance)
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn gpu_state_matches_the_seeded_cpu_reference_after_n_ticks() -> Result<()> {
    let mut rig = Rig::new()?;
    let emitter = fountain("fountain", 42);
    let dt = 0.05;
    let mut reference = ParticleReference::new(emitter.capacity);
    for tick in 1..=24u64 {
        let report = rig.tick(tick, dt, std::slice::from_ref(&emitter))?;
        assert!(report.advanced);
        reference.step(&emitter, dt);
        if matches!(tick, 1 | 2 | 5 | 8 | 12 | 24) {
            assert_matches_reference(
                &rig.pipeline,
                "fountain",
                &reference,
                &format!("tick {tick}"),
            );
        }
    }
    // The run exercised every path: lifetimes expired and the pool overflowed.
    let final_counters = reference.counters;
    assert!(final_counters.killed > 0, "{final_counters:?}");
    assert!(final_counters.dropped > 0, "{final_counters:?}");
    assert!(final_counters.alive <= 64);

    // A different seed gives a different (but still reference-exact) pool.
    let other = fountain("other", 7);
    let mut other_reference = ParticleReference::new(other.capacity);
    for tick in 25..=30u64 {
        rig.tick(tick, dt, &[emitter.clone(), other.clone()])?;
        reference.step(&emitter, dt);
        other_reference.step(&other, dt);
    }
    assert_matches_reference(&rig.pipeline, "fountain", &reference, "first of two");
    assert_matches_reference(&rig.pipeline, "other", &other_reference, "second of two");
    let first = rig.pipeline.read_particles("fountain")?.unwrap();
    let second = rig.pipeline.read_particles("other")?.unwrap();
    assert_ne!(first.particles, second.particles, "seeds change the state");
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn a_moving_emitter_spawns_where_it_is_at_each_tick() -> Result<()> {
    let mut rig = Rig::new()?;
    let mut emitter = fountain("mover", 3);
    let mut reference = ParticleReference::new(emitter.capacity);
    for tick in 1..=10u64 {
        emitter.position.x = tick as f32 * 0.5;
        rig.tick(tick, 0.05, std::slice::from_ref(&emitter))?;
        reference.step(&emitter, 0.05);
    }
    assert_matches_reference(&rig.pipeline, "mover", &reference, "moving emitter");
    let xs: Vec<f32> = rig
        .pipeline
        .read_particles("mover")?
        .unwrap()
        .particles
        .iter()
        .filter(|p| p.is_alive())
        .map(|p| p.position[0])
        .collect();
    let (low, high) = xs
        .iter()
        .fold((f32::MAX, f32::MIN), |(l, h), x| (l.min(*x), h.max(*x)));
    assert!(
        high - low > 1.0,
        "particles trail the emitter: {low}..{high}"
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn repeated_ticks_and_extra_views_do_not_advance_the_simulation() -> Result<()> {
    let mut rig = Rig::new()?;
    let emitter = fountain("fountain", 42);
    for tick in 1..=3u64 {
        rig.tick(tick, 0.05, std::slice::from_ref(&emitter))?;
    }
    let before = rig.pipeline.read_particles("fountain")?.unwrap();
    let report = rig.pipeline.particle_report();
    assert_eq!((report.simulated_ticks, report.repeated_ticks), (3, 0));
    assert_eq!(report.last_tick, Some(3));

    // The same tick id again, even with different input, is a no-op.
    let mut changed = emitter.clone();
    changed.position.y = 3.0;
    let repeat = rig.tick(3, 0.05, &[changed])?;
    assert!(!repeat.advanced);
    assert_eq!(rig.pipeline.read_particles("fountain")?.unwrap(), before);

    // Render the tick for three views of different sizes and several frames
    // each (eyes, split screen and presentation passes all look like this).
    for (id, size) in [(ViewId::MAIN, 64), (ViewId(2), 32), (ViewId(3), 64)] {
        for _ in 0..3 {
            let (_, view_report) = rig.render(id, size, &[])?;
            assert_eq!(
                view_report.features.get(names::PARTICLES).unwrap().activity,
                Activity::Ran
            );
        }
        let view = rig.pipeline.particle_view_report(id).unwrap();
        assert_eq!(view.emitters_drawn, 1, "view {id:?}");
        assert!(view.instances > 0);
    }
    assert_eq!(rig.pipeline.read_particles("fountain")?.unwrap(), before);
    let report = rig.pipeline.particle_report();
    assert_eq!((report.simulated_ticks, report.repeated_ticks), (3, 1));
    assert_eq!(report.emitters[0].ticks, 3);

    // Only a new tick id advances, exactly once.
    let next = rig.tick(4, 0.05, std::slice::from_ref(&emitter))?;
    assert!(next.advanced);
    assert_ne!(rig.pipeline.read_particles("fountain")?.unwrap(), before);
    let report = rig.pipeline.particle_report();
    assert_eq!((report.simulated_ticks, report.last_tick), (4, Some(4)));

    // Closing a view drops its report; the simulation is untouched.
    rig.pipeline.forget_view(ViewId(2));
    assert_eq!(rig.pipeline.particle_view_report(ViewId(2)), None);
    assert_eq!(rig.pipeline.particle_report().simulated_ticks, 4);
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn overflow_keeps_the_pool_at_capacity_and_counts_dropped_spawns() -> Result<()> {
    let mut rig = Rig::new()?;
    let mut emitter = fountain("overflow", 5);
    emitter.capacity = 16;
    emitter.spawn_rate = 2000.0;
    emitter.lifetime = [100.0, 100.0];
    let mut reference = ParticleReference::new(16);
    for tick in 1..=3u64 {
        rig.tick(tick, 0.05, std::slice::from_ref(&emitter))?;
        reference.step(&emitter, 0.05);
    }
    let readback = rig.pipeline.read_particles("overflow")?.unwrap();
    // 100 spawns asked per tick, 16 tried: the first tick fills the pool and
    // every later try finds a live slot.
    assert_eq!(readback.counters.alive, 16);
    assert_eq!(readback.alive_slots(), 16);
    assert_eq!(readback.particles.len(), 16, "the pool never grows");
    assert_eq!(readback.counters.spawned, 16);
    assert_eq!(readback.counters.dropped, 32);
    assert_eq!(readback.rate_limited, 3 * 84);
    assert_eq!(
        u64::from(readback.counters.spawned) + readback.dropped_total(),
        300,
        "every requested spawn is spawned or counted as dropped"
    );
    assert_matches_reference(&rig.pipeline, "overflow", &reference, "overflow");
    let report = rig.pipeline.particle_report();
    assert_eq!(report.emitters[0].spawn_attempts, 48);
    assert_eq!(report.emitters[0].high_water, 16);
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn invalid_input_is_rejected_before_anything_changes() -> Result<()> {
    let mut rig = Rig::new()?;
    let good = fountain("good", 1);
    rig.tick(1, 0.05, std::slice::from_ref(&good))?;
    let before = rig.pipeline.read_particles("good")?.unwrap();

    let mut nan = good.clone();
    nan.position.x = f32::NAN;
    let mut no_capacity = good.clone();
    no_capacity.capacity = 0;
    let mut too_big = good.clone();
    too_big.capacity = MAX_PARTICLES_PER_EMITTER + 1;
    let mut negative_rate = good.clone();
    negative_rate.spawn_rate = -1.0;
    let mut negative_lifetime = good.clone();
    negative_lifetime.lifetime = [-1.0, 1.0];
    for (name, emitters) in [
        ("non-finite", vec![nan]),
        ("zero capacity", vec![no_capacity]),
        ("excess capacity", vec![too_big]),
        ("negative rate", vec![negative_rate]),
        ("negative lifetime", vec![negative_lifetime]),
        ("duplicate ids", vec![good.clone(), good.clone()]),
        (
            "too many emitters",
            (0..=MAX_EMITTERS)
                .map(|i| ParticleEmitter {
                    id: format!("e{i}"),
                    capacity: 1,
                    ..good.clone()
                })
                .collect(),
        ),
    ] {
        assert!(rig.tick(2, 0.05, &emitters).is_err(), "{name}");
    }
    for dt in [0.0, -0.05, f32::NAN, f32::INFINITY, MAX_TICK_DT * 2.0] {
        assert!(
            rig.tick(2, dt, std::slice::from_ref(&good)).is_err(),
            "dt {dt}"
        );
    }
    // Nothing was simulated, freed or created by the rejected calls.
    let report = rig.pipeline.particle_report();
    assert_eq!((report.simulated_ticks, report.last_tick), (1, Some(1)));
    assert_eq!(report.emitters.len(), 1);
    assert_eq!(rig.pipeline.read_particles("good")?.unwrap(), before);
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn particles_change_pixels_where_expected_and_no_particles_is_the_baseline() -> Result<()> {
    let mut rig = Rig::new()?;
    let (baseline, base_report) = rig.render(ViewId::MAIN, 64, &[])?;
    assert_eq!(
        base_report.features.get(names::PARTICLES).unwrap().activity,
        Activity::Disabled,
        "no emitters, no pass"
    );
    assert_eq!(rig.pipeline.particle_view_report(ViewId::MAIN), None);

    // An emitter that has not spawned anything yet draws nothing either.
    let mut idle = dot(1.0);
    idle.spawn_rate = 0.0;
    rig.tick(1, 0.25, &[idle])?;
    let (still_baseline, _) = rig.render(ViewId::MAIN, 64, &[])?;
    assert_eq!(still_baseline.pixels, baseline.pixels);

    // One alpha-blended red disc of radius 1 m (8 px) at the centre.
    rig.tick(2, 0.25, &[dot(1.0)])?;
    let (image, report) = rig.render(ViewId::MAIN, 64, &[])?;
    assert_eq!(
        report.features.get(names::PARTICLES).unwrap().activity,
        Activity::Ran
    );
    assert!(
        close(
            image.pixel(32, 32),
            [1.0, 0.0, 0.0, image.pixel(32, 32)[3]],
            0.01
        ),
        "opaque red over the floor: {:?}",
        image.pixel(32, 32)
    );
    assert!(close(
        image.pixel(35, 33),
        [1.0, 0.0, 0.0, image.pixel(35, 33)[3]],
        0.01
    ));
    for (x, y) in [
        (2, 2),
        (60, 60),
        (32, 8),
        (32, 56),
        (8, 32),
        (56, 32),
        (45, 32),
    ] {
        assert_eq!(
            image.pixel(x, y),
            baseline.pixel(x, y),
            "({x}, {y}) is outside"
        );
    }
    let changed = changed_pixels(&baseline, &image);
    assert!(
        (180..=215).contains(&changed),
        "an 8 px radius disc is about 201 pixels, got {changed}"
    );

    // Additive: the floor stays and the red adds on top.
    let mut additive = dot(1.0);
    additive.id = "glow".to_owned();
    additive.blend = ParticleBlend::Additive;
    rig.tick(3, 0.25, &[additive])?;
    let (glow, _) = rig.render(ViewId::MAIN, 64, &[])?;
    let floor = baseline.pixel(32, 32);
    assert!(
        close(
            glow.pixel(32, 32),
            [floor[0] + 1.0, floor[1], floor[2], floor[3]],
            0.02
        ),
        "additive red on grey: {:?} over {floor:?}",
        glow.pixel(32, 32)
    );
    assert_eq!(glow.pixel(2, 2), baseline.pixel(2, 2));

    // Removing the emitter restores the baseline.
    rig.tick(4, 0.25, &[])?;
    let (restored, report) = rig.render(ViewId::MAIN, 64, &[])?;
    assert_eq!(restored.pixels, baseline.pixels);
    assert_eq!(
        report.features.get(names::PARTICLES).unwrap().activity,
        Activity::Disabled
    );
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn nearer_geometry_occludes_particles() -> Result<()> {
    let mut rig = Rig::new()?;
    // A blue roof over x < 0, 1 m above the particle and nearer the camera.
    let roof_mesh = quad(&rig.gpu, 20.0);
    let roof = unlit(
        "roof",
        &roof_mesh,
        Mat4::from_translation(Vec3::new(-20.0, 2.0, 0.0)),
        [0.0, 0.0, 1.0],
    );
    let (roofed, _) = rig.render(ViewId::MAIN, 64, std::slice::from_ref(&roof))?;
    rig.tick(1, 0.25, &[dot(1.0)])?;
    let (image, _) = rig.render(ViewId::MAIN, 64, std::slice::from_ref(&roof))?;
    // Left of the centre the roof hides the disc; right of it the disc shows.
    assert_eq!(
        image.pixel(28, 32),
        roofed.pixel(28, 32),
        "hidden by the roof"
    );
    assert!(close(
        roofed.pixel(28, 32),
        [0.0, 0.0, 1.0, roofed.pixel(28, 32)[3]],
        0.01
    ));
    assert!(
        close(
            image.pixel(36, 32),
            [1.0, 0.0, 0.0, image.pixel(36, 32)[3]],
            0.01
        ),
        "visible right of the roof edge: {:?}",
        image.pixel(36, 32)
    );
    // Emitters draw in id order and later ones land over earlier ones where
    // both pass the depth test: a larger green emitter below the red one.
    let mut behind = dot(0.5);
    behind.id = "behind".to_owned();
    behind.start_color = Vec4::new(0.0, 1.0, 0.0, 1.0);
    behind.end_color = behind.start_color;
    behind.size = [2.0, 2.0];
    rig.tick(2, 0.25, &[dot(1.0), behind])?;
    let (two, _) = rig.render(ViewId::MAIN, 64, &[])?;
    // "behind" draws before "dot": red over green at the centre, green
    // outside the red disc.
    assert!(close(
        two.pixel(32, 32),
        [1.0, 0.0, 0.0, two.pixel(32, 32)[3]],
        0.01
    ));
    assert!(close(
        two.pixel(32 + 12, 32),
        [0.0, 1.0, 0.0, two.pixel(44, 32)[3]],
        0.01
    ));
    Ok(())
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn targets_resize_and_the_emitter_lifetime_frees_and_restarts_pools() -> Result<()> {
    let mut rig = Rig::new()?;
    let (baseline, _) = rig.render(ViewId::MAIN, 64, &[])?;
    rig.tick(1, 0.25, &[dot(1.0)])?;
    let (small, _) = rig.render(ViewId::MAIN, 64, &[])?;
    assert!(close(
        small.pixel(32, 32),
        [1.0, 0.0, 0.0, small.pixel(32, 32)[3]],
        0.01
    ));
    // The same view at another size reallocates its targets; the pool is
    // untouched and the disc is centred (12 px per metre now).
    let (large, _) = rig.render(ViewId::MAIN, 96, &[])?;
    assert_eq!(large.size, [96, 96]);
    assert!(close(
        large.pixel(48, 48),
        [1.0, 0.0, 0.0, large.pixel(48, 48)[3]],
        0.01
    ));
    assert!(close(
        large.pixel(48 + 10, 48),
        [1.0, 0.0, 0.0, large.pixel(58, 48)[3]],
        0.01
    ));
    assert_eq!(rig.pipeline.particle_report().simulated_ticks, 1);
    let (back, _) = rig.render(ViewId::MAIN, 64, &[])?;
    assert_eq!(back.pixels, small.pixels, "resizing back is lossless");

    // A changed capacity restarts the emitter with an empty pool.
    let mut bigger = dot(1.0);
    bigger.capacity = 8;
    bigger.spawn_rate = 0.0;
    rig.tick(2, 0.25, std::slice::from_ref(&bigger))?;
    let restarted = rig.pipeline.read_particles("dot")?.unwrap();
    assert_eq!(restarted.particles.len(), 8);
    assert_eq!(restarted.counters.alive, 0);
    assert_eq!(restarted.spawn_attempts, 0);
    assert_eq!(rig.pipeline.particle_report().emitters[0].ticks, 1);
    let (empty, _) = rig.render(ViewId::MAIN, 64, &[])?;
    assert_eq!(
        empty.pixels, baseline.pixels,
        "an unspawned pool draws nothing"
    );

    // An emitter that leaves the set frees its pool and its report.
    rig.tick(3, 0.25, &[dot(1.0)])?;
    assert!(rig.pipeline.read_particles("dot")?.is_some());
    rig.tick(4, 0.25, &[])?;
    assert!(rig.pipeline.read_particles("dot")?.is_none());
    assert!(rig.pipeline.particle_report().emitters.is_empty());
    let (_, report) = rig.render(ViewId::MAIN, 64, &[])?;
    assert_eq!(
        report.features.get(names::PARTICLES).unwrap().activity,
        Activity::Disabled
    );
    assert_eq!(rig.pipeline.particle_view_report(ViewId::MAIN), None);
    Ok(())
}
