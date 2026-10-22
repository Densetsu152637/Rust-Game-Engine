//! Swept AABB collision, masks, deterministic contacts, triggers, safe-position
//! search, follow steering and flock targets, including multi-step simulations.
use super::common::*;
use engine_project::gameplay2d::{
    Body, TriggerEvent, TriggerPhase, World, flock_target, follow_velocity,
};

fn at(x: f32, y: f32) -> Body {
    Body {
        position: [x, y],
        ..Default::default()
    }
}

fn boxed(x: f32, y: f32, w: f32, h: f32) -> Body {
    Body {
        position: [x, y],
        size: [w, h],
        ..Default::default()
    }
}

fn trig(x: f32, y: f32) -> Body {
    Body {
        trigger: true,
        ..at(x, y)
    }
}

fn world(items: &[(&str, Body)]) -> World {
    let mut w = World::default();
    for (id, body) in items {
        w.upsert(*id, *body).unwrap();
    }
    w
}

fn pos(w: &World, id: &str) -> [f32; 2] {
    w.body(id).unwrap().position
}

fn event(first: &str, second: &str, phase: TriggerPhase) -> TriggerEvent {
    TriggerEvent {
        first: first.into(),
        second: second.into(),
        phase,
    }
}

// ---- body validation -----------------------------------------------------------

fn rejected(id: &str, body: Body) {
    let mut w = World::default();
    expect_code(w.upsert(id, body), "collision.body.invalid");
    assert!(w.body(id).is_none());
}

table! { rejected;
    body_with_empty_id_is_rejected => ("", at(0.0, 0.0));
    body_with_id_over_256_bytes_is_rejected => (&"x".repeat(257), at(0.0, 0.0));
    body_with_zero_width_is_rejected => ("b", boxed(0.0, 0.0, 0.0, 1.0));
    body_with_zero_height_is_rejected => ("b", boxed(0.0, 0.0, 1.0, 0.0));
    body_with_negative_width_is_rejected => ("b", boxed(0.0, 0.0, -1.0, 1.0));
    body_with_nan_size_is_rejected => ("b", boxed(0.0, 0.0, f32::NAN, 1.0));
    body_with_infinite_size_is_rejected => ("b", boxed(0.0, 0.0, 1.0, f32::INFINITY));
    body_with_size_above_a_million_is_rejected => ("b", boxed(0.0, 0.0, 1_000_001.0, 1.0));
    body_with_nan_position_is_rejected => ("b", at(f32::NAN, 0.0));
    body_with_infinite_position_is_rejected => ("b", at(0.0, f32::NEG_INFINITY));
    body_with_position_beyond_a_million_is_rejected => ("b", at(1_000_001.0, 0.0));
    body_with_offset_beyond_a_million_is_rejected => ("b", Body { offset: [0.0, -1_000_001.0], ..at(0.0, 0.0) });
    body_with_nan_offset_is_rejected => ("b", Body { offset: [f32::NAN, 0.0], ..at(0.0, 0.0) });
    body_with_zero_layer_is_rejected => ("b", Body { layer: 0, ..at(0.0, 0.0) });
    body_with_zero_mask_is_rejected => ("b", Body { mask: 0, ..at(0.0, 0.0) });
}

#[test]
fn body_extremes_inside_the_limits_are_accepted() {
    let mut w = World::default();
    w.upsert(
        "x".repeat(256),
        boxed(1_000_000.0, -1_000_000.0, 1_000_000.0, 1.0),
    )
    .unwrap();
    assert_eq!(w.ids().count(), 1);
}

#[test]
fn upsert_replaces_an_existing_body_in_place() {
    let mut w = world(&[("a", at(0.0, 0.0))]);
    w.upsert("a", at(5.0, 6.0)).unwrap();
    assert_eq!(pos(&w, "a"), [5.0, 6.0]);
    assert_eq!(w.ids().count(), 1);
}

#[test]
fn rejected_upsert_leaves_the_previous_body_untouched() {
    let mut w = world(&[("a", at(1.0, 2.0))]);
    assert!(w.upsert("a", at(f32::NAN, 0.0)).is_err());
    assert_eq!(pos(&w, "a"), [1.0, 2.0]);
}

#[test]
fn body_capacity_is_1024_and_updates_remain_possible() {
    let mut w = World::default();
    for i in 0..World::MAXIMUM_BODIES {
        w.upsert(format!("b{i}"), at(i as f32 * 2.0, 0.0)).unwrap();
    }
    expect_code(w.upsert("overflow", at(0.0, 5.0)), "collision.capacity");
    w.upsert("b0", at(0.0, 9.0)).unwrap();
    assert!(w.remove("b1"));
    w.upsert("overflow", at(0.0, 5.0)).unwrap();
    assert_eq!(w.ids().count(), World::MAXIMUM_BODIES);
}

#[test]
fn remove_reports_whether_a_body_existed() {
    let mut w = world(&[("a", at(0.0, 0.0))]);
    assert!(w.remove("a"));
    assert!(!w.remove("a"));
    assert!(w.body("a").is_none());
}

#[test]
fn ids_are_listed_in_sorted_order_regardless_of_insertion() {
    let w = world(&[
        ("m", at(0.0, 0.0)),
        ("a", at(5.0, 0.0)),
        ("z", at(9.0, 0.0)),
    ]);
    assert_eq!(w.ids().collect::<Vec<_>>(), vec!["a", "m", "z"]);
}

#[test]
fn clear_removes_every_body() {
    let mut w = world(&[("a", at(0.0, 0.0)), ("b", at(5.0, 0.0))]);
    w.clear();
    assert_eq!(w.ids().count(), 0);
}

// ---- queries and safety ---------------------------------------------------------

