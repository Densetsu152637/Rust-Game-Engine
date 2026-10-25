//! Exact-number tests of the scheduler: transitions, budgets, hysteresis, eviction, layers.
use crate::*;

/// Cells of 100 m in a row along +x: cell `i` spans x in `[100 i, 100 i + 100)`.
fn row(n: i32, limits: Limits) -> Streamer {
    let mut s = Streamer::new(Grid::new(100.0, GridPlane::Xz).unwrap(), limits);
    for i in 0..n {
        s.add_cell(id(i), 10).unwrap();
    }
    s
}

fn id(i: i32) -> CellId {
    CellId {
        key: CellKey([i, 0, 0]),
        layer: Layer::DEFAULT,
    }
}

fn source(x: f64) -> Source {
    Source {
        position: [x, 0.0, 50.0],
        priority: 0,
        load_radius: 150.0,
        unload_radius: 250.0,
    }
}

fn limits(cells: usize, max_loaded: usize) -> Limits {
    Limits {
        cells_per_tick: cells,
        bytes_per_tick: u64::MAX,
        max_in_flight: 64,
        max_loaded,
    }
}

/// Finishes every transition a plan started, like a host with instant I/O.
fn settle(s: &mut Streamer, plan: &Plan) {
    for c in &plan.start_loads {
        s.complete_load(*c).unwrap();
    }
    for c in &plan.start_unloads {
        s.complete_unload(*c).unwrap();
    }
}

fn keys(ids: &[CellId]) -> Vec<i32> {
    ids.iter().map(|c| c.key.0[0]).collect()
}

fn loaded(s: &Streamer) -> Vec<i32> {
    s.cells()
        .filter(|(_, st)| *st == CellState::Loaded)
        .map(|(c, _)| c.key.0[0])
        .collect()
}

#[test]
fn cells_in_the_load_radius_load_in_distance_order_under_the_cell_budget() {
    // Source at x = 50: cell 0 d=0, cell 1 d=50, cell 2 d=150 (inclusive), cell 3 d=250.
    let mut s = row(6, limits(2, 64));
    let sources = [source(50.0)];
    let first = s.plan(&sources);
    assert_eq!(keys(&first.start_loads), [0, 1]);
    assert_eq!(first.bytes, 20);
    assert_eq!(s.state(id(0)), Some(CellState::Loading));
    assert_eq!(s.state(id(2)), Some(CellState::Unloaded));
    settle(&mut s, &first);
    let second = s.plan(&sources);
    assert_eq!(keys(&second.start_loads), [2]);
    settle(&mut s, &second);
    let third = s.plan(&sources);
    assert_eq!(third, Plan::default(), "cell 3 is outside the load radius");
    assert_eq!(loaded(&s), [0, 1, 2]);
    let c = s.counts();
    assert_eq!((c.loaded, c.unloaded, c.loading, c.unloading), (3, 3, 0, 0));
}

#[test]
fn band_cells_stay_loaded_until_beyond_the_unload_radius() {
    let mut s = row(10, limits(64, 64));
    let plan = s.plan(&[source(50.0)]);
    settle(&mut s, &plan);
    // x = 300: cell 0 (0..100) is 200 away: inside the band, retained, not reloaded either.
    let plan = s.plan(&[source(300.0)]);
    assert!(plan.start_unloads.is_empty(), "{plan:?}");
    assert_eq!(
        keys(&plan.start_loads),
        [3, 4],
        "cells within 150: 3 (d 0), 4 (d 100), 2 loaded"
    );
    settle(&mut s, &plan);
    assert_eq!(s.state(id(0)), Some(CellState::Loaded));
    // x = 400: cell 0 is 300 away (> 250): unloads; cell 1 (100..200) is 200 away: stays.
    let plan = s.plan(&[source(400.0)]);
    assert_eq!(keys(&plan.start_unloads), [0]);
    assert_eq!(s.state(id(0)), Some(CellState::Unloading));
    settle(&mut s, &plan);
    assert_eq!(s.state(id(0)), Some(CellState::Unloaded));
    assert_eq!(s.state(id(1)), Some(CellState::Loaded));
}

