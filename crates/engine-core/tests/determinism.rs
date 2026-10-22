//! Parallel schedule determinism: identical results across runs, executors and threads.
mod common;

use common::*;
use engine_core::bevy_ecs::schedule::{MultiThreadedExecutor, SingleThreadedExecutor};
use engine_core::prelude::*;

#[derive(Component, Clone, Copy, Debug, PartialEq)]
struct Fx(f32);
#[derive(Component, Clone, Copy, Debug, PartialEq)]
struct Energy(i64);

#[derive(Resource)]
struct Dice(Rng);

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
struct Par;

#[derive(Clone, Copy, Debug)]
enum Exec {
    Default,
    Single,
    Multi,
}

/// Builds a simulation whose systems are conflict-free (so they may run in parallel),
/// plus ordered stages that mix their results.
fn build(seed: u64, bodies: usize, exec: Exec, shuffle_registration: bool) -> EngineApp {
    let mut app = app();
    match exec {
        Exec::Default => {}
        Exec::Single => {
            update(&mut app).set_executor(SingleThreadedExecutor::new());
        }
        Exec::Multi => {
            update(&mut app).set_executor(MultiThreadedExecutor::new());
        }
    }
    let mut rng = Rng::new(seed);
    for _ in 0..bodies {
        app.world_mut().spawn((
            Pos {
                x: rng.below(100) as i32,
                y: rng.below(100) as i32,
            },
            Vel {
                x: rng.below(7) as i32 - 3,
                y: rng.below(7) as i32 - 3,
            },
            Health(rng.below(50) as i32),
            Fx(rng.below(1000) as f32 / 7.0),
            Energy(rng.below(500) as i64),
        ));
    }
    app.world_mut()
        .insert_resource(Dice(Rng::new(seed ^ 0xABCD)));
    app.world_mut().init_resource::<Counter>();
    let mut systems: Vec<(u8, bool)> = vec![(0, false), (1, false), (2, false), (3, false)];
    if shuffle_registration {
        systems.reverse();
    }
    for (which, _) in systems {
        match which {
            0 => {
                update(&mut app).add_systems(
                    (|mut q: Query<(&mut Pos, &Vel)>| {
                        for (mut p, v) in &mut q {
                            p.x += v.x;
                            p.y += v.y;
                        }
                    })
                    .in_set(Par),
                );
            }
            1 => {
                update(&mut app).add_systems(
                    (|mut q: Query<&mut Fx>| {
                        for mut f in &mut q {
                            f.0 = f.0 * 0.5 + 1.25;
                        }
                    })
                    .in_set(Par),
                );
            }
            2 => {
                update(&mut app).add_systems(
                    (|mut q: Query<&mut Health>| {
                        for mut h in &mut q {
                            h.0 = (h.0 * 3 + 1) % 97;
                        }
                    })
                    .in_set(Par),
                );
            }
            _ => {
                update(&mut app).add_systems(
                    (|mut q: Query<&mut Energy>| {
                        for mut e in &mut q {
                            e.0 = (e.0 * 5 + 7) % 1009;
                        }
                    })
                    .in_set(Par),
                );
            }
        }
    }
    // Ordered tail: serial use of the RNG and a reduction over the parallel results.
    update(&mut app).add_systems(
        (
            |mut dice: ResMut<Dice>, mut q: Query<&mut Health>| {
                for mut h in &mut q {
                    h.0 += dice.0.below(5) as i32;
                }
            },
            |q: Query<(&Pos, &Energy)>, mut c: ResMut<Counter>| {
                c.0 = c.0.wrapping_mul(31).wrapping_add(
                    q.iter()
                        .map(|(p, e)| i64::from(p.x) * 3 + i64::from(p.y) + e.0)
                        .sum::<i64>(),
                );
            },
        )
            .chain()
            .after(Par),
    );
    app
}

