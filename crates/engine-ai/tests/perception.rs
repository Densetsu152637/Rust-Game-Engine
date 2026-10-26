//! Perception: sight cone, occlusion, ray budget, hearing, damage, teams, memory.
use engine_ai::perception::{
    ANONYMOUS, Affiliation, AffiliationFilter, Damage, DamageConfig, EventKind, HearingConfig, Id,
    MAX_RECORDS, Noise, PerceiverInput, Perception, PerceptionHost, Sense, SensorConfig,
    SightConfig, TargetInput, TeamTable, UpdateInput, UpdateReport,
};
use std::collections::BTreeSet;

/// Blocks segments whose target is in `blocked`; counts rays.
#[derive(Default)]
struct Host {
    blocked: BTreeSet<Id>,
    rays: Vec<(Id, Id)>,
    attenuation: Option<f32>,
}

impl PerceptionHost for Host {
    fn line_of_sight(&mut self, perceiver: Id, target: Id, _: [f32; 3], _: [f32; 3]) -> bool {
        self.rays.push((perceiver, target));
        !self.blocked.contains(&target)
    }
    fn hearing_attenuation(&mut self, _: Id, _: [f32; 3], _: Option<Id>, _: [f32; 3]) -> f32 {
        self.attenuation.unwrap_or(1.0)
    }
}

const ME: Id = 1;

fn sight() -> SightConfig {
    SightConfig {
        range: 10.0,
        lose_range: 12.0,
        half_angle: 45.0,
        peripheral_half_angle: 60.0,
        height_offset: 0.0,
        max_age: 1.0,
        affiliations: AffiliationFilter::HOSTILE,
    }
}

fn perceiver(config: SensorConfig) -> PerceiverInput {
    PerceiverInput {
        id: ME,
        position: [0.0; 3],
        forward: [1.0, 0.0, 0.0],
        config,
    }
}

fn sensor(team: u32) -> SensorConfig {
    SensorConfig {
        team,
        sight: Some(sight()),
        ..SensorConfig::default()
    }
}

/// A target `distance` away at `angle` degrees from the forward (+X) axis in the XZ plane.
fn at(id: Id, distance: f32, angle: f32, team: u32) -> TargetInput {
    let a = angle.to_radians();
    TargetInput {
        id,
        position: [distance * a.cos(), 0.0, distance * a.sin()],
        team,
        height_offset: 0.0,
    }
}

fn input(config: SensorConfig, targets: Vec<TargetInput>) -> UpdateInput {
    UpdateInput {
        perceivers: vec![perceiver(config)],
        targets,
        noises: vec![],
        damages: vec![],
        max_rays: 64,
    }
}

fn sensed_subjects(p: &Perception) -> Vec<Id> {
    p.known(ME)
        .into_iter()
        .filter(|k| k.sensed)
        .map(|k| k.subject)
        .collect()
}

#[test]
fn the_sight_cone_accepts_and_rejects_at_range_and_angle_edges() {
    // (distance, angle, sensed)
    let cases = [
        (9.9, 0.0, true),
        (10.1, 0.0, false),
        (5.0, 44.0, true),
        (5.0, 46.0, false),
        (5.0, -44.0, true),
        (5.0, -46.0, false),
        (5.0, 90.0, false),
        (5.0, 180.0, false),
        (0.0, 0.0, true),
    ];
    for (distance, angle, expected) in cases {
        let mut p = Perception::default();
        let mut host = Host::default();
        p.update(
            0.1,
            &input(sensor(1), vec![at(2, distance, angle, 2)]),
            &mut host,
        );
        assert_eq!(
            sensed_subjects(&p) == [2],
            expected,
            "distance {distance} angle {angle}"
        );
    }
}

