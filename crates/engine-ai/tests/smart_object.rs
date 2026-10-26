//! Smart objects: slots, claims, occupation, timeouts and arbitration.
use engine_ai::smart_object::{
    ClaimRequest, ObjectDef, ReleaseReason, SlotDef, SlotError, SlotRef, SlotState, SmartEvent,
    SmartObjects,
};

fn slot(name: &str, offset: [f32; 3], tags: &[&str]) -> SlotDef {
    SlotDef {
        name: name.into(),
        offset,
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
        behavior: format!("use_{name}"),
    }
}

fn bench(timeout: f32) -> ObjectDef {
    ObjectDef {
        slots: vec![
            slot("left", [-1.0, 0.0, 0.0], &["sit", "wood"]),
            slot("right", [1.0, 0.0, 0.0], &["sit"]),
            slot("top", [0.0, 1.0, 0.0], &["stand"]),
        ],
        claim_timeout: timeout,
    }
}

fn at(object: u64, slot: u32) -> SlotRef {
    SlotRef { object, slot }
}

fn tags(list: &[&str]) -> Vec<String> {
    list.iter().map(|t| (*t).to_owned()).collect()
}

#[test]
fn slots_are_found_by_tags_in_a_stable_order() {
    let mut objects = SmartObjects::default();
    objects.set_object(20, bench(10.0)).unwrap();
    objects.set_object(10, bench(10.0)).unwrap();
    assert_eq!(objects.object_count(), 2);
    assert_eq!(
        objects.find(&tags(&["sit"]), true),
        [at(10, 0), at(10, 1), at(20, 0), at(20, 1)]
    );
    assert_eq!(
        objects.find(&tags(&["sit", "wood"]), true),
        [at(10, 0), at(20, 0)]
    );
    assert_eq!(objects.find(&tags(&["nothing"]), true), []);
    assert_eq!(
        objects.find(&[], true).len(),
        6,
        "no tags matches every slot"
    );
    objects.claim(at(10, 0), 7).unwrap();
    assert_eq!(objects.find(&tags(&["sit", "wood"]), true), [at(20, 0)]);
    assert_eq!(
        objects.find(&tags(&["sit", "wood"]), false),
        [at(10, 0), at(20, 0)],
        "free_only false lists held slots too"
    );
}

#[test]
fn a_slot_goes_free_claimed_occupied_free_with_events() {
    let mut objects = SmartObjects::default();
    objects.set_object(1, bench(10.0)).unwrap();
    let slot = at(1, 1);
    assert_eq!(objects.state(slot), Some(SlotState::Free));
    assert_eq!(
        objects.claim(slot, 5),
        Ok(SmartEvent::Claimed { slot, user: 5 })
    );
    assert!(matches!(
        objects.state(slot),
        Some(SlotState::Claimed { user: 5, remaining: Some(r) }) if (r - 10.0).abs() < 1e-9
    ));
    assert_eq!(objects.claim(slot, 6), Err(SlotError::Taken(5)));
    assert_eq!(
        objects.claim(slot, 5),
        Err(SlotError::Taken(5)),
        "no double claim"
    );
    assert_eq!(objects.occupy(slot, 6), Err(SlotError::NotClaimed));
    assert_eq!(
        objects.occupy(slot, 5),
        Ok(SmartEvent::Activated { slot, user: 5 })
    );
    assert_eq!(objects.state(slot), Some(SlotState::Occupied { user: 5 }));
    assert_eq!(
        objects.occupy(slot, 5),
        Err(SlotError::NotClaimed),
        "already occupied"
    );
    assert_eq!(
        objects.release(slot, 6, ReleaseReason::Completed),
        Err(SlotError::NotClaimed)
    );
    assert_eq!(
        objects.release(slot, 5, ReleaseReason::Completed),
        Ok(SmartEvent::Released {
            slot,
            user: 5,
            reason: ReleaseReason::Completed
        })
    );
    assert_eq!(objects.state(slot), Some(SlotState::Free));
    assert_eq!(objects.claim(at(9, 0), 5), Err(SlotError::UnknownSlot));
    assert_eq!(objects.claim(at(1, 9), 5), Err(SlotError::UnknownSlot));
    assert_eq!(objects.slot_def(slot).unwrap().behavior, "use_right");
}

#[test]
fn unoccupied_claims_expire_at_the_timeout_for_both_tick_rates() {
    // (dt, claim_timeout, ticks until released)
    for (dt, timeout, ticks) in [
        (0.1f32, 1.0f32, 10u32),
        (1.0 / 60.0, 1.0, 60),
        (0.1, 0.25, 3),
    ] {
        let mut objects = SmartObjects::default();
        objects.set_object(1, bench(timeout)).unwrap();
        objects.claim(at(1, 0), 5).unwrap();
        for n in 1..=ticks + 5 {
            let events = objects.tick(dt);
            if n < ticks {
                assert!(events.is_empty(), "dt {dt} timeout {timeout} tick {n}");
            } else if n == ticks {
                assert_eq!(
                    events,
                    [SmartEvent::Released {
                        slot: at(1, 0),
                        user: 5,
                        reason: ReleaseReason::Timeout
                    }],
                    "dt {dt} timeout {timeout}"
                );
            } else {
                assert!(events.is_empty());
            }
        }
        assert_eq!(objects.state(at(1, 0)), Some(SlotState::Free));
    }
    // Occupied slots and timeout 0 never expire.
    let mut objects = SmartObjects::default();
    objects.set_object(1, bench(1.0)).unwrap();
    objects.set_object(2, bench(0.0)).unwrap();
    objects.claim(at(1, 0), 5).unwrap();
    objects.occupy(at(1, 0), 5).unwrap();
    objects.claim(at(2, 0), 6).unwrap();
    for _ in 0..100 {
        assert!(objects.tick(0.1).is_empty());
    }
    assert_eq!(
        objects.state(at(1, 0)),
        Some(SlotState::Occupied { user: 5 })
    );
    assert!(matches!(
        objects.state(at(2, 0)),
        Some(SlotState::Claimed {
            user: 6,
            remaining: None
        })
    ));
}