fn fingerprint(app: &mut EngineApp) -> Vec<u64> {
    let mut rows: Vec<(u64, u64)> = app
        .world_mut()
        .query::<(Entity, &Pos, &Health, &Fx, &Energy)>()
        .iter(app.world())
        .map(|(e, p, h, f, en)| {
            let mix = (p.x as u64)
                ^ ((p.y as u64) << 16)
                ^ ((h.0 as u64) << 32)
                ^ (u64::from(f.0.to_bits()) << 8)
                ^ ((en.0 as u64) << 48);
            (e.to_bits(), mix)
        })
        .collect();
    rows.sort();
    let mut out: Vec<u64> = rows.into_iter().flat_map(|(a, b)| [a, b]).collect();
    out.push(app.world().resource::<Counter>().0 as u64);
    out
}

fn run(seed: u64, bodies: usize, exec: Exec, shuffle: bool, ticks: u64) -> Vec<u64> {
    let mut app = build(seed, bodies, exec, shuffle);
    app.run_fixed(ticks, DT).unwrap();
    fingerprint(&mut app)
}

macro_rules! executor_cases {
    ($($name:ident: seed $seed:expr, bodies $bodies:expr, ticks $ticks:expr;)*) => {
        $(
            #[test]
            fn $name() {
                let reference = run($seed, $bodies, Exec::Single, false, $ticks);
                for exec in [Exec::Default, Exec::Multi, Exec::Single] {
                    for shuffle in [false, true] {
                        assert_eq!(run($seed, $bodies, exec, shuffle, $ticks), reference, "{exec:?} shuffle={shuffle}");
                    }
                }
            }
        )*
    };
}

executor_cases! {
    executors_agree_on_a_tiny_world: seed 1, bodies 3, ticks 10;
    executors_agree_on_a_small_world: seed 2, bodies 25, ticks 30;
    executors_agree_on_a_large_world: seed 3, bodies 600, ticks 20;
    executors_agree_with_another_seed: seed 424242, bodies 100, ticks 40;
    executors_agree_for_a_single_tick: seed 9, bodies 64, ticks 1;
    executors_agree_for_one_body_many_ticks: seed 10, bodies 1, ticks 200;
}

#[test]
fn different_seeds_produce_different_outcomes() {
    let a = run(1, 40, Exec::Default, false, 15);
    let b = run(2, 40, Exec::Default, false, 15);
    assert_ne!(a, b);
}

#[test]
fn one_hundred_repeated_multithreaded_runs_are_bitwise_identical() {
    let reference = run(7, 80, Exec::Multi, false, 12);
    for _ in 0..100 {
        assert_eq!(run(7, 80, Exec::Multi, false, 12), reference);
    }
}

#[test]
fn concurrent_apps_on_many_os_threads_do_not_interfere() {
    let reference = run(11, 50, Exec::Default, false, 25);
    let handles: Vec<_> = (0..12)
        .map(|_| std::thread::spawn(|| run(11, 50, Exec::Default, false, 25)))
        .collect();
    for handle in handles {
        assert_eq!(handle.join().unwrap(), reference);
    }
}

#[test]
fn splitting_a_run_into_single_ticks_matches_run_fixed() {
    let whole = run(5, 30, Exec::Default, false, 20);
    let mut app = build(5, 30, Exec::Default, false);
    for _ in 0..20 {
        app.tick(DT).unwrap();
    }
    app.shutdown().unwrap();
    assert_eq!(fingerprint(&mut app), whole);
}

#[test]
fn parallel_iteration_reductions_are_deterministic_for_integers() {
    use std::sync::atomic::{AtomicI64, Ordering};
    let mut expected = None;
    for _ in 0..30 {
        let mut app = build(13, 400, Exec::Multi, false);
        app.tick(DT).unwrap();
        let total = AtomicI64::new(0);
        let mut state = app.world_mut().query::<&Energy>();
        state.par_iter(app.world()).for_each(|e| {
            total.fetch_add(e.0, Ordering::Relaxed);
        });
        let total = total.load(Ordering::Relaxed);
        assert_eq!(*expected.get_or_insert(total), total);
    }
}