#[test]
fn query_returns_overlapping_bodies_sorted_by_id() {
    let w = world(&[
        ("zeta", at(0.2, 0.0)),
        ("alpha", at(-0.2, 0.0)),
        ("far", at(9.0, 0.0)),
    ]);
    assert_eq!(w.query(at(0.0, 0.0), "").unwrap(), vec!["alpha", "zeta"]);
}

#[test]
fn query_ignores_the_named_body() {
    let w = world(&[("me", at(0.0, 0.0)), ("other", at(0.5, 0.0))]);
    assert_eq!(w.query(at(0.0, 0.0), "me").unwrap(), vec!["other"]);
}

#[test]
fn query_does_not_report_touching_edges() {
    let w = world(&[("edge", at(1.0, 0.0))]);
    assert!(w.query(at(0.0, 0.0), "").unwrap().is_empty());
}

#[test]
fn query_does_not_report_corner_contact() {
    let w = world(&[("corner", at(1.0, 1.0))]);
    assert!(w.query(at(0.0, 0.0), "").unwrap().is_empty());
}

#[test]
fn query_includes_trigger_bodies() {
    let w = world(&[("zone", trig(0.0, 0.0))]);
    assert_eq!(w.query(at(0.0, 0.0), "").unwrap(), vec!["zone"]);
}

#[test]
fn query_respects_the_area_body_mask() {
    let w = world(&[
        (
            "a",
            Body {
                layer: 2,
                ..at(0.0, 0.0)
            },
        ),
        (
            "b",
            Body {
                layer: 4,
                ..at(0.0, 0.0)
            },
        ),
    ]);
    let area = Body {
        mask: 4,
        ..at(0.0, 0.0)
    };
    assert_eq!(w.query(area, "").unwrap(), vec!["b"]);
}

#[test]
fn query_requires_both_mask_directions_to_agree() {
    let w = world(&[(
        "picky",
        Body {
            mask: 8,
            ..at(0.0, 0.0)
        },
    )]);
    assert!(
        w.query(
            Body {
                layer: 1,
                ..at(0.0, 0.0)
            },
            ""
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        w.query(
            Body {
                layer: 8,
                ..at(0.0, 0.0)
            },
            ""
        )
        .unwrap(),
        vec!["picky"]
    );
}

#[test]
fn query_honours_collider_offsets() {
    let w = world(&[(
        "shifted",
        Body {
            offset: [5.0, 0.0],
            ..at(0.0, 0.0)
        },
    )]);
    assert!(w.query(at(0.0, 0.0), "").unwrap().is_empty());
    assert_eq!(w.query(at(5.0, 0.0), "").unwrap(), vec!["shifted"]);
}

#[test]
fn query_rejects_an_invalid_area() {
    let w = World::default();
    expect_code(
        w.query(boxed(0.0, 0.0, 0.0, 1.0), ""),
        "collision.query.invalid",
    );
}

#[test]
fn overlaps_of_unknown_body_is_an_error() {
    expect_code(World::default().overlaps("ghost"), "collision.body.unknown");
}

#[test]
fn overlaps_lists_neighbours_for_a_registered_body() {
    let w = world(&[
        ("a", at(0.0, 0.0)),
        ("b", at(0.9, 0.0)),
        ("c", at(-0.9, 0.9)),
    ]);
    assert_eq!(w.overlaps("a").unwrap(), vec!["b", "c"]);
}

#[test]
fn is_safe_ignores_trigger_bodies() {
    let w = world(&[("zone", trig(0.0, 0.0))]);
    assert!(w.is_safe(at(0.0, 0.0), "").unwrap());
}

#[test]
fn is_safe_detects_solid_overlap() {
    let w = world(&[("rock", at(0.0, 0.0))]);
    assert!(!w.is_safe(at(0.5, 0.0), "").unwrap());
}

#[test]
fn is_safe_allows_touching() {
    let w = world(&[("rock", at(0.0, 0.0))]);
    assert!(w.is_safe(at(1.0, 0.0), "").unwrap());
}

#[test]
fn is_safe_ignores_the_named_body() {
    let w = world(&[("me", at(0.0, 0.0))]);
    assert!(w.is_safe(at(0.0, 0.0), "me").unwrap());
}

#[test]
fn is_safe_skips_bodies_on_non_matching_layers() {
    let w = world(&[(
        "ghost",
        Body {
            layer: 2,
            ..at(0.0, 0.0)
        },
    )]);
    assert!(
        w.is_safe(
            Body {
                mask: 1,
                ..at(0.0, 0.0)
            },
            ""
        )
        .unwrap()
    );
}

#[test]
fn is_safe_rejects_an_invalid_candidate() {
    expect_code(
        World::default().is_safe(at(f32::NAN, 0.0), ""),
        "collision.body.invalid",
    );
}

// ---- swept movement -------------------------------------------------------------

fn run_into_wall(wall_at: [f32; 2], delta: [f32; 2], expected: [f32; 2]) {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("wall", at(wall_at[0], wall_at[1])),
    ]);
    let result = w.move_body("actor", delta).unwrap();
    assert_eq!(result.position, expected);
    assert_eq!(result.contacts, vec!["wall"]);
    assert_eq!(pos(&w, "actor"), expected);
}

table! { run_into_wall;
    moving_right_stops_flush_against_a_wall => ([3.0, 0.0], [8.0, 0.0], [2.0, 0.0]);
    moving_left_stops_flush_against_a_wall => ([-3.0, 0.0], [-8.0, 0.0], [-2.0, 0.0]);
    moving_up_stops_flush_against_a_wall => ([0.0, 3.0], [0.0, 8.0], [0.0, 2.0]);
    moving_down_stops_flush_against_a_wall => ([0.0, -3.0], [0.0, -8.0], [0.0, -2.0]);
    moving_towards_a_wall_exactly_to_contact_reports_it => ([3.0, 0.0], [2.0, 0.0], [2.0, 0.0]);
}