fn transitions(unload_radius: f64) -> usize {
    let mut s = row(10, limits(64, 64));
    let mut count = 0;
    for tick in 0..100 {
        // Jitter across the load boundary of cell 3 (x = 300): d = 160 / 140 vs radius 150.
        let x = if tick % 2 == 0 { 140.0 } else { 160.0 };
        let mut src = source(x);
        src.unload_radius = unload_radius;
        let plan = s.plan(&[src]);
        count += plan.start_loads.len() + plan.start_unloads.len();
        settle(&mut s, &plan);
    }
    count
}

#[test]
fn hysteresis_prevents_thrashing_at_the_boundary() {
    // With unload == load the boundary cells flip every other tick; with the band they never do.
    let without = transitions(150.0);
    let with = transitions(250.0);
    assert!(without >= 100, "control must thrash, got {without}");
    // Exactly the five cells within 150 m of x in 140..160 load once and stay: cells 0..=3
    // (d = 0, 0, 0, 140/160 -> loaded when 140) plus cell 4 (d 240/260, never).
    assert_eq!(with, 4, "each cell loads once and nothing unloads");
}

#[test]
fn higher_priority_source_is_served_first_even_when_farther() {
    let mut s = row(20, limits(1, 64));
    let near = source(50.0);
    let mut urgent = source(1450.0);
    urgent.priority = 5;
    let sources = [near, urgent];
    let order: Vec<i32> = (0..3)
        .flat_map(|_| {
            let plan = s.plan(&sources);
            settle(&mut s, &plan);
            keys(&plan.start_loads)
        })
        .collect();
    // Priority 5 first (its own cell, d = 0), then its neighbours by distance.
    assert_eq!(order, [14, 13, 15]);
    let rank = s.rank(id(0)).unwrap();
    assert_eq!(rank.priority(), 0);
}

#[test]
fn byte_budget_limits_admissions_but_never_starves_a_large_cell() {
    let mut s = Streamer::new(
        Grid::new(100.0, GridPlane::Xz).unwrap(),
        Limits {
            cells_per_tick: 10,
            bytes_per_tick: 25,
            max_in_flight: 64,
            max_loaded: 64,
        },
    );
    for (i, bytes) in [(0, 10), (1, 10), (2, 10), (3, 100)] {
        s.add_cell(id(i), bytes).unwrap();
    }
    let sources = [source(50.0)];
    let p1 = s.plan(&sources);
    assert_eq!(keys(&p1.start_loads), [0, 1]);
    assert_eq!(p1.bytes, 20, "third 10-byte cell would reach 30 > 25");
    settle(&mut s, &p1);
    let p2 = s.plan(&sources);
    assert_eq!(keys(&p2.start_loads), [2]);
    settle(&mut s, &p2);
    // Cell 3 (d = 250) joins when the source moves; it alone exceeds the budget but is admitted.
    let p3 = s.plan(&[source(150.0)]);
    assert_eq!(keys(&p3.start_loads), [3]);
    assert_eq!(p3.bytes, 100);
}

#[test]
fn in_flight_cap_bounds_concurrent_loads() {
    let mut s = row(
        10,
        Limits {
            max_in_flight: 2,
            ..limits(10, 64)
        },
    );
    let sources = [source(50.0)];
    let p1 = s.plan(&sources);
    assert_eq!(keys(&p1.start_loads), [0, 1]);
    assert!(s.plan(&sources).start_loads.is_empty(), "two still loading");
    s.complete_load(id(0)).unwrap();
    let p3 = s.plan(&sources);
    assert_eq!(keys(&p3.start_loads), [2]);
    assert_eq!(s.counts().loading, 2);
}