#[test]
fn par_iter_mut_updates_every_entity_exactly_once() {
    let mut app = build(17, 500, Exec::Multi, false);
    app.tick(DT).unwrap(); // running the multithreaded executor initialises the task pool
    let before: i64 = app
        .world_mut()
        .query::<&Energy>()
        .iter(app.world())
        .map(|e| e.0)
        .sum();
    let mut state = app.world_mut().query::<&mut Energy>();
    state
        .par_iter_mut(app.world_mut())
        .for_each(|mut e| e.0 += 1);
    let after: i64 = app
        .world_mut()
        .query::<&Energy>()
        .iter(app.world())
        .map(|e| e.0)
        .sum();
    assert_eq!(after - before, 500);
}

#[test]
fn chained_systems_produce_the_same_entity_handles_every_run() {
    fn ids() -> Vec<u64> {
        let mut app = app();
        update(&mut app).add_systems(
            (
                |mut c: Commands| {
                    for i in 0..20 {
                        c.spawn(Health(i));
                    }
                },
                |mut c: Commands, q: Query<Entity, With<Health>>| {
                    for e in &q {
                        c.entity(e).insert(Tag);
                    }
                    for _ in 0..5 {
                        c.spawn(Other);
                    }
                },
            )
                .chain(),
        );
        app.run_fixed(3, DT).unwrap();
        let mut v: Vec<u64> = app
            .world_mut()
            .query::<Entity>()
            .iter(app.world())
            .map(|e| e.to_bits())
            .collect();
        v.sort();
        v
    }
    let reference = ids();
    for _ in 0..20 {
        assert_eq!(ids(), reference);
    }
}

#[test]
fn seeded_pseudo_random_systems_replay_exactly_and_diverge_with_other_seeds() {
    fn draws(seed: u64) -> Vec<i64> {
        let mut app = app();
        app.world_mut().insert_resource(Dice(Rng::new(seed)));
        app.world_mut().init_resource::<Seen>();
        update(&mut app).add_systems(|mut d: ResMut<Dice>, mut s: ResMut<Seen>| {
            s.0.push(d.0.below(1_000_000) as i64);
        });
        app.run_fixed(50, DT).unwrap();
        app.world().resource::<Seen>().0.clone()
    }
    assert_eq!(draws(8), draws(8));
    assert_ne!(draws(8), draws(9));
    let mut unique = draws(8);
    unique.sort();
    unique.dedup();
    assert!(unique.len() > 45, "splitmix output should rarely repeat");
}

#[test]
fn registration_order_of_conflict_free_systems_does_not_change_results() {
    let a = run(21, 60, Exec::Multi, false, 18);
    let b = run(21, 60, Exec::Multi, true, 18);
    assert_eq!(a, b);
}

#[test]
fn energy_transfers_conserve_the_total_regardless_of_executor() {
    for exec in [Exec::Single, Exec::Multi, Exec::Default] {
        let mut app = app();
        match exec {
            Exec::Single => {
                update(&mut app).set_executor(SingleThreadedExecutor::new());
            }
            Exec::Multi => {
                update(&mut app).set_executor(MultiThreadedExecutor::new());
            }
            Exec::Default => {}
        }
        let ids: Vec<Entity> = (0..16)
            .map(|_| app.world_mut().spawn(Energy(1000)).id())
            .collect();
        app.world_mut().insert_resource(Dice(Rng::new(2718)));
        app.world_mut().insert_resource(Roster(ids.clone()));
        update(&mut app).add_systems(
            |mut dice: ResMut<Dice>, roster: Res<Roster>, mut q: Query<&mut Energy>| {
                for _ in 0..8 {
                    let a = roster.0[dice.0.below(16) as usize];
                    let b = roster.0[dice.0.below(16) as usize];
                    if a == b {
                        continue;
                    }
                    let amount = (dice.0.below(300) as i64).min(q.get(a).unwrap().0);
                    q.get_mut(a).unwrap().0 -= amount;
                    q.get_mut(b).unwrap().0 += amount;
                }
            },
        );
        app.run_fixed(100, DT).unwrap();
        let total: i64 = app
            .world_mut()
            .query::<&Energy>()
            .iter(app.world())
            .map(|e| e.0)
            .sum();
        assert_eq!(total, 16_000, "{exec:?}");
    }
}

#[derive(Resource)]
struct Roster(Vec<Entity>);