#[test]
fn removed_users_and_objects_release_what_they_held() {
    let mut objects = SmartObjects::default();
    objects.set_object(1, bench(10.0)).unwrap();
    objects.set_object(2, bench(10.0)).unwrap();
    objects.claim(at(1, 0), 5).unwrap();
    objects.claim(at(2, 2), 5).unwrap();
    objects.occupy(at(2, 2), 5).unwrap();
    objects.claim(at(2, 1), 6).unwrap();
    let released = objects.purge_user(5);
    assert_eq!(released.len(), 2);
    assert!(released.iter().all(|e| matches!(
        e,
        SmartEvent::Released {
            user: 5,
            reason: ReleaseReason::Removed,
            ..
        }
    )));
    assert_eq!(objects.state(at(1, 0)), Some(SlotState::Free));
    let released = objects.remove_object(2);
    assert_eq!(
        released,
        [SmartEvent::Released {
            slot: at(2, 1),
            user: 6,
            reason: ReleaseReason::Removed
        }]
    );
    assert_eq!(objects.state(at(2, 1)), None);
    assert!(objects.remove_object(2).is_empty());
}

#[test]
fn redefining_an_object_keeps_the_state_of_unchanged_slots() {
    let mut objects = SmartObjects::default();
    objects.set_object(1, bench(10.0)).unwrap();
    objects.claim(at(1, 0), 5).unwrap();
    objects.claim(at(1, 2), 6).unwrap();
    let mut changed = bench(10.0);
    changed.slots.truncate(2);
    changed.slots[1].name = "renamed".into();
    let events = objects.set_object(1, changed).unwrap();
    assert_eq!(
        objects.state(at(1, 0)),
        Some(SlotState::Claimed {
            user: 5,
            remaining: Some(10.0)
        })
    );
    assert_eq!(objects.state(at(1, 1)), Some(SlotState::Free));
    assert_eq!(
        events,
        [SmartEvent::Released {
            slot: at(1, 2),
            user: 6,
            reason: ReleaseReason::Removed
        }]
    );
}

#[test]
fn arbitration_serves_priority_then_user_then_nearest_slot() {
    let position = |slot: SlotRef| -> [f32; 3] {
        match slot.slot {
            0 => [0.0, 0.0, 0.0],
            1 => [10.0, 0.0, 0.0],
            _ => [20.0, 0.0, 0.0],
        }
    };
    let request = |user, priority, x| ClaimRequest {
        user,
        tags: tags(&["sit"]),
        position: [x, 0.0, 0.0],
        priority,
    };
    let mut objects = SmartObjects::default();
    objects.set_object(1, bench(10.0)).unwrap(); // sit slots: 0 (x=0) and 1 (x=10)
    // Two sitters, one of them urgent: the urgent one picks first even though its id is higher.
    let served = objects.arbitrate(
        &[request(3, 0, 0.0), request(9, 5, 0.0), request(4, 0, 9.0)],
        position,
    );
    let users: Vec<_> = served.iter().map(|(u, _)| *u).collect();
    assert_eq!(users, [9, 3, 4], "priority first, then user id");
    assert_eq!(
        served[0].1.as_ref().map(|s| s.0),
        Some(at(1, 0)),
        "9 takes the nearest"
    );
    assert_eq!(
        served[1].1.as_ref().map(|s| s.0),
        Some(at(1, 1)),
        "3 gets the other"
    );
    assert!(served[2].1.is_none(), "4 finds nothing free");
    assert_eq!(
        objects.state(at(1, 0)),
        Some(SlotState::Claimed {
            user: 9,
            remaining: Some(10.0)
        })
    );
    // Equal distances go to the lowest slot.
    let mut objects = SmartObjects::default();
    objects.set_object(1, bench(10.0)).unwrap();
    let served = objects.arbitrate(&[request(1, 0, 5.0)], position);
    assert_eq!(served[0].1.as_ref().map(|s| s.0), Some(at(1, 0)));
}

#[test]
fn definitions_are_validated() {
    let bad = |edit: fn(&mut ObjectDef)| {
        let mut def = bench(10.0);
        edit(&mut def);
        def.validate().is_err()
    };
    assert!(bench(10.0).validate().is_ok());
    assert!(bad(|d| d.claim_timeout = -1.0));
    assert!(bad(|d| d.claim_timeout = f32::NAN));
    assert!(bad(|d| d.slots[0].name = String::new()));
    assert!(bad(|d| d.slots[1].name = "left".into()));
    assert!(bad(|d| d.slots[0].name = "bad name".into()));
    assert!(bad(|d| d.slots[0].offset[0] = f32::INFINITY));
    assert!(bad(
        |d| d.slots[0].tags = (0..9).map(|i| format!("t{i}")).collect()
    ));
    assert!(bad(|d| d.slots[0].tags = vec!["a b".into()]));
    assert!(bad(|d| d.slots[0].behavior = "x".repeat(200)));
    assert!(bad(|d| {
        d.slots = (0..17)
            .map(|i| slot(&format!("s{i}"), [0.0; 3], &[]))
            .collect();
    }));
    let mut objects = SmartObjects::default();
    assert!(
        objects
            .set_object(
                1,
                ObjectDef {
                    claim_timeout: -5.0,
                    ..ObjectDef::default()
                }
            )
            .is_err()
    );
}