#[test]
fn memory_budget_evicts_the_farthest_cell_for_a_closer_one() {
    let mut s = row(10, limits(64, 2));
    let first = s.plan(&[source(50.0)]);
    assert_eq!(keys(&first.start_loads), [0, 1], "cell 2 does not fit");
    settle(&mut s, &first);
    assert!(s.plan(&[source(50.0)]).start_loads.is_empty());
    // Source at x = 250: ranks d(0)=150, d(1)=50, d(2)=0, d(3)=50, d(4)=150.
    let plan = s.plan(&[source(250.0)]);
    assert_eq!(keys(&plan.start_loads), [2]);
    assert_eq!(
        keys(&plan.start_unloads),
        [0],
        "the farthest loaded cell is evicted"
    );
    settle(&mut s, &plan);
    let again = s.plan(&[source(250.0)]);
    assert_eq!(
        again,
        Plan::default(),
        "cell 3 ties cell 1 and does not evict it; occupancy is full"
    );
    let c = s.counts();
    assert_eq!((c.loaded, c.loading), (2, 0));
    assert_eq!(loaded(&s), [1, 2]);
}

#[test]
fn shrinking_the_memory_budget_evicts_the_farthest_first() {
    let mut s = row(10, limits(64, 64));
    let plan = s.plan(&[source(50.0)]);
    settle(&mut s, &plan);
    assert_eq!(loaded(&s), [0, 1, 2]);
    let mut l = s.limits();
    l.max_loaded = 1;
    s.set_limits(l);
    let plan = s.plan(&[source(50.0)]);
    assert_eq!(keys(&plan.start_unloads), [2, 1], "d = 150 then d = 50");
    settle(&mut s, &plan);
    assert_eq!(loaded(&s), [0]);
}

#[test]
fn disabled_layer_unloads_and_reenabling_reloads() {
    let mut s = Streamer::new(Grid::new(100.0, GridPlane::Xz).unwrap(), limits(64, 64));
    let props = s.layer("props").unwrap();
    assert_eq!(s.layer("props").unwrap(), props, "interned");
    assert_eq!(s.layer_name(props), "props");
    for i in 0..2 {
        s.add_cell(id(i), 10).unwrap();
        s.add_cell(
            CellId {
                key: CellKey([i, 0, 0]),
                layer: props,
            },
            10,
        )
        .unwrap();
    }
    let sources = [source(50.0)];
    let plan = s.plan(&sources);
    assert_eq!(plan.start_loads.len(), 4);
    settle(&mut s, &plan);
    s.set_layer_enabled(props, false);
    let plan = s.plan(&sources);
    assert_eq!(plan.start_unloads.len(), 2);
    assert!(plan.start_unloads.iter().all(|c| c.layer == props));
    settle(&mut s, &plan);
    assert_eq!(loaded(&s), [0, 1], "default layer untouched");
    assert!(
        s.plan(&sources).start_loads.is_empty(),
        "disabled layer stays unloaded"
    );
    s.set_layer_enabled(props, true);
    let plan = s.plan(&sources);
    assert_eq!(plan.start_loads.len(), 2);
}

#[test]
fn cells_leaving_range_while_loading_are_cancelled() {
    let mut s = row(10, limits(64, 64));
    let plan = s.plan(&[source(50.0)]);
    assert_eq!(plan.start_loads.len(), 3);
    let plan = s.plan(&[source(1000.0)]);
    assert_eq!(keys(&plan.cancel_loads), [0, 1, 2]);
    assert_eq!(s.state(id(0)), Some(CellState::Unloaded));
    assert!(s.complete_load(id(0)).is_err(), "result must be discarded");
}