#[test]
fn a_sensed_target_is_kept_inside_the_lose_range_and_peripheral_angle() {
    // (distance, angle, still sensed)
    let cases = [
        (11.5, 0.0, true),
        (12.5, 0.0, false),
        (5.0, 55.0, true),
        (5.0, 65.0, false),
    ];
    for (distance, angle, expected) in cases {
        let mut p = Perception::default();
        let mut host = Host::default();
        p.update(0.1, &input(sensor(1), vec![at(2, 5.0, 0.0, 2)]), &mut host);
        assert_eq!(sensed_subjects(&p), [2]);
        let report = p.update(
            0.1,
            &input(sensor(1), vec![at(2, distance, angle, 2)]),
            &mut host,
        );
        assert_eq!(sensed_subjects(&p) == [2], expected, "{distance} {angle}");
        let lost = report.events.iter().any(|e| e.kind == EventKind::Lost);
        assert_eq!(lost, !expected);
    }
}

#[test]
fn eye_height_and_target_height_offsets_shift_the_sight_line() {
    let mut config = sensor(1);
    config.sight.as_mut().unwrap().height_offset = 5.0;
    config.sight.as_mut().unwrap().half_angle = 10.0;
    config.sight.as_mut().unwrap().peripheral_half_angle = 10.0;
    let mut p = Perception::default();
    let mut host = Host::default();
    // 8 away, eye 5 above: the target sits 32 degrees below the forward axis.
    p.update(
        0.1,
        &input(config.clone(), vec![at(2, 8.0, 0.0, 2)]),
        &mut host,
    );
    assert!(sensed_subjects(&p).is_empty());
    let mut tall = at(2, 8.0, 0.0, 2);
    tall.height_offset = 5.0;
    p.update(0.1, &input(config, vec![tall]), &mut host);
    assert_eq!(sensed_subjects(&p), [2], "aiming at the head lines up");
}

#[test]
fn occlusion_blocks_sight_until_the_line_clears() {
    let mut p = Perception::default();
    let mut host = Host::default();
    host.blocked.insert(2);
    let scene = || input(sensor(1), vec![at(2, 5.0, 0.0, 2), at(3, 5.0, 10.0, 2)]);
    let report = p.update(0.1, &scene(), &mut host);
    assert_eq!(sensed_subjects(&p), [3], "only the unblocked target");
    assert_eq!(report.rays_cast, 2);
    host.blocked.clear();
    p.update(0.1, &scene(), &mut host);
    assert_eq!(sensed_subjects(&p), [2, 3]);
    host.blocked.insert(3);
    let report = p.update(0.1, &scene(), &mut host);
    assert_eq!(sensed_subjects(&p), [2]);
    assert!(
        report
            .events
            .iter()
            .any(|e| e.kind == EventKind::Lost && e.subject == 3)
    );
    // Targets outside the cone cost no ray.
    let mut host = Host::default();
    let mut p = Perception::default();
    let report = p.update(
        0.1,
        &input(sensor(1), vec![at(2, 5.0, 120.0, 2), at(3, 50.0, 0.0, 2)]),
        &mut host,
    );
    assert_eq!((report.rays_cast, host.rays.len()), (0, 0));
}

#[test]
fn the_ray_budget_is_served_round_robin_and_unserved_pairs_keep_their_result() {
    let targets = || (2..=6).map(|id| at(id, 5.0, 0.0, 2)).collect::<Vec<_>>();
    let mut p = Perception::default();
    let mut host = Host::default();
    let mut served: Vec<Vec<Id>> = Vec::new();
    let mut known_after: Vec<usize> = Vec::new();
    for _ in 0..4 {
        let mut scene = input(sensor(1), targets());
        scene.max_rays = 2;
        let before = host.rays.len();
        let report = p.update(0.1, &scene, &mut host);
        assert!(report.rays_cast <= 2);
        assert_eq!(report.rays_cast + report.rays_deferred, 5);
        served.push(host.rays[before..].iter().map(|r| r.1).collect());
        known_after.push(sensed_subjects(&p).len());
    }
    assert_eq!(
        served,
        [vec![2, 3], vec![4, 5], vec![2, 6], vec![3, 4]],
        "rotation resumes after the last pair served (3rd update: 6, then wraps to 2)"
    );
    assert_eq!(
        known_after,
        [2, 4, 5, 5],
        "sensing builds up as pairs are served"
    );
    // A target that was sensed stays sensed while its ray waits for its turn.
    host.blocked.insert(2);
    let mut scene = input(sensor(1), targets());
    scene.max_rays = 1;
    p.update(0.1, &scene, &mut host);
    assert!(
        sensed_subjects(&p).contains(&2),
        "the ray for 2 was deferred"
    );
    // With the budget spent unevenly, a full rotation reaches every pair.
    for _ in 0..5 {
        p.update(0.1, &scene, &mut host);
    }
    assert!(
        !sensed_subjects(&p).contains(&2),
        "2 was re-checked and is blocked"
    );
}

