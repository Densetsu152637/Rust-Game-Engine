//! Buffered `Current`/`Next` values: old/new visibility and publication.
mod common;

use common::*;
use engine_core::prelude::*;

fn buffered_app() -> EngineApp {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    app
}

fn seen(app: &EngineApp) -> Vec<i64> {
    app.world().resource::<Seen>().0.clone()
}

fn set_next(mut q: Query<&mut Next<i32>>) {
    for mut n in &mut q {
        n.0 += 1;
    }
}

fn read_current(q: Query<&Current<i32>>, mut s: ResMut<Seen>) {
    s.0.extend(q.iter().map(|c| i64::from(c.0)));
}

#[test]
fn readers_before_publication_see_the_old_value_and_after_see_the_new() {
    let mut app = buffered_app();
    app.world_mut().spawn((Current(0_i32), Next(0_i32)));
    update(&mut app).add_systems(
        (
            set_next,
            read_current,
            publish_buffered::<i32>,
            read_current,
        )
            .chain(),
    );
    app.run_fixed(3, DT).unwrap();
    assert_eq!(seen(&app), [0, 1, 1, 2, 2, 3]);
}

#[test]
fn current_is_unchanged_without_a_publish_system() {
    let mut app = buffered_app();
    let e = app.world_mut().spawn((Current(5_i32), Next(5_i32))).id();
    update(&mut app).add_systems(set_next);
    app.run_fixed(4, DT).unwrap();
    assert_eq!(app.world().get::<Current<i32>>(e), Some(&Current(5)));
    assert_eq!(app.world().get::<Next<i32>>(e), Some(&Next(9)));
}

#[test]
fn writers_of_next_and_readers_of_current_need_no_ordering() {
    let mut app = buffered_app();
    app.world_mut().spawn((Current(1_i32), Next(1_i32)));
    update(&mut app).add_systems((set_next, read_current));
    app.tick(DT).unwrap();
    assert_eq!(seen(&app), [1]);
}

#[test]
fn publish_unordered_with_a_current_reader_is_a_rejected_conflict() {
    let mut app = buffered_app();
    app.world_mut().spawn((Current(1_i32), Next(2_i32)));
    update(&mut app).add_systems((publish_buffered::<i32>, read_current));
    assert!(app.tick(DT).is_err());
}

#[test]
fn publish_unordered_with_a_next_writer_is_a_rejected_conflict() {
    let mut app = buffered_app();
    app.world_mut().spawn((Current(1_i32), Next(2_i32)));
    update(&mut app).add_systems((publish_buffered::<i32>, set_next));
    assert!(app.tick(DT).is_err());
}

#[test]
fn publish_only_flags_current_as_changed_when_the_value_differs() {
    let mut app = buffered_app();
    app.world_mut().spawn((Current(1_i32), Next(1_i32)));
    update(&mut app).add_systems(
        (
            |mut q: Query<&mut Next<i32>>, t: Res<Time>| {
                if t.tick == 3 {
                    for mut n in &mut q {
                        n.0 = 9;
                    }
                }
            },
            publish_buffered::<i32>,
            |q: Query<(), Changed<Current<i32>>>, mut s: ResMut<Seen>| {
                s.0.push(q.iter().count() as i64)
            },
        )
            .chain(),
    );
    app.run_fixed(5, DT).unwrap();
    assert_eq!(seen(&app), [1, 0, 1, 0, 0]);
}

#[test]
fn next_modified_and_reverted_before_publication_never_reaches_current() {
    let mut app = buffered_app();
    app.world_mut().spawn((Current(4_i32), Next(4_i32)));
    update(&mut app).add_systems(
        (
            |mut q: Query<&mut Next<i32>>| {
                for mut n in &mut q {
                    n.0 = 100;
                    n.0 = 4;
                }
            },
            publish_buffered::<i32>,
            |q: Query<(), Changed<Current<i32>>>, mut s: ResMut<Seen>| {
                s.0.push(q.iter().count() as i64)
            },
        )
            .chain(),
    );
    app.run_fixed(2, DT).unwrap();
    assert_eq!(seen(&app), [1, 0]); // only the initial spawn counts as a change
}

