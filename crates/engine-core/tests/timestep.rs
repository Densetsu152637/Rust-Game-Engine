//! Fixed-step time: delta validation, accumulation, bounded ticks and invariants.
//!
//! `EngineApp` itself advances exactly one fixed step per `tick(dt)`; accumulating
//! real frame time is a host concern. The `FrameDriver` below is the standard host
//! pattern built purely on `tick`, used to verify that the engine's observable time
//! stays consistent under accumulation, fractional carry, spikes and clamping.
mod common;

use common::*;
use engine_core::prelude::*;

/// Host-side fixed-timestep accumulator driving `EngineApp::tick`.
struct FrameDriver {
    accumulator: f64,
    step: f32,
    max_steps: u32,
    dropped: f64,
    total_in: f64,
}

impl FrameDriver {
    fn new(step: f32, max_steps: u32) -> Self {
        Self {
            accumulator: 0.0,
            step,
            max_steps,
            dropped: 0.0,
            total_in: 0.0,
        }
    }

    /// Feeds one real frame; returns the number of simulation ticks executed.
    fn frame(&mut self, app: &mut EngineApp, real_dt: f64) -> u32 {
        assert!(real_dt.is_finite() && real_dt >= 0.0);
        self.total_in += real_dt;
        self.accumulator += real_dt;
        let step = f64::from(self.step);
        let mut ran = 0;
        while self.accumulator >= step && ran < self.max_steps {
            app.tick(self.step).unwrap();
            self.accumulator -= step;
            ran += 1;
        }
        if self.accumulator >= step {
            let whole_steps = (self.accumulator / step).floor();
            self.dropped += whole_steps * step;
            self.accumulator -= whole_steps * step;
        }
        ran
    }
}

fn counting_app() -> EngineApp {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app).add_systems(|mut c: ResMut<Counter>| c.0 += 1);
    app
}

fn updates(app: &EngineApp) -> i64 {
    app.world().resource::<Counter>().0
}

// ---- delta validation --------------------------------------------------------

fn invalid_dt_is_rejected_without_side_effects(dt: f32) {
    let mut app = counting_app();
    assert!(app.tick(dt).is_err(), "dt {dt:?} must be rejected");
    assert_eq!(app.state(), AppState::Configuring);
    assert_eq!(app.time().tick, 0);
    assert_eq!(app.time().elapsed, 0.0);
    assert_eq!(updates(&app), 0);
    assert!(app.run_fixed(3, dt).is_err());
    assert_eq!(app.state(), AppState::Configuring);
    assert_eq!(updates(&app), 0);
}

macro_rules! invalid_dt_cases {
    ($($name:ident: $dt:expr;)*) => {
        $( #[test] fn $name() { invalid_dt_is_rejected_without_side_effects($dt); } )*
    };
}

invalid_dt_cases! {
    zero_delta_is_rejected: 0.0;
    negative_zero_delta_is_rejected: -0.0;
    negative_delta_is_rejected: -0.25;
    tiny_negative_delta_is_rejected: -1.0e-30;
    most_negative_finite_delta_is_rejected: f32::MIN;
    nan_delta_is_rejected: f32::NAN;
    positive_infinity_delta_is_rejected: f32::INFINITY;
    negative_infinity_delta_is_rejected: f32::NEG_INFINITY;
}

#[test]
fn invalid_delta_on_a_running_app_does_not_advance_time() {
    let mut app = counting_app();
    app.tick(0.5).unwrap();
    for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(app.tick(bad).is_err());
    }
    assert_eq!(
        (app.time().tick, app.time().elapsed, updates(&app)),
        (1, 0.5, 1)
    );
    assert_eq!(app.state(), AppState::Running);
}

fn valid_dt_is_recorded_exactly(dt: f32) {
    let mut app = counting_app();
    app.tick(dt).unwrap();
    assert_eq!(app.time().dt, dt);
    assert_eq!(app.time().elapsed, f64::from(dt));
    assert_eq!(app.time().tick, 1);
    assert_eq!(updates(&app), 1);
}