fn noise(source: Option<Id>, distance: f32, loudness: f32, range: f32, team: u32) -> Noise {
    Noise {
        source,
        position: [distance, 0.0, 0.0],
        loudness,
        range,
        team,
    }
}

fn hearing_sensor(team: u32) -> SensorConfig {
    SensorConfig {
        team,
        hearing: Some(HearingConfig {
            range: 30.0,
            threshold: 0.05,
            max_age: 1.0,
            affiliations: AffiliationFilter::ALL,
        }),
        ..SensorConfig::default()
    }
}

fn hear(config: SensorConfig, noises: Vec<Noise>, attenuation: Option<f32>) -> Vec<(Id, f32)> {
    let mut p = Perception::default();
    let mut host = Host {
        attenuation,
        ..Host::default()
    };
    let mut scene = input(config, vec![]);
    scene.noises = noises;
    p.update(0.1, &scene, &mut host);
    p.known(ME)
        .into_iter()
        .filter(|k| k.sensed && k.sense == Sense::Hearing)
        .map(|k| (k.subject, k.strength))
        .collect()
}

#[test]
fn hearing_respects_noise_range_listener_range_loudness_and_threshold() {
    // (noise distance, loudness, noise range, expected strength or none)
    let cases = [
        (0.0, 1.0, 10.0, Some(1.0)),
        (5.0, 1.0, 10.0, Some(0.5)),
        (5.0, 0.5, 10.0, Some(0.25)),
        (10.0, 1.0, 10.0, None), // strength 0 at the edge
        (10.5, 1.0, 10.0, None),
        (9.8, 1.0, 10.0, None), // 0.02 is below the 0.05 threshold
        (25.0, 1.0, 100.0, Some(1.0 / 6.0)), // reach is capped at the listener's 30
        (31.0, 1.0, 100.0, None), // beyond this listener's own range of 30
    ];
    for (distance, loudness, range, expected) in cases {
        let heard = hear(
            hearing_sensor(1),
            vec![noise(Some(9), distance, loudness, range, 2)],
            None,
        );
        match expected {
            Some(strength) => {
                assert_eq!(heard.len(), 1, "{distance} {loudness} {range}");
                assert!((heard[0].1 - strength).abs() < 1e-4, "{heard:?}");
            }
            None => assert!(heard.is_empty(), "{distance} {loudness} {range}: {heard:?}"),
        }
    }
}

#[test]
fn the_attenuation_hook_scales_loudness_and_anonymous_noises_share_a_subject() {
    let loud = noise(Some(9), 5.0, 1.0, 10.0, 2);
    let full = hear(hearing_sensor(1), vec![loud], None);
    let half = hear(hearing_sensor(1), vec![loud], Some(0.5));
    assert!((full[0].1 - 0.5).abs() < 1e-4 && (half[0].1 - 0.25).abs() < 1e-4);
    assert!(hear(hearing_sensor(1), vec![loud], Some(0.0)).is_empty());
    // Two anonymous noises collapse to the stronger one; a noise of the listener itself is ignored.
    let heard = hear(
        hearing_sensor(1),
        vec![
            noise(None, 8.0, 1.0, 10.0, 0),
            noise(None, 2.0, 1.0, 10.0, 0),
            noise(Some(ME), 1.0, 1.0, 10.0, 2),
        ],
        None,
    );
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].0, ANONYMOUS);
    assert!((heard[0].1 - 0.8).abs() < 1e-4);
}