#[test]
fn publishing_twice_in_one_tick_is_the_same_as_once() {
    let mut once = buffered_app();
    let mut twice = buffered_app();
    for app in [&mut once, &mut twice] {
        app.world_mut().spawn((Current(0_i32), Next(0_i32)));
    }
    update(&mut once).add_systems((set_next, publish_buffered::<i32>).chain());
    update(&mut twice)
        .add_systems((set_next, publish_buffered::<i32>, publish_buffered::<i32>).chain());
    once.run_fixed(5, DT).unwrap();
    twice.run_fixed(5, DT).unwrap();
    let get = |app: &mut EngineApp| {
        app.world_mut()
            .query::<&Current<i32>>()
            .single(app.world())
            .unwrap()
            .0
    };
    assert_eq!(get(&mut once), get(&mut twice));
    assert_eq!(get(&mut once), 5);
}

#[test]
fn entities_without_a_next_partner_are_not_touched_by_publication() {
    let mut app = buffered_app();
    let lonely = app.world_mut().spawn(Current(7_i32)).id();
    let pending = app.world_mut().spawn(Next(8_i32)).id();
    update(&mut app).add_systems(publish_buffered::<i32>);
    app.run_fixed(2, DT).unwrap();
    assert_eq!(app.world().get::<Current<i32>>(lonely), Some(&Current(7)));
    assert!(app.world().get::<Current<i32>>(pending).is_none());
}

#[test]
fn removing_next_leaves_current_and_stops_publication() {
    let mut app = buffered_app();
    let e = app.world_mut().spawn((Current(1_i32), Next(2_i32))).id();
    update(&mut app).add_systems(publish_buffered::<i32>);
    app.tick(DT).unwrap();
    app.world_mut().entity_mut(e).remove::<Next<i32>>();
    app.world_mut().entity_mut(e).insert(Current(50_i32));
    app.tick(DT).unwrap();
    assert_eq!(app.world().get::<Current<i32>>(e), Some(&Current(50)));
}

#[test]
fn different_value_types_publish_independently() {
    let mut app = buffered_app();
    let e = app
        .world_mut()
        .spawn((
            Current(1_i32),
            Next(2_i32),
            Current("a".to_string()),
            Next("b".to_string()),
        ))
        .id();
    update(&mut app).add_systems(publish_buffered::<i32>);
    app.tick(DT).unwrap();
    assert_eq!(app.world().get::<Current<i32>>(e), Some(&Current(2)));
    assert_eq!(
        app.world().get::<Current<String>>(e),
        Some(&Current("a".to_string()))
    );
    update(&mut app).add_systems(publish_buffered::<String>);
    app.tick(DT).unwrap();
    assert_eq!(
        app.world().get::<Current<String>>(e),
        Some(&Current("b".to_string()))
    );
}

#[derive(Clone, Debug)]
struct Approx(f32, u32);
impl PartialEq for Approx {
    fn eq(&self, other: &Self) -> bool {
        (self.0 - other.0).abs() < 0.5
    }
}

#[test]
fn publication_uses_the_value_types_own_equality() {
    let mut app = buffered_app();
    let e = app
        .world_mut()
        .spawn((Current(Approx(1.0, 0)), Next(Approx(1.25, 1))))
        .id();
    update(&mut app).add_systems(publish_buffered::<Approx>);
    app.tick(DT).unwrap();
    assert_eq!(
        app.world().get::<Current<Approx>>(e).unwrap().0.1,
        0,
        "equal values are not copied"
    );
    app.world_mut().get_mut::<Next<Approx>>(e).unwrap().0 = Approx(5.0, 2);
    app.tick(DT).unwrap();
    assert_eq!(app.world().get::<Current<Approx>>(e).unwrap().0.1, 2);
}

/// Reference implementation of a 1D cellular automaton (rule 110) on a ring.
fn reference_rule110(cells: &[bool], generations: usize) -> Vec<bool> {
    let mut cur = cells.to_vec();
    for _ in 0..generations {
        let n = cur.len();
        cur = (0..n)
            .map(|i| {
                let l = cur[(i + n - 1) % n];
                let c = cur[i];
                let r = cur[(i + 1) % n];
                matches!(
                    (l, c, r),
                    (true, true, false)
                        | (true, false, true)
                        | (false, true, true)
                        | (false, true, false)
                        | (false, false, true)
                )
            })
            .collect();
    }
    cur
}

#[derive(Component)]
struct Cell(usize);