#[test]
fn free_movement_reaches_the_target_without_contacts() {
    let mut w = world(&[("actor", at(0.0, 0.0))]);
    let result = w.move_body("actor", [3.0, -2.0]).unwrap();
    assert_eq!(result.position, [3.0, -2.0]);
    assert!(result.contacts.is_empty());
}

#[test]
fn movement_stopping_short_of_a_far_wall_has_no_contact() {
    let mut w = world(&[("actor", at(0.0, 0.0)), ("wall", at(30.0, 0.0))]);
    let result = w.move_body("actor", [4.0, 0.0]).unwrap();
    assert_eq!(result.position, [4.0, 0.0]);
    assert!(result.contacts.is_empty());
}

#[test]
fn zero_displacement_is_a_noop() {
    let mut w = world(&[("actor", at(2.0, 3.0))]);
    let result = w.move_body("actor", [0.0, 0.0]).unwrap();
    assert_eq!(result.position, [2.0, 3.0]);
    assert!(result.contacts.is_empty());
}

#[test]
fn diagonal_motion_into_a_tall_wall_slides_along_it() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("wall", boxed(3.0, 0.0, 1.0, 10.0)),
    ]);
    let result = w.move_body("actor", [8.0, 4.0]).unwrap();
    assert_eq!(result.position, [2.0, 4.0]);
    assert_eq!(result.contacts, vec!["wall"]);
}

#[test]
fn diagonal_motion_into_a_wide_wall_slides_along_it() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("wall", boxed(0.0, 3.0, 10.0, 1.0)),
    ]);
    let result = w.move_body("actor", [4.0, 8.0]).unwrap();
    assert_eq!(result.position, [4.0, 2.0]);
    assert_eq!(result.contacts, vec!["wall"]);
}

#[test]
fn inner_corner_stops_both_axes() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("right", boxed(3.0, 0.0, 1.0, 10.0)),
        ("top", boxed(0.0, 3.0, 10.0, 1.0)),
    ]);
    let result = w.move_body("actor", [8.0, 8.0]).unwrap();
    assert_eq!(result.position, [2.0, 2.0]);
    assert_eq!(result.contacts, vec!["right", "top"]);
}

#[test]
fn corridor_exactly_as_wide_as_the_body_is_passable() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("up", at(3.0, 1.0)),
        ("down", at(3.0, -1.0)),
    ]);
    let result = w.move_body("actor", [8.0, 0.0]).unwrap();
    assert_eq!(result.position, [8.0, 0.0]);
    assert!(result.contacts.is_empty());
}

#[test]
fn corridor_narrower_than_the_body_blocks() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("up", boxed(3.0, 0.75, 1.0, 1.0)),
        ("down", boxed(3.0, -0.75, 1.0, 1.0)),
    ]);
    let result = w.move_body("actor", [8.0, 0.0]).unwrap();
    assert_eq!(result.position, [2.0, 0.0]);
    assert_eq!(result.contacts, vec!["down", "up"]);
}

#[test]
fn very_fast_motion_cannot_tunnel_through_a_thin_wall() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("thin", boxed(50.0, 0.0, 0.0625, 10.0)),
    ]);
    let result = w.move_body("actor", [100_000.0, 0.0]).unwrap();
    assert!((result.position[0] - 49.46875).abs() < 1e-3);
    assert_eq!(result.contacts, vec!["thin"]);
    assert!(w.overlaps("actor").unwrap().is_empty());
}

#[test]
fn contacts_are_sorted_and_independent_of_insertion_order() {
    let forward = world(&[
        ("actor", at(0.0, 0.0)),
        ("a", at(3.0, 0.0)),
        ("m", at(3.0, 0.0)),
        ("z", at(3.0, 0.0)),
    ]);
    let backward = world(&[
        ("z", at(3.0, 0.0)),
        ("m", at(3.0, 0.0)),
        ("a", at(3.0, 0.0)),
        ("actor", at(0.0, 0.0)),
    ]);
    let (mut f, mut b) = (forward, backward);
    let fr = f.move_body("actor", [8.0, 0.0]).unwrap();
    let br = b.move_body("actor", [8.0, 0.0]).unwrap();
    assert_eq!(fr, br);
    assert_eq!(fr.contacts, vec!["a", "m", "z"]);
}

#[test]
fn bodies_behind_the_direction_of_travel_are_not_contacts() {
    let mut w = world(&[("actor", at(0.0, 0.0)), ("behind", at(-1.0, 0.0))]);
    let result = w.move_body("actor", [4.0, 0.0]).unwrap();
    assert_eq!(result.position, [4.0, 0.0]);
    assert!(result.contacts.is_empty());
}

#[test]
fn a_body_already_touching_a_wall_can_move_away_freely() {
    let mut w = world(&[("actor", at(0.0, 0.0)), ("wall", at(1.0, 0.0))]);
    let result = w.move_body("actor", [-4.0, 0.0]).unwrap();
    assert_eq!(result.position, [-4.0, 0.0]);
    assert!(result.contacts.is_empty());
}

#[test]
fn a_body_resting_on_a_wall_cannot_push_into_it() {
    let mut w = world(&[("actor", at(0.0, 0.0)), ("wall", at(1.0, 0.0))]);
    let result = w.move_body("actor", [2.0, 0.0]).unwrap();
    assert_eq!(result.position, [0.0, 0.0]);
    assert_eq!(result.contacts, vec!["wall"]);
}