macro_rules! valid_dt_cases {
    ($($name:ident: $dt:expr;)*) => {
        $( #[test] fn $name() { valid_dt_is_recorded_exactly($dt); } )*
    };
}

valid_dt_cases! {
    smallest_positive_normal_delta_is_accepted: f32::MIN_POSITIVE;
    subnormal_delta_is_accepted: 1.0e-40;
    microsecond_delta_is_accepted: 1.0e-6;
    sixty_hertz_delta_is_accepted: 1.0 / 60.0;
    quarter_second_delta_is_accepted: 0.25;
    one_second_delta_is_accepted: 1.0;
    one_thousand_second_delta_is_accepted: 1000.0;
    largest_finite_delta_is_accepted_without_clamping: f32::MAX;
}

// ---- accumulation in the engine ------------------------------------------------

#[test]
fn elapsed_is_the_f64_sum_of_the_f32_deltas() {
    let mut app = counting_app();
    let mut expected = 0.0_f64;
    for _ in 0..10_000 {
        app.tick(1.0 / 60.0).unwrap();
        expected += f64::from(1.0_f32 / 60.0);
    }
    assert_eq!(app.time().elapsed, expected);
    assert!((app.time().elapsed - 10_000.0 / 60.0).abs() < 1e-4);
}

#[test]
fn exact_binary_fractions_accumulate_without_error() {
    let mut app = counting_app();
    app.run_fixed(20_000, 0.015625).unwrap();
    assert_eq!(app.time().elapsed, 20_000.0 / 64.0);
    assert_eq!(app.time().tick, 20_000);
}

#[test]
fn tick_counter_advances_by_one_regardless_of_delta_size() {
    let mut app = counting_app();
    for dt in [1.0e-6_f32, 1000.0, 0.25, 3.0e7, 0.5] {
        let before = app.time().tick;
        app.tick(dt).unwrap();
        assert_eq!(app.time().tick, before + 1);
    }
}

#[test]
fn systems_observe_the_delta_of_their_own_tick() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(|t: Res<Time>, mut s: ResMut<Seen>| {
        s.0.push((t.dt * 1024.0) as i64);
    });
    for dt in [0.5_f32, 0.25, 1.0, 0.125, 2.0] {
        app.tick(dt).unwrap();
    }
    assert_eq!(
        app.world().resource::<Seen>().0,
        [512, 256, 1024, 128, 2048]
    );
}

#[test]
fn first_update_sees_tick_one_and_startup_sees_tick_zero() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    app.schedule_mut(Stage::Startup)
        .add_systems(|t: Res<Time>, mut s: ResMut<Seen>| {
            s.0.push(t.tick as i64 * 100 + t.dt as i64)
        });
    update(&mut app).add_systems(|t: Res<Time>, mut s: ResMut<Seen>| s.0.push(t.tick as i64));
    app.run_fixed(2, 0.5).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [0, 1, 2]);
}

#[test]
fn time_is_identical_in_update_extract_and_render_for_one_tick() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(|t: Res<Time>, mut s: ResMut<Seen>| {
        s.0.push(t.tick as i64);
        s.0.push((t.elapsed * 8.0) as i64);
    });
    app.schedule_mut(Stage::Extract)
        .add_systems(|t: Res<Time>, mut s: ResMut<Seen>| {
            s.0.push(t.tick as i64);
            s.0.push((t.elapsed * 8.0) as i64);
        });
    app.render_world_mut().init_resource::<Seen>();
    app.schedule_mut(Stage::Render)
        .add_systems(|t: Res<Time>, mut s: ResMut<Seen>| {
            s.0.push(t.tick as i64);
            s.0.push((t.elapsed * 8.0) as i64);
        });
    app.tick(0.125).unwrap();
    app.tick(0.25).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [1, 1, 1, 1, 2, 3, 2, 3]);
    assert_eq!(app.render_world().resource::<Seen>().0, [1, 1, 2, 3]);
}

#[test]
fn time_is_retained_for_shutdown_systems() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    app.schedule_mut(Stage::Shutdown)
        .add_systems(|t: Res<Time>, mut s: ResMut<Seen>| s.0.push(t.tick as i64));
    app.run_fixed(7, DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [7]);
}

#[test]
fn tick_overflow_is_reported_without_corrupting_time() {
    let mut app = counting_app();
    app.startup().unwrap();
    app.world_mut().resource_mut::<Time>().tick = u64::MAX;
    app.world_mut().resource_mut::<Time>().elapsed = 5.0;
    let error = app.tick(DT).unwrap_err().to_string();
    assert!(error.contains("tick overflow"), "{error}");
    assert_eq!(app.time().tick, u64::MAX);
    assert_eq!(app.time().elapsed, 5.0);
    assert_eq!(updates(&app), 0);
}

#[test]
fn last_representable_tick_is_still_accepted() {
    let mut app = counting_app();
    app.startup().unwrap();
    app.world_mut().resource_mut::<Time>().tick = u64::MAX - 1;
    app.tick(DT).unwrap();
    assert_eq!(app.time().tick, u64::MAX);
    assert!(app.tick(DT).is_err());
}

// ---- bounded runs ----------------------------------------------------------------