#[test]
fn damage_reveals_the_attacker_to_the_victim() {
    let config = SensorConfig {
        damage: Some(DamageConfig::default()),
        ..SensorConfig::default()
    };
    let mut p = Perception::default();
    let mut host = Host::default();
    let mut scene = input(config.clone(), vec![]);
    scene.damages = vec![Damage {
        victim: ME,
        instigator: Some(7),
        position: [3.0, 0.0, 4.0],
        amount: 12.0,
        team: 2,
    }];
    let report = p.update(0.1, &scene, &mut host);
    assert_eq!(report.events.len(), 1);
    assert_eq!(report.events[0].sense, Sense::Damage);
    assert_eq!(report.events[0].subject, 7);
    let known = p.known(ME);
    assert_eq!(known[0].position, [3.0, 0.0, 4.0]);
    // Damage taken by someone else is not ours to know.
    let mut other = input(config, vec![]);
    other.damages = vec![Damage {
        victim: 99,
        instigator: Some(7),
        position: [0.0; 3],
        amount: 1.0,
        team: 2,
    }];
    let mut fresh = Perception::default();
    assert!(fresh.update(0.1, &other, &mut host).events.is_empty());
}

#[test]
fn team_relations_decide_which_stimuli_a_sense_reacts_to() {
    let mut teams = TeamTable::default();
    teams.set(1, 3, Affiliation::Neutral);
    teams.set(1, 4, Affiliation::Friendly);
    // (target team, filter, sensed): default relations are hostile between teams,
    // friendly within a team, neutral with team 0.
    let cases = [
        (2, AffiliationFilter::HOSTILE, true),
        (1, AffiliationFilter::HOSTILE, false),
        (0, AffiliationFilter::HOSTILE, false),
        (3, AffiliationFilter::HOSTILE, false),
        (4, AffiliationFilter::HOSTILE, false),
        (
            3,
            AffiliationFilter {
                hostile: false,
                neutral: true,
                friendly: false,
            },
            true,
        ),
        (
            0,
            AffiliationFilter {
                hostile: false,
                neutral: true,
                friendly: false,
            },
            true,
        ),
        (
            1,
            AffiliationFilter {
                hostile: false,
                neutral: false,
                friendly: true,
            },
            true,
        ),
        (
            4,
            AffiliationFilter {
                hostile: false,
                neutral: false,
                friendly: true,
            },
            true,
        ),
        (
            2,
            AffiliationFilter {
                hostile: false,
                neutral: false,
                friendly: true,
            },
            false,
        ),
        (2, AffiliationFilter::ALL, true),
    ];
    for (team, filter, expected) in cases {
        let mut config = sensor(1);
        config.sight.as_mut().unwrap().affiliations = filter;
        let mut p = Perception::new(teams.clone());
        let mut host = Host::default();
        p.update(0.1, &input(config, vec![at(2, 5.0, 0.0, team)]), &mut host);
        assert_eq!(
            sensed_subjects(&p) == [2],
            expected,
            "team {team} {filter:?}"
        );
    }
    // Filters are per sense: sight ignores a friendly noise maker that hearing accepts.
    let mut config = sensor(1);
    config.hearing = Some(HearingConfig {
        affiliations: AffiliationFilter::ALL,
        ..HearingConfig::default()
    });
    let mut p = Perception::new(teams);
    let mut scene = input(config, vec![at(2, 5.0, 0.0, 4)]);
    scene.noises = vec![noise(Some(2), 5.0, 1.0, 20.0, 4)];
    p.update(0.1, &scene, &mut Host::default());
    let senses: Vec<_> = p.known(ME).into_iter().map(|k| k.sense).collect();
    assert_eq!(senses, [Sense::Hearing]);
}