#[test]
fn initial_overlap_is_resolved_along_the_shallowest_axis() {
    let mut w = world(&[("actor", at(0.0, 0.0)), ("wall", at(0.25, 0.0))]);
    let result = w.move_body("actor", [0.0, 0.0]).unwrap();
    assert_eq!(result.position, [-0.75, 0.0]);
    assert_eq!(result.contacts, vec!["wall"]);
}

#[test]
fn identical_overlap_is_resolved_towards_the_left_deterministically() {
    let mut w = world(&[("actor", at(0.0, 0.0)), ("wall", at(0.0, 0.0))]);
    let result = w.move_body("actor", [0.0, 0.0]).unwrap();
    assert_eq!(result.position, [-1.0, 0.0]);
}

#[test]
fn vertical_overlap_is_pushed_out_vertically_when_shallower() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("floor", boxed(0.0, -0.75, 10.0, 1.0)),
    ]);
    let result = w.move_body("actor", [0.0, 0.0]).unwrap();
    assert_eq!(result.position, [0.0, 0.25]);
    assert_eq!(result.contacts, vec!["floor"]);
}

#[test]
fn trigger_bodies_move_through_solids_without_contacts() {
    let mut w = world(&[("ghost", trig(0.0, 0.0)), ("wall", at(3.0, 0.0))]);
    let result = w.move_body("ghost", [8.0, 0.0]).unwrap();
    assert_eq!(result.position, [8.0, 0.0]);
    assert!(result.contacts.is_empty());
}

#[test]
fn solid_bodies_pass_through_triggers() {
    let mut w = world(&[("actor", at(0.0, 0.0)), ("zone", trig(3.0, 0.0))]);
    let result = w.move_body("actor", [8.0, 0.0]).unwrap();
    assert_eq!(result.position, [8.0, 0.0]);
    assert!(result.contacts.is_empty());
}

#[test]
fn matching_layers_block_movement() {
    let mut w = world(&[
        (
            "actor",
            Body {
                mask: 2,
                ..at(0.0, 0.0)
            },
        ),
        (
            "wall",
            Body {
                layer: 2,
                ..at(3.0, 0.0)
            },
        ),
    ]);
    assert_eq!(
        w.move_body("actor", [8.0, 0.0]).unwrap().position,
        [2.0, 0.0]
    );
}

#[test]
fn bodies_outside_the_movers_mask_are_passed_through() {
    let mut w = world(&[
        (
            "actor",
            Body {
                mask: 2,
                ..at(0.0, 0.0)
            },
        ),
        (
            "wall",
            Body {
                layer: 4,
                ..at(3.0, 0.0)
            },
        ),
    ]);
    assert_eq!(
        w.move_body("actor", [8.0, 0.0]).unwrap().position,
        [8.0, 0.0]
    );
}

#[test]
fn walls_that_refuse_the_movers_layer_are_passed_through() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        (
            "wall",
            Body {
                mask: 2,
                ..at(3.0, 0.0)
            },
        ),
    ]);
    assert_eq!(
        w.move_body("actor", [8.0, 0.0]).unwrap().position,
        [8.0, 0.0]
    );
}

#[test]
fn collider_offsets_shift_the_contact_position() {
    let mut w = world(&[
        (
            "actor",
            Body {
                offset: [1.0, 0.0],
                ..at(0.0, 0.0)
            },
        ),
        ("wall", at(4.0, 0.0)),
    ]);
    assert_eq!(
        w.move_body("actor", [8.0, 0.0]).unwrap().position,
        [2.0, 0.0]
    );
}

fn bad_delta(delta: [f32; 2]) {
    let mut w = world(&[("a", at(0.0, 0.0))]);
    expect_code(w.move_body("a", delta), "collision.move.invalid");
    assert_eq!(pos(&w, "a"), [0.0, 0.0]);
}

table! { bad_delta;
    nan_displacement_is_rejected => ([f32::NAN, 0.0]);
    infinite_displacement_is_rejected => ([0.0, f32::INFINITY]);
    negative_infinite_displacement_is_rejected => ([f32::NEG_INFINITY, 1.0]);
    displacement_beyond_a_million_is_rejected => ([1_000_001.0, 0.0]);
    negative_displacement_beyond_a_million_is_rejected => ([0.0, -1_000_001.0]);
}

#[test]
fn moving_an_unknown_body_is_an_error() {
    expect_code(
        World::default().move_body("ghost", [1.0, 0.0]),
        "collision.body.unknown",
    );
}

#[test]
fn unknown_body_error_precedes_displacement_validation() {
    expect_code(
        World::default().move_body("ghost", [f32::NAN, 0.0]),
        "collision.body.unknown",
    );
}

#[test]
fn leaving_the_representable_world_is_rejected() {
    let mut w = world(&[("a", at(999_999.0, 0.0))]);
    expect_code(w.move_body("a", [10.0, 0.0]), "collision.move.bounds");
    assert_eq!(pos(&w, "a"), [999_999.0, 0.0]);
}

#[test]
fn failed_move_does_not_modify_other_bodies() {
    let mut w = world(&[("a", at(999_999.0, 0.0)), ("b", at(0.0, 0.0))]);
    assert!(w.move_body("a", [10.0, 0.0]).is_err());
    assert_eq!(pos(&w, "b"), [0.0, 0.0]);
}

// ---- multi-step simulations --------------------------------------------------------