fn automaton_matches_reference(seed: u64, width: usize, generations: usize) {
    let mut rng = Rng::new(seed);
    let initial: Vec<bool> = (0..width).map(|_| rng.chance(1, 2)).collect();
    let mut app = app();
    let ids: Vec<Entity> = initial
        .iter()
        .enumerate()
        .map(|(i, v)| app.world_mut().spawn((Cell(i), Current(*v), Next(*v))).id())
        .collect();
    update(&mut app).add_systems(
        (
            // Every cell reads neighbours' Current while writing only its own Next.
            |mut cells: Query<(&Cell, &mut Next<bool>)>,
             current: Query<&Current<bool>>,
             ids: Res<Ring>| {
                let n = ids.0.len();
                for (cell, mut next) in &mut cells {
                    let i = cell.0;
                    let l = current.get(ids.0[(i + n - 1) % n]).unwrap().0;
                    let c = current.get(ids.0[i]).unwrap().0;
                    let r = current.get(ids.0[(i + 1) % n]).unwrap().0;
                    next.0 = matches!(
                        (l, c, r),
                        (true, true, false)
                            | (true, false, true)
                            | (false, true, true)
                            | (false, true, false)
                            | (false, false, true)
                    );
                }
            },
            publish_buffered::<bool>,
        )
            .chain(),
    );
    app.world_mut().insert_resource(Ring(ids.clone()));
    app.run_fixed(generations as u64, DT).unwrap();
    let actual: Vec<bool> = ids
        .iter()
        .map(|e| app.world().get::<Current<bool>>(*e).unwrap().0)
        .collect();
    assert_eq!(actual, reference_rule110(&initial, generations));
}

#[derive(Resource)]
struct Ring(Vec<Entity>);

macro_rules! automaton_cases {
    ($($name:ident: $seed:expr, $width:expr, $gens:expr;)*) => {
        $( #[test] fn $name() { automaton_matches_reference($seed, $width, $gens); } )*
    };
}

automaton_cases! {
    double_buffered_automaton_matches_reference_narrow_ring: 1, 5, 20;
    double_buffered_automaton_matches_reference_wide_ring: 2, 64, 40;
    double_buffered_automaton_matches_reference_odd_width: 3, 31, 33;
    double_buffered_automaton_matches_reference_single_generation: 4, 17, 1;
    double_buffered_automaton_matches_reference_long_run: 5, 24, 120;
}

#[test]
fn diffusion_through_buffers_conserves_total_mass_every_generation() {
    #[derive(Component)]
    struct Slot(usize);
    let mut app = buffered_app();
    let n = 12;
    let mut rng = Rng::new(31);
    let ids: Vec<Entity> = (0..n)
        .map(|i| {
            let m = rng.below(1000) as i32;
            app.world_mut().spawn((Slot(i), Current(m), Next(m))).id()
        })
        .collect();
    let total: i32 = ids
        .iter()
        .map(|e| app.world().get::<Current<i32>>(*e).unwrap().0)
        .sum();
    app.world_mut().insert_resource(Ring(ids.clone()));
    update(&mut app).add_systems(
        (
            |mut q: Query<(&Slot, &mut Next<i32>)>, cur: Query<&Current<i32>>, ring: Res<Ring>| {
                let n = ring.0.len();
                for (slot, mut next) in &mut q {
                    let i = slot.0;
                    let l = cur.get(ring.0[(i + n - 1) % n]).unwrap().0;
                    let r = cur.get(ring.0[(i + 1) % n]).unwrap().0;
                    let c = cur.get(ring.0[i]).unwrap().0;
                    // Each cell sends a quarter to each neighbour and keeps the rest.
                    next.0 = c - c / 4 - c / 4 + l / 4 + r / 4;
                }
            },
            publish_buffered::<i32>,
            |cur: Query<&Current<i32>>, mut s: ResMut<Seen>| {
                s.0.push(cur.iter().map(|c| i64::from(c.0)).sum())
            },
        )
            .chain(),
    );
    app.run_fixed(30, DT).unwrap();
    // Integer division loses remainders identically on send and receive, so the
    // sum is conserved only if reads are consistent snapshots.
    let sums = seen(&app);
    assert_eq!(sums.len(), 30);
    assert!(sums.iter().all(|s| *s == i64::from(total)), "{sums:?}");
}