/// Updates until the (single) record of the subject is forgotten; returns how many
/// updates after the last sensing that took.
fn updates_until_forgotten(dt: f32, max_age: f32, hearing: bool) -> u32 {
    let mut config = sensor(1);
    config.sight.as_mut().unwrap().max_age = max_age;
    config.hearing = Some(HearingConfig {
        max_age,
        affiliations: AffiliationFilter::ALL,
        ..HearingConfig::default()
    });
    let mut p = Perception::default();
    let mut host = Host::default();
    let mut scene = if hearing {
        input(config.clone(), vec![])
    } else {
        input(config.clone(), vec![at(2, 5.0, 0.0, 2)])
    };
    if hearing {
        scene.noises = vec![noise(Some(2), 1.0, 1.0, 20.0, 2)];
    }
    p.update(dt, &scene, &mut host);
    assert_eq!(p.record_count(ME), 1);
    // The stimulus is gone from here on.
    let empty = input(config, vec![]);
    for n in 1..100_000 {
        let report = p.update(dt, &empty, &mut host);
        if report.events.iter().any(|e| e.kind == EventKind::Forgotten) {
            assert_eq!(p.record_count(ME), 0);
            return n;
        }
        assert_eq!(p.record_count(ME), 1);
    }
    panic!("never forgotten");
}

#[test]
fn memory_is_forgotten_at_max_age_for_both_tick_rates() {
    // (dt, max_age, updates after the last sensing)
    let cases: [(f32, f32, u32); 6] = [
        (1.0 / 60.0, 1.0, 60),
        (1.0 / 60.0, 0.5, 30),
        (1.0 / 60.0, 5.0, 300),
        (0.1, 1.0, 10),
        (0.1, 0.25, 3),
        (0.1, 5.0, 50),
    ];
    for (dt, max_age, expected) in cases {
        for hearing in [false, true] {
            assert_eq!(
                updates_until_forgotten(dt, max_age, hearing),
                expected,
                "dt {dt} max_age {max_age} hearing {hearing}"
            );
        }
    }
    // max_age 0 forgets as soon as the stimulus stops.
    assert_eq!(updates_until_forgotten(0.1, 0.0, false), 1);
}

#[test]
fn remembered_strength_decays_linearly_and_regaining_restores_it() {
    let mut p = Perception::default();
    let mut host = Host::default();
    p.update(0.1, &input(sensor(1), vec![at(2, 5.0, 0.0, 2)]), &mut host);
    let empty = || input(sensor(1), vec![]);
    for _ in 0..5 {
        p.update(0.1, &empty(), &mut host);
    }
    let known = p.known(ME)[0];
    assert!(!known.sensed);
    assert!((known.age - 0.5).abs() < 1e-4);
    assert!(
        (known.strength - 0.5).abs() < 1e-4,
        "half way to max_age 1.0"
    );
    assert_eq!(
        known.position,
        at(2, 5.0, 0.0, 2).position,
        "last known position"
    );
    let report = p.update(0.1, &input(sensor(1), vec![at(2, 6.0, 0.0, 2)]), &mut host);
    assert_eq!(report.events.len(), 1);
    assert_eq!(report.events[0].kind, EventKind::Sensed);
    let known = p.known(ME)[0];
    assert!(known.sensed && known.age == 0.0 && known.strength == 1.0);
}

#[test]
fn the_best_target_prefers_sensed_then_strong_then_fresh_then_low_ids() {
    let mut config = sensor(1);
    config.hearing = Some(HearingConfig {
        affiliations: AffiliationFilter::ALL,
        ..HearingConfig::default()
    });
    let mut p = Perception::default();
    let mut host = Host::default();
    let mut scene = input(
        config.clone(),
        vec![at(5, 5.0, 0.0, 2), at(4, 5.0, 10.0, 2)],
    );
    scene.noises = vec![noise(Some(9), 1.0, 1.0, 40.0, 3)];
    p.update(0.1, &scene, &mut host);
    let best = p.best(ME, AffiliationFilter::ALL).unwrap();
    assert_eq!(
        (best.subject, best.sense),
        (4, Sense::Sight),
        "tie: lowest subject id"
    );
    assert_eq!(p.best(ME, AffiliationFilter::HOSTILE).unwrap().subject, 4);
    // Subject 4 leaves; 5 stays in view and is sensed, 9 is merely remembered.
    let mut scene = input(config.clone(), vec![at(5, 5.0, 0.0, 2)]);
    p.update(0.1, &scene, &mut host);
    assert_eq!(p.best(ME, AffiliationFilter::ALL).unwrap().subject, 5);
    // Everything leaves: the strongest remembered one wins.
    scene.targets.clear();
    p.update(0.1, &scene, &mut host);
    assert!(!p.best(ME, AffiliationFilter::ALL).unwrap().sensed);
    let none = AffiliationFilter {
        hostile: false,
        neutral: false,
        friendly: true,
    };
    assert!(p.best(ME, none).is_none());
    assert!(p.best(77, AffiliationFilter::ALL).is_none());
}