#[test]
fn patrol_around_a_walled_room_visits_each_corner_deterministically() {
    let build = || {
        world(&[
            ("actor", at(0.0, 0.0)),
            ("east", boxed(6.0, 0.0, 2.0, 20.0)),
            ("north", boxed(0.0, 6.0, 20.0, 2.0)),
            ("west", boxed(-6.0, 0.0, 2.0, 20.0)),
            ("south", boxed(0.0, -6.0, 20.0, 2.0)),
        ])
    };
    let route = |w: &mut World| {
        [[16.0, 0.0], [0.0, 16.0], [-16.0, 0.0], [0.0, -16.0]]
            .map(|d| w.move_body("actor", d).unwrap())
    };
    let first = route(&mut build());
    assert_eq!(first[0].position, [4.5, 0.0]);
    assert_eq!(first[1].position, [4.5, 4.5]);
    assert_eq!(first[2].position, [-4.5, 4.5]);
    assert_eq!(first[3].position, [-4.5, -4.5]);
    assert_eq!(first[0].contacts, vec!["east"]);
    assert_eq!(first[1].contacts, vec!["north"]);
    assert_eq!(first[2].contacts, vec!["west"]);
    assert_eq!(first[3].contacts, vec!["south"]);
    assert_eq!(first, route(&mut build()));
}

#[test]
fn sliding_along_a_floor_reports_contact_every_step_and_keeps_height() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("floor", boxed(0.0, -2.0, 100.0, 1.0)),
    ]);
    for step in 1..=10 {
        let r = w.move_body("actor", [1.0, -1.0]).unwrap();
        assert_eq!(r.position, [step as f32, -1.0], "step {step}");
        assert_eq!(r.contacts, vec!["floor"]);
    }
}

#[test]
fn walking_off_a_ledge_falls_once_the_floor_ends() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("floor", boxed(0.0, -2.0, 4.0, 1.0)),
    ]);
    let mut path = Vec::new();
    for _ in 0..4 {
        path.push(w.move_body("actor", [1.0, -1.0]).unwrap());
    }
    let positions: Vec<_> = path.iter().map(|r| r.position).collect();
    assert_eq!(
        positions,
        vec![[1.0, -1.0], [2.0, -1.0], [3.0, -1.0], [4.0, -2.0]]
    );
    assert!(path[3].contacts.is_empty());
    assert_eq!(path[2].contacts, vec!["floor"]);
}

#[test]
fn pushing_diagonally_into_a_corner_never_penetrates_over_many_steps() {
    let mut w = world(&[
        ("actor", at(0.0, 0.0)),
        ("right", boxed(6.0, 0.0, 2.0, 40.0)),
        ("top", boxed(0.0, 6.0, 40.0, 2.0)),
    ]);
    for _ in 0..40 {
        w.move_body("actor", [0.75, 0.5]).unwrap();
        assert!(w.overlaps("actor").unwrap().is_empty());
    }
    assert_eq!(pos(&w, "actor"), [4.5, 4.5]);
}

#[test]
fn follower_closes_to_the_separation_distance_and_stays_there() {
    let mut w = world(&[("target", at(0.0, 0.0)), ("follower", at(10.0, 0.0))]);
    let mut history = Vec::new();
    for _ in 0..14 {
        let from = pos(&w, "follower");
        let v = follow_velocity(from, pos(&w, "target"), 2.0, 1.0).unwrap();
        w.move_body("follower", [v[0] * 0.5, v[1] * 0.5]).unwrap();
        history.push(pos(&w, "follower")[0]);
    }
    assert_eq!(history[0], 9.0);
    assert_eq!(history[8], 1.0);
    assert!(history[8..].iter().all(|x| *x == 1.0));
}

#[test]
fn flock_of_eight_surrounds_the_leader_without_overlaps() {
    let mut w = World::default();
    for i in 0..8 {
        let target = flock_target([2.0, -3.0], i, 8, 4.0).unwrap();
        w.upsert(format!("f{i}"), at(target[0], target[1])).unwrap();
    }
    for i in 0..8 {
        assert!(w.overlaps(&format!("f{i}")).unwrap().is_empty());
        let p = pos(&w, &format!("f{i}"));
        let radius = ((p[0] - 2.0).powi(2) + (p[1] + 3.0).powi(2)).sqrt();
        assert!((radius - 4.0).abs() < 1e-4);
    }
}

#[test]
fn trigger_body_sweeping_across_a_zone_enters_then_exits() {
    let mut w = world(&[("zone", trig(4.0, 0.0)), ("scout", at(0.0, 0.0))]);
    assert!(w.advance_triggers().is_empty());
    w.move_body("scout", [3.5, 0.0]).unwrap();
    assert_eq!(
        w.advance_triggers(),
        vec![event("scout", "zone", TriggerPhase::Enter)]
    );
    w.move_body("scout", [0.25, 0.0]).unwrap();
    assert_eq!(
        w.advance_triggers(),
        vec![event("scout", "zone", TriggerPhase::Stay)]
    );
    w.move_body("scout", [3.0, 0.0]).unwrap();
    assert_eq!(
        w.advance_triggers(),
        vec![event("scout", "zone", TriggerPhase::Exit)]
    );
    assert!(w.advance_triggers().is_empty());
}

// ---- trigger event lifecycle ---------------------------------------------------------

#[test]
fn trigger_events_go_enter_stay_exit_then_silence() {
    let mut w = world(&[("zone", trig(0.0, 0.0)), ("walker", at(5.0, 0.0))]);
    assert!(w.advance_triggers().is_empty());
    w.upsert("walker", at(0.5, 0.0)).unwrap();
    assert_eq!(
        w.advance_triggers(),
        vec![event("walker", "zone", TriggerPhase::Enter)]
    );
    assert_eq!(
        w.advance_triggers(),
        vec![event("walker", "zone", TriggerPhase::Stay)]
    );
    w.upsert("walker", at(5.0, 0.0)).unwrap();
    assert_eq!(
        w.advance_triggers(),
        vec![event("walker", "zone", TriggerPhase::Exit)]
    );
    assert!(w.advance_triggers().is_empty());
}