macro_rules! run_fixed_cases {
    ($($name:ident: $ticks:expr, $dt:expr;)*) => {
        $(
            #[test]
            fn $name() {
                let mut app = counting_app();
                app.run_fixed($ticks, $dt).unwrap();
                assert_eq!(updates(&app), $ticks as i64);
                assert_eq!(app.time().tick, $ticks);
                assert_eq!(app.time().elapsed, ($ticks as f64) * f64::from($dt));
                assert_eq!(app.state(), AppState::Stopped);
            }
        )*
    };
}

run_fixed_cases! {
    run_fixed_one_tick_of_half_a_second: 1, 0.5_f32;
    run_fixed_eight_ticks_of_an_eighth: 8, 0.125_f32;
    run_fixed_sixty_four_ticks_of_a_quarter: 64, 0.25_f32;
    run_fixed_one_thousand_ticks_of_one_sixty_fourth: 1000, 0.015625_f32;
    run_fixed_three_ticks_of_one_hundred_seconds: 3, 100.0_f32;
}

#[test]
fn run_fixed_with_zero_ticks_never_updates() {
    let mut app = counting_app();
    app.run_fixed(0, DT).unwrap();
    assert_eq!((updates(&app), app.time().tick), (0, 0));
}

#[test]
fn run_fixed_is_bounded_even_when_asked_for_a_huge_tick_count_and_stopped() {
    let mut app = counting_app();
    update(&mut app).add_systems(|t: Res<Time>, mut c: ResMut<RuntimeControl>| {
        if t.tick == 25 {
            c.request_stop();
        }
    });
    app.run_fixed(u64::MAX, DT).unwrap();
    assert_eq!(app.time().tick, 25);
    assert_eq!(updates(&app), 25);
}

#[test]
fn consecutive_run_fixed_calls_cannot_resume_a_stopped_app() {
    let mut app = counting_app();
    app.run_fixed(3, DT).unwrap();
    assert!(app.run_fixed(3, DT).is_err());
    assert_eq!(updates(&app), 3);
}

#[test]
fn many_small_ticks_equal_the_same_simulated_time_as_fewer_large_ticks() {
    let mut fine = counting_app();
    let mut coarse = counting_app();
    fine.run_fixed(64, 0.015625).unwrap();
    coarse.run_fixed(4, 0.25).unwrap();
    assert_eq!(fine.time().elapsed, coarse.time().elapsed);
    assert_ne!(fine.time().tick, coarse.time().tick);
}

// ---- host accumulator pattern -----------------------------------------------------

macro_rules! driver_cases {
    ($($name:ident: frame $frame:expr => ticks $ticks:expr, residual $residual:expr;)*) => {
        $(
            #[test]
            fn $name() {
                let mut app = counting_app();
                let mut driver = FrameDriver::new(0.25, 5);
                let ran = driver.frame(&mut app, $frame);
                assert_eq!(ran, $ticks as u32);
                assert_eq!(driver.accumulator, $residual);
                assert_eq!(app.time().tick, ($ticks as u64));
                assert_eq!(updates(&app), ($ticks as i64));
            }
        )*
    };
}

driver_cases! {
    zero_frame_time_runs_no_ticks: frame 0.0 => ticks 0, residual 0.0;
    sub_step_frame_is_carried_not_run: frame 0.125 => ticks 0, residual 0.125;
    just_under_one_step_is_carried: frame 0.2490234375 => ticks 0, residual 0.2490234375;
    exactly_one_step_runs_one_tick: frame 0.25 => ticks 1, residual 0.0;
    one_and_a_half_steps_runs_one_tick_with_carry: frame 0.375 => ticks 1, residual 0.125;
    exactly_three_steps_runs_three_ticks: frame 0.75 => ticks 3, residual 0.0;
    exactly_at_the_cap_runs_five_ticks: frame 1.25 => ticks 5, residual 0.0;
    one_step_over_the_cap_is_clamped_and_dropped: frame 1.5 => ticks 5, residual 0.0;
    fractional_excess_beyond_the_cap_keeps_only_the_remainder: frame 1.625 => ticks 5, residual 0.125;
    enormous_spike_is_clamped_to_the_cap: frame 1.0e9 => ticks 5, residual 0.0;
}

#[test]
fn fractional_carry_accumulates_across_frames() {
    let mut app = counting_app();
    let mut driver = FrameDriver::new(0.25, 8);
    let ran: Vec<u32> = [0.125, 0.125, 0.125, 0.125, 0.375, 0.125]
        .iter()
        .map(|f| driver.frame(&mut app, *f))
        .collect();
    assert_eq!(ran, [0, 1, 0, 1, 1, 1]);
    assert_eq!(driver.accumulator, 0.0);
    assert_eq!(app.time().elapsed, 1.0);
}