#[test]
fn memory_is_bounded_and_dropped_with_its_perceiver() {
    let mut p = Perception::default();
    let mut host = Host::default();
    // 100 distinct noise sources in one update exceed the record cap.
    let mut scene = input(hearing_sensor(1), vec![]);
    scene.noises = (0..100)
        .map(|i| noise(Some(100 + i), 1.0, 1.0, 20.0, 2))
        .collect();
    let report = p.update(0.1, &scene, &mut host);
    assert_eq!(p.record_count(ME), MAX_RECORDS);
    assert_eq!(
        report
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Sensed)
            .count(),
        MAX_RECORDS,
        "records sensed now are never evicted for newer ones"
    );
    assert_eq!(p.perceiver_count(), 1);
    p.update(0.1, &UpdateInput::default(), &mut host);
    assert_eq!(p.perceiver_count(), 0, "no input perceiver, no memory");
}

fn trace() -> Vec<UpdateReport> {
    let mut p = Perception::default();
    let mut host = Host::default();
    let mut config = sensor(1);
    config.hearing = Some(HearingConfig::default());
    let mut reports = Vec::new();
    for step in 0..120u32 {
        let phase = step as f32 * 0.1;
        let targets: Vec<TargetInput> = (2..8)
            .map(|id| {
                at(
                    id,
                    4.0 + (id as f32 + phase).sin() * 6.0,
                    (phase * 20.0 + id as f32 * 15.0) % 120.0 - 60.0,
                    id as u32 % 3,
                )
            })
            .collect();
        let mut scene = input(config.clone(), targets);
        scene.max_rays = 1;
        if step % 7 == 0 {
            scene.noises = vec![noise(Some(20 + (step % 3) as Id), 8.0, 1.0, 30.0, 2)];
        }
        host.blocked = (2..8u64)
            .filter(|id| (u64::from(step / 10) + id) % 3 == 0)
            .collect();
        reports.push(p.update(1.0 / 30.0, &scene, &mut host));
    }
    reports
}

#[test]
fn two_runs_give_identical_event_traces() {
    let first = trace();
    assert_eq!(first, trace());
    assert!(first.iter().any(|r| !r.events.is_empty()));
    assert!(first.iter().any(|r| r.rays_deferred > 0));
}

#[test]
fn configs_validate_their_bounds() {
    assert!(sight().validate().is_ok());
    let bad = |edit: fn(&mut SightConfig)| {
        let mut config = sight();
        edit(&mut config);
        config.validate().is_err()
    };
    assert!(bad(|c| c.range = 0.0));
    assert!(bad(|c| c.range = f32::NAN));
    assert!(bad(|c| c.lose_range = 5.0));
    assert!(bad(|c| c.half_angle = 200.0));
    assert!(bad(|c| c.peripheral_half_angle = 30.0));
    assert!(bad(|c| c.max_age = -1.0));
    assert!(bad(|c| c.height_offset = 5000.0));
    assert!(HearingConfig::default().validate().is_ok());
    assert!(
        HearingConfig {
            threshold: 2.0,
            ..HearingConfig::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        DamageConfig {
            max_age: 4000.0,
            ..DamageConfig::default()
        }
        .validate()
        .is_err()
    );
}