#[test]
fn trigger_pairs_are_ordered_by_first_then_second_id() {
    let mut w = world(&[
        ("b", trig(0.0, 0.0)),
        ("a", at(-0.7, 0.0)),
        ("c", at(0.7, 0.0)),
    ]);
    let events = w.advance_triggers();
    assert_eq!(
        events,
        vec![
            event("a", "b", TriggerPhase::Enter),
            event("b", "c", TriggerPhase::Enter)
        ]
    );
}

#[test]
fn trigger_event_order_does_not_depend_on_insertion_order() {
    let mut x = world(&[
        ("c", at(0.7, 0.0)),
        ("b", trig(0.0, 0.0)),
        ("a", at(-0.7, 0.0)),
    ]);
    let mut y = world(&[
        ("a", at(-0.7, 0.0)),
        ("b", trig(0.0, 0.0)),
        ("c", at(0.7, 0.0)),
    ]);
    assert_eq!(x.advance_triggers(), y.advance_triggers());
}

#[test]
fn exit_events_sort_together_with_stay_events() {
    let mut w = world(&[
        ("m", trig(0.0, 0.0)),
        ("a", at(0.0, 0.0)),
        ("z", at(0.0, 0.0)),
    ]);
    w.advance_triggers();
    w.upsert("a", at(9.0, 0.0)).unwrap();
    assert_eq!(
        w.advance_triggers(),
        vec![
            event("a", "m", TriggerPhase::Exit),
            event("m", "z", TriggerPhase::Stay)
        ]
    );
}

#[test]
fn removing_a_body_emits_exit_exactly_once() {
    let mut w = world(&[("zone", trig(0.0, 0.0)), ("walker", at(0.0, 0.0))]);
    w.advance_triggers();
    w.remove("walker");
    assert_eq!(
        w.advance_triggers(),
        vec![event("walker", "zone", TriggerPhase::Exit)]
    );
    assert!(w.advance_triggers().is_empty());
}

#[test]
fn removing_the_trigger_itself_emits_exit() {
    let mut w = world(&[("zone", trig(0.0, 0.0)), ("walker", at(0.0, 0.0))]);
    w.advance_triggers();
    w.remove("zone");
    assert_eq!(w.advance_triggers()[0].phase, TriggerPhase::Exit);
}

#[test]
fn solid_overlaps_without_a_trigger_emit_nothing() {
    let mut w = world(&[("a", at(0.0, 0.0)), ("b", at(0.5, 0.0))]);
    assert!(w.advance_triggers().is_empty());
}

#[test]
fn two_overlapping_triggers_still_pair_up() {
    let mut w = world(&[("t1", trig(0.0, 0.0)), ("t2", trig(0.5, 0.0))]);
    assert_eq!(
        w.advance_triggers(),
        vec![event("t1", "t2", TriggerPhase::Enter)]
    );
}

#[test]
fn touching_triggers_do_not_fire() {
    let mut w = world(&[("zone", trig(0.0, 0.0)), ("walker", at(1.0, 0.0))]);
    assert!(w.advance_triggers().is_empty());
}

#[test]
fn trigger_masks_filter_events() {
    let mut w = world(&[
        (
            "zone",
            Body {
                mask: 2,
                ..trig(0.0, 0.0)
            },
        ),
        (
            "walker",
            Body {
                layer: 1,
                ..at(0.0, 0.0)
            },
        ),
    ]);
    assert!(w.advance_triggers().is_empty());
    w.upsert(
        "walker",
        Body {
            layer: 2,
            ..at(0.0, 0.0)
        },
    )
    .unwrap();
    assert_eq!(w.advance_triggers().len(), 1);
}

#[test]
fn clearing_the_world_forgets_trigger_history() {
    let mut w = world(&[("zone", trig(0.0, 0.0)), ("walker", at(0.0, 0.0))]);
    w.advance_triggers();
    w.clear();
    w.upsert("zone", trig(0.0, 0.0)).unwrap();
    w.upsert("walker", at(0.0, 0.0)).unwrap();
    assert_eq!(w.advance_triggers()[0].phase, TriggerPhase::Enter);
}

#[test]
fn replacing_a_body_with_same_id_and_overlap_keeps_stay_state() {
    let mut w = world(&[("zone", trig(0.0, 0.0)), ("walker", at(0.0, 0.0))]);
    w.advance_triggers();
    w.upsert("walker", at(0.25, 0.0)).unwrap();
    assert_eq!(w.advance_triggers()[0].phase, TriggerPhase::Stay);
}

#[test]
fn trigger_phases_serialize_in_lowercase() {
    assert_eq!(
        serde_json::to_string(&TriggerPhase::Enter).unwrap(),
        "\"enter\""
    );
    assert_eq!(
        serde_json::to_string(&TriggerPhase::Exit).unwrap(),
        "\"exit\""
    );
}

// ---- safe-position search -------------------------------------------------------------

#[test]
fn find_safe_returns_the_origin_when_it_is_already_free() {
    let w = world(&[("rock", at(5.0, 0.0))]);
    assert_eq!(
        w.find_safe(at(0.0, 0.0), 3.0, 1.0, "").unwrap(),
        Some([0.0, 0.0])
    );
}

#[test]
fn find_safe_prefers_the_left_neighbour_first() {
    let w = world(&[("rock", at(0.0, 0.0))]);
    assert_eq!(
        w.find_safe(at(0.0, 0.0), 3.0, 1.0, "").unwrap(),
        Some([-1.0, 0.0])
    );
}