#[test]
fn zero_length_frames_between_real_frames_change_nothing() {
    let mut app = counting_app();
    let mut driver = FrameDriver::new(0.25, 8);
    for frame in [0.0, 0.25, 0.0, 0.0, 0.5, 0.0] {
        driver.frame(&mut app, frame);
    }
    assert_eq!(app.time().tick, 3);
}

#[test]
fn driver_conserves_real_time_across_seeded_random_frames() {
    for seed in [1_u64, 2, 3, 99, 2026] {
        let mut rng = Rng::new(seed);
        let mut app = counting_app();
        let mut driver = FrameDriver::new(0.25, 4);
        let mut max_ran = 0;
        for _ in 0..2_000 {
            // Multiples of 1/32 keep every sum exactly representable.
            let frame = match rng.below(10) {
                0 => 0.0,
                1 => rng.below(2000) as f64 / 32.0, // occasional spike
                _ => rng.below(20) as f64 / 32.0,
            };
            max_ran = max_ran.max(driver.frame(&mut app, frame));
        }
        let simulated = app.time().elapsed;
        assert!(
            max_ran <= 4,
            "seed {seed}: ticks per frame must stay bounded"
        );
        assert!(driver.accumulator >= 0.0 && driver.accumulator < 0.25);
        assert_eq!(
            simulated + driver.accumulator + driver.dropped,
            driver.total_in,
            "seed {seed}: real time must be conserved"
        );
        assert_eq!(app.time().tick as f64 * 0.25, simulated);
    }
}

#[test]
fn spikes_never_exceed_the_per_frame_tick_budget() {
    let mut app = counting_app();
    let mut driver = FrameDriver::new(0.25, 3);
    for _ in 0..50 {
        let before = app.time().tick;
        driver.frame(&mut app, 1.0e6);
        assert_eq!(app.time().tick - before, 3);
    }
    assert!(driver.dropped > 0.0);
}

fn state_after_pacing(frames: &[f64]) -> (u64, i64) {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    let body = app
        .world_mut()
        .spawn((Pos::default(), Vel { x: 3, y: -1 }))
        .id();
    update(&mut app).add_systems(|mut q: Query<(&mut Pos, &Vel)>| {
        for (mut pos, vel) in &mut q {
            pos.x += vel.x;
            pos.y += vel.y;
        }
    });
    let mut driver = FrameDriver::new(0.25, 1_000);
    for frame in frames {
        driver.frame(&mut app, *frame);
    }
    let pos = *app.world().get::<Pos>(body).unwrap();
    (app.time().tick, i64::from(pos.x) * 1_000 + i64::from(pos.y))
}

#[test]
fn simulation_result_does_not_depend_on_how_real_time_is_sliced_into_frames() {
    let one_big = state_after_pacing(&[8.0]);
    let regular = state_after_pacing(&[0.25; 32]);
    let irregular = state_after_pacing(&[
        0.125, 0.375, 1.5, 0.0, 2.25, 0.5, 0.25, 0.375, 2.0, 0.5, 0.125, 0.0, 0.0, 0.0625, 0.0625,
    ]);
    let tiny = state_after_pacing(&[0.0625; 128]);
    assert_eq!(one_big, regular);
    assert_eq!(regular, irregular);
    assert_eq!(irregular, tiny);
    assert_eq!(one_big.0, 32);
}

#[test]
fn integrated_motion_matches_the_closed_form_for_exact_steps() {
    let mut app = app();
    let e = app
        .world_mut()
        .spawn((Pos::default(), Vel { x: 2, y: 0 }))
        .id();
    update(&mut app).add_systems(|t: Res<Time>, mut q: Query<(&mut Pos, &Vel)>| {
        for (mut p, v) in &mut q {
            p.x += (f64::from(t.dt) * 4.0) as i32 * v.x;
        }
    });
    app.run_fixed(40, 0.25).unwrap();
    // 40 ticks * (0.25 * 4 = 1) * 2
    assert_eq!(app.world().get::<Pos>(e).unwrap().x, 80);
}

#[test]
fn elapsed_time_is_strictly_monotonic_for_varying_deltas() {
    let mut rng = Rng::new(404);
    let mut app = counting_app();
    let mut previous = app.time().elapsed;
    for _ in 0..500 {
        let dt = (rng.below(1000) + 1) as f32 / 256.0;
        app.tick(dt).unwrap();
        assert!(app.time().elapsed > previous);
        previous = app.time().elapsed;
    }
}