#[test]
fn failed_loads_back_off_then_stop_until_reset() {
    let mut s = row(3, limits(64, 64));
    let sources = [source(50.0)];
    // Failure k on tick t delays the retry by min(2^k, 32) ticks.
    let p = s.plan(&sources); // tick 1
    assert_eq!(p.start_loads.len(), 3);
    for c in p.start_loads {
        if c == id(0) {
            s.fail_load(c).unwrap();
        } else {
            s.complete_load(c).unwrap();
        }
    }
    assert!(s.plan(&sources).start_loads.is_empty()); // tick 2 (retry_at = 1 + 2 = 3)
    assert_eq!(keys(&s.plan(&sources).start_loads), [0]); // tick 3
    s.fail_load(id(0)).unwrap(); // retry_at = 3 + 4 = 7
    for _ in 4..7 {
        assert!(s.plan(&sources).start_loads.is_empty());
    }
    assert_eq!(keys(&s.plan(&sources).start_loads), [0]); // tick 7
    s.fail_load(id(0)).unwrap();
    assert_eq!(s.failures(id(0)), 3);
    for _ in 0..40 {
        assert!(
            s.plan(&sources).start_loads.is_empty(),
            "gave up after 3 failures"
        );
    }
    s.reset_failures(id(0)).unwrap();
    assert_eq!(keys(&s.plan(&sources).start_loads), [0]);
}

#[test]
fn pinned_cells_load_anywhere_and_survive_eviction() {
    let mut s = row(20, limits(64, 2));
    s.pin(id(15)).unwrap();
    let sources = [source(50.0)];
    let p = s.plan(&sources);
    assert_eq!(
        keys(&p.start_loads),
        [15, 0],
        "pinned first; capacity 2 admits one more"
    );
    settle(&mut s, &p);
    let p = s.plan(&sources);
    assert!(
        p.start_loads.is_empty() && p.start_unloads.is_empty(),
        "{p:?}"
    );
    s.unpin(id(15)).unwrap();
    let p = s.plan(&sources);
    assert_eq!(keys(&p.start_unloads), [15]);
}

#[test]
fn works_more_than_ten_kilometres_from_the_origin() {
    // 100 m cells; source at x = 12_345.6 m is in cell 123.
    let mut s = Streamer::new(Grid::new(100.0, GridPlane::Xz).unwrap(), limits(64, 64));
    for k in 118..=128 {
        s.add_cell(id(k), 10).unwrap();
    }
    let mut src = source(12_345.6);
    src.position[2] = -23_456.0;
    let plan = s.plan(&[src]);
    // The source is 23 km away from the row's z span (cells span z in [0, 100)): nothing loads.
    assert!(plan.start_loads.is_empty());
    src.position[2] = 50.0;
    let plan = s.plan(&[src]);
    // x = 12_345.6: cell 123 (d 0), 122 (d 45.6), 124 (d 54.4), 121 (145.6 ok), 125 (154.4 no).
    assert_eq!(keys(&plan.start_loads), [123, 122, 124, 121]);
}

#[test]
fn identical_inputs_give_identical_plans() {
    let run = || {
        let mut s = row(30, limits(3, 7));
        let mut log = Vec::new();
        for tick in 0..60 {
            let x = 50.0 + f64::from(tick) * 37.0;
            let plan = s.plan(&[
                source(x),
                Source {
                    priority: 2,
                    ..source(2900.0 - x)
                },
            ]);
            log.push(plan.clone());
            settle(&mut s, &plan);
        }
        log
    };
    assert_eq!(run(), run());
}

#[test]
fn misuse_of_transitions_is_an_error() {
    let mut s = row(2, limits(64, 64));
    assert!(s.complete_load(id(0)).is_err());
    assert!(s.complete_unload(id(0)).is_err());
    assert!(s.fail_load(id(9)).is_err());
    assert!(s.add_cell(id(0), 1).is_err());
    s.force_begin_load(id(0)).unwrap();
    assert!(s.force_begin_load(id(0)).is_err());
    s.complete_load(id(0)).unwrap();
    s.force_begin_unload(id(0)).unwrap();
    s.complete_unload(id(0)).unwrap();
    assert_eq!(s.state(id(0)), Some(CellState::Unloaded));
}