#[test]
fn find_safe_tries_down_when_left_is_blocked() {
    let w = world(&[("rock", at(0.0, 0.0)), ("l", at(-1.0, 0.0))]);
    assert_eq!(
        w.find_safe(at(0.0, 0.0), 3.0, 1.0, "").unwrap(),
        Some([0.0, -1.0])
    );
}

#[test]
fn find_safe_tries_up_when_left_and_down_are_blocked() {
    let w = world(&[
        ("rock", at(0.0, 0.0)),
        ("l", at(-1.0, 0.0)),
        ("d", at(0.0, -1.0)),
    ]);
    assert_eq!(
        w.find_safe(at(0.0, 0.0), 3.0, 1.0, "").unwrap(),
        Some([0.0, 1.0])
    );
}

#[test]
fn find_safe_tries_right_when_three_sides_are_blocked() {
    let w = world(&[
        ("rock", at(0.0, 0.0)),
        ("l", at(-1.0, 0.0)),
        ("d", at(0.0, -1.0)),
        ("u", at(0.0, 1.0)),
    ]);
    assert_eq!(
        w.find_safe(at(0.0, 0.0), 3.0, 1.0, "").unwrap(),
        Some([1.0, 0.0])
    );
}

#[test]
fn find_safe_expands_to_the_next_ring_when_ring_one_is_full() {
    let w = world(&[
        ("rock", at(0.0, 0.0)),
        ("l", at(-1.0, 0.0)),
        ("d", at(0.0, -1.0)),
        ("u", at(0.0, 1.0)),
        ("r", at(1.0, 0.0)),
    ]);
    assert_eq!(
        w.find_safe(at(0.0, 0.0), 3.0, 1.0, "").unwrap(),
        Some([-2.0, 0.0])
    );
}

#[test]
fn find_safe_gives_up_within_a_small_radius() {
    let w = world(&[("slab", boxed(0.0, 0.0, 10.0, 10.0))]);
    assert_eq!(w.find_safe(at(0.0, 0.0), 1.0, 0.5, "").unwrap(), None);
}

#[test]
fn find_safe_with_zero_radius_only_accepts_the_origin() {
    let blocked = world(&[("rock", at(0.0, 0.0))]);
    assert_eq!(blocked.find_safe(at(0.0, 0.0), 0.0, 1.0, "").unwrap(), None);
    let free = World::default();
    assert_eq!(
        free.find_safe(at(0.0, 0.0), 0.0, 1.0, "").unwrap(),
        Some([0.0, 0.0])
    );
}

#[test]
fn find_safe_ignores_triggers() {
    let w = world(&[("zone", trig(0.0, 0.0))]);
    assert_eq!(
        w.find_safe(at(0.0, 0.0), 2.0, 1.0, "").unwrap(),
        Some([0.0, 0.0])
    );
}

#[test]
fn find_safe_ignores_the_body_being_relocated() {
    let w = world(&[("me", at(0.0, 0.0))]);
    assert_eq!(
        w.find_safe(at(0.0, 0.0), 2.0, 1.0, "me").unwrap(),
        Some([0.0, 0.0])
    );
}

#[test]
fn find_safe_ignores_obstacles_on_other_layers() {
    let w = world(&[(
        "rock",
        Body {
            layer: 4,
            ..at(0.0, 0.0)
        },
    )]);
    let probe = Body {
        mask: 1,
        ..at(0.0, 0.0)
    };
    assert_eq!(w.find_safe(probe, 2.0, 1.0, "").unwrap(), Some([0.0, 0.0]));
}

#[test]
fn find_safe_last_ring_is_clamped_to_the_radius() {
    let w = world(&[("slab", boxed(0.0, 0.0, 10.0, 10.0))]);
    assert_eq!(
        w.find_safe(at(0.0, 0.0), 6.0, 4.0, "").unwrap(),
        Some([-6.0, 0.0])
    );
}

#[test]
fn find_safe_accepts_exactly_64_rings() {
    let w = world(&[("slab", boxed(0.0, 0.0, 200.0, 200.0))]);
    assert_eq!(w.find_safe(at(0.0, 0.0), 64.0, 1.0, "").unwrap(), None);
}

fn bad_search(radius: f32, step: f32) {
    let w = World::default();
    expect_code(
        w.find_safe(at(0.0, 0.0), radius, step, ""),
        "collision.search.invalid",
    );
}

table! { bad_search;
    search_negative_radius_is_rejected => (-1.0, 1.0);
    search_nan_radius_is_rejected => (f32::NAN, 1.0);
    search_radius_beyond_a_million_is_rejected => (1_000_001.0, 1_000_000.0);
    search_zero_step_is_rejected => (2.0, 0.0);
    search_negative_step_is_rejected => (2.0, -1.0);
    search_nan_step_is_rejected => (2.0, f32::NAN);
    search_infinite_step_is_rejected => (2.0, f32::INFINITY);
    search_more_than_64_rings_is_rejected => (65.0, 1.0);
}

#[test]
fn find_safe_rejects_an_invalid_starting_body() {
    let w = World::default();
    expect_code(
        w.find_safe(at(f32::NAN, 0.0), 1.0, 1.0, ""),
        "collision.search.invalid",
    );
}

// ---- follow steering ------------------------------------------------------------------

fn follow(from: [f32; 2], to: [f32; 2], speed: f32, separation: f32, expected: [f32; 2]) {
    let v = follow_velocity(from, to, speed, separation).unwrap();
    assert!(
        near(v[0], expected[0]) && near(v[1], expected[1]),
        "{v:?} vs {expected:?}"
    );
}

table! { follow;
    follow_moves_along_positive_x => ([0.0, 0.0], [10.0, 0.0], 3.0, 1.0, [3.0, 0.0]);
    follow_moves_along_negative_y => ([0.0, 5.0], [0.0, 0.0], 2.0, 1.0, [0.0, -2.0]);
    follow_diagonal_has_speed_magnitude => ([0.0, 0.0], [3.0, 4.0], 5.0, 0.5, [3.0, 4.0]);
    follow_inside_separation_stops => ([0.0, 0.0], [0.5, 0.0], 3.0, 1.0, [0.0, 0.0]);
    follow_exactly_at_separation_stops => ([0.0, 0.0], [1.0, 0.0], 3.0, 1.0, [0.0, 0.0]);
    follow_just_beyond_separation_moves => ([0.0, 0.0], [1.5, 0.0], 3.0, 1.0, [3.0, 0.0]);
    follow_same_position_does_not_divide_by_zero => ([2.0, 2.0], [2.0, 2.0], 3.0, 0.0, [0.0, 0.0]);
    follow_with_zero_speed_is_still => ([0.0, 0.0], [9.0, 0.0], 0.0, 1.0, [0.0, 0.0]);
    follow_speed_is_independent_of_distance => ([0.0, 0.0], [1000.0, 0.0], 3.0, 1.0, [3.0, 0.0]);
}

fn bad_follow(from: [f32; 2], to: [f32; 2], speed: f32, separation: f32) {
    expect_code(
        follow_velocity(from, to, speed, separation),
        "gameplay.follow.invalid",
    );
}

table! { bad_follow;
    follow_nan_origin_is_rejected => ([f32::NAN, 0.0], [1.0, 0.0], 1.0, 0.0);
    follow_infinite_target_is_rejected => ([0.0, 0.0], [f32::INFINITY, 0.0], 1.0, 0.0);
    follow_target_beyond_limits_is_rejected => ([0.0, 0.0], [2.0e6, 0.0], 1.0, 0.0);
    follow_negative_speed_is_rejected => ([0.0, 0.0], [1.0, 0.0], -1.0, 0.0);
    follow_nan_speed_is_rejected => ([0.0, 0.0], [1.0, 0.0], f32::NAN, 0.0);
    follow_speed_beyond_limits_is_rejected => ([0.0, 0.0], [1.0, 0.0], 2.0e6, 0.0);
    follow_negative_separation_is_rejected => ([0.0, 0.0], [1.0, 0.0], 1.0, -0.1);
    follow_nan_separation_is_rejected => ([0.0, 0.0], [1.0, 0.0], 1.0, f32::NAN);
}

// ---- flock targets ---------------------------------------------------------------------

fn flock(center: [f32; 2], index: usize, count: usize, radius: f32, expected: [f32; 2]) {
    let t = flock_target(center, index, count, radius).unwrap();
    assert!(
        near(t[0], expected[0]) && near(t[1], expected[1]),
        "{t:?} vs {expected:?}"
    );
}

table! { flock;
    flock_first_member_sits_on_the_positive_x_axis => ([0.0, 0.0], 0, 4, 2.0, [2.0, 0.0]);
    flock_quarter_member_sits_on_the_positive_y_axis => ([0.0, 0.0], 1, 4, 2.0, [0.0, 2.0]);
    flock_half_member_sits_on_the_negative_x_axis => ([0.0, 0.0], 2, 4, 2.0, [-2.0, 0.0]);
    flock_three_quarter_member_sits_on_the_negative_y_axis => ([0.0, 0.0], 3, 4, 2.0, [0.0, -2.0]);
    flock_is_offset_by_the_center => ([10.0, -5.0], 0, 2, 3.0, [13.0, -5.0]);
    flock_single_member_takes_the_east_slot => ([1.0, 1.0], 0, 1, 4.0, [5.0, 1.0]);
    flock_zero_radius_collapses_onto_the_center => ([3.0, 4.0], 2, 5, 0.0, [3.0, 4.0]);
    flock_third_of_three_is_at_240_degrees => ([0.0, 0.0], 2, 3, 2.0, [-1.0, -1.7320508]);
}

fn bad_flock(center: [f32; 2], index: usize, count: usize, radius: f32, expected: &str) {
    expect_code(flock_target(center, index, count, radius), expected);
}

table! { bad_flock;
    flock_zero_count_is_rejected => ([0.0, 0.0], 0, 0, 1.0, "gameplay.flock.invalid");
    flock_index_equal_to_count_is_rejected => ([0.0, 0.0], 4, 4, 1.0, "gameplay.flock.invalid");
    flock_count_above_capacity_is_rejected => ([0.0, 0.0], 0, 1025, 1.0, "gameplay.flock.invalid");
    flock_negative_radius_is_rejected => ([0.0, 0.0], 0, 4, -1.0, "gameplay.flock.invalid");
    flock_nan_radius_is_rejected => ([0.0, 0.0], 0, 4, f32::NAN, "gameplay.flock.invalid");
    flock_center_beyond_limits_is_rejected => ([2.0e6, 0.0], 0, 4, 1.0, "gameplay.flock.invalid");
    flock_target_beyond_limits_is_rejected => ([1_000_000.0, 0.0], 0, 4, 1_000_000.0, "gameplay.flock.bounds");
}

#[test]
fn flock_capacity_of_1024_members_is_accepted() {
    assert!(flock_target([0.0, 0.0], 1023, 1024, 1.0).is_ok());
}

#[test]
fn flock_members_get_distinct_targets() {
    let targets: Vec<_> = (0..16)
        .map(|i| flock_target([0.0; 2], i, 16, 5.0).unwrap())
        .collect();
    for (i, a) in targets.iter().enumerate() {
        assert!(targets[i + 1..].iter().all(|b| a != b));
    }
}
