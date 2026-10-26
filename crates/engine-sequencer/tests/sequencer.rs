use engine_sequencer::*;
use std::sync::Arc;
fn player() -> Player {
    let timeline = Timeline {
        duration: 1.,
        events: vec![
            Marker {
                time: 0.,
                id: "start".into(),
            },
            Marker {
                time: 0.5,
                id: "a".into(),
            },
            Marker {
                time: 0.5,
                id: "b".into(),
            },
            Marker {
                time: 1.,
                id: "end".into(),
            },
        ],
        ..Default::default()
    };
    Player::new(Arc::new(timeline.compile(1).unwrap()))
}
fn ids(events: &[EventCrossing]) -> Vec<&str> {
    events.iter().map(|e| e.marker.id.as_str()).collect()
}
#[test]
fn exact_keys_hold_step_interpolate_vectors_rotations_and_fov() {
    let doc = Timeline {
        duration: 2.,
        tracks: vec![
            Track::Translation {
                binding: 0,
                channel: Channel {
                    sampling: Sampling::Linear,
                    keys: vec![
                        Key {
                            time: 0.,
                            value: [0.; 3],
                        },
                        Key {
                            time: 2.,
                            value: [2., 4., 6.],
                        },
                    ],
                },
            },
            Track::Rotation {
                binding: 0,
                channel: Channel {
                    sampling: Sampling::Linear,
                    keys: vec![
                        Key {
                            time: 0.,
                            value: [0., 0., 0., 1.],
                        },
                        Key {
                            time: 2.,
                            value: [0., 1., 0., 0.],
                        },
                    ],
                },
            },
            Track::Scale {
                binding: 0,
                channel: Channel {
                    sampling: Sampling::Step,
                    keys: vec![
                        Key {
                            time: 0.,
                            value: [1.; 3],
                        },
                        Key {
                            time: 2.,
                            value: [3.; 3],
                        },
                    ],
                },
            },
            Track::CameraFov {
                binding: 1,
                channel: Channel {
                    sampling: Sampling::Linear,
                    keys: vec![
                        Key {
                            time: 0.,
                            value: 1.,
                        },
                        Key {
                            time: 2.,
                            value: 2.,
                        },
                    ],
                },
            },
        ],
        camera_cuts: vec![CameraCut {
            time: 0.5,
            binding: 1,
        }],
        ..Default::default()
    };
    let compiled = doc.compile(2).unwrap();
    let samples = compiled.sample(1.).unwrap();
    assert_eq!(samples[0].value, Value::Vector([1., 2., 3.]));
    if let Value::Rotation(value) = samples[1].value {
        let rotation = glam::Quat::from_array(value);
        assert!((rotation.length() - 1.).abs() < 1e-6);
        assert!((rotation * glam::Vec3::X).z.abs() > 0.999);
    } else {
        panic!("rotation sample");
    }
    assert_eq!(samples[2].value, Value::Vector([1.; 3]));
    assert_eq!(samples[3].value, Value::Scalar(1.5));
    assert_eq!(
        compiled.sample(2.).unwrap()[2].value,
        Value::Vector([3.; 3])
    );
    assert_eq!(compiled.camera_at(0.), None);
    assert_eq!(compiled.camera_at(0.5), Some(1));
}
#[test]
fn deterministic_pause_seek_reverse_and_tie_order() {
    let mut p = player();
    p.play();
    assert_eq!(ids(&p.advance(0.5).unwrap()), ["a", "b"]);
    p.pause();
    assert!(p.advance(4.).unwrap().is_empty());
    assert_eq!(p.position(), 0.5);
    assert!(p.seek(1., SeekEvents::Silent).unwrap().is_empty());
    p.set_speed(-1.).unwrap();
    p.play();
    let events = p.advance(0.5).unwrap();
    assert_eq!(ids(&events), ["a", "b"]);
    assert!(events.iter().all(|e| e.direction == Direction::Reverse));
    assert_eq!(ids(&p.seek(0., SeekEvents::Crossed).unwrap()), ["start"]);
    p.stop();
    assert_eq!(p.state(), State::Stopped);
    assert_eq!(p.position(), 0.);
}
#[test]
fn seams_emit_both_endpoints_once_and_exclude_departure() {
    let mut p = player();
    p.set_looping(true);
    p.play();
    let events = p.advance(1.).unwrap();
    assert_eq!(ids(&events), ["a", "b", "end", "start"]);
    assert_eq!(events.last().unwrap().cycle, 1);
    assert_eq!(p.position(), 0.);
    assert!(p.advance(0.25).unwrap().is_empty());
    p.set_speed(-1.).unwrap();
    let events = p.advance(0.25).unwrap();
    assert_eq!(ids(&events), ["start", "end"]);
    assert_eq!(p.position(), 1.);
    assert_eq!(ids(&p.advance(0.5).unwrap()), ["a", "b"]);
}
#[test]
fn reject_large_delta_and_seek_without_partial_state_change() {
    let mut p = player();
    p.set_looping(true);
    p.play();
    assert!(p.advance(65.).is_err());
    assert_eq!(p.position(), 0.);
    assert_eq!(p.cycle(), 0);
    assert!(p.seek(f64::NAN, SeekEvents::Crossed).is_err());
    assert_eq!(p.position(), 0.);
    assert!(p.set_speed(0.).is_err());
    assert_eq!(p.speed(), 1.);
    assert_eq!(ids(&p.advance(0.5).unwrap()), ["a", "b"]);
}
#[test]
fn event_capacity_rejection_is_atomic_and_nonloop_large_delta_is_bounded() {
    let doc = Timeline {
        events: (0..1024)
            .map(|i| Marker {
                time: 0.5,
                id: i.to_string(),
            })
            .collect(),
        ..Default::default()
    };
    let mut p = Player::new(Arc::new(doc.compile(1).unwrap()));
    p.set_looping(true);
    p.play();
    assert!(p.advance(5.).is_err());
    assert_eq!(p.position(), 0.);
    assert_eq!(p.state(), State::Playing);
    p.set_looping(false);
    assert_eq!(p.advance(1e100).unwrap().len(), 1024);
    assert_eq!(p.state(), State::Finished);
}
#[test]
fn invalid_targets_times_and_values_are_rejected_before_playback() {
    let mut doc = Timeline::default();
    doc.tracks.push(Track::Translation {
        binding: 0,
        channel: Channel {
            sampling: Sampling::Linear,
            keys: vec![Key {
                time: 0.,
                value: [f32::NAN, 0., 0.],
            }],
        },
    });
    assert!(doc.compile(1).is_err());
    doc.tracks = vec![Track::Rotation {
        binding: 0,
        channel: Channel {
            sampling: Sampling::Linear,
            keys: vec![Key {
                time: 0.,
                value: [0.; 4],
            }],
        },
    }];
    assert!(doc.compile(1).is_err());
    doc.tracks.clear();
    doc.camera_cuts.push(CameraCut {
        time: 0.,
        binding: 2,
    });
    assert!(doc.compile(1).is_err());
}
#[test]
fn signed_zero_markers_keep_author_order_at_loop_seams() {
    let doc = Timeline {
        events: vec![
            Marker {
                time: 0.0,
                id: "first".into(),
            },
            Marker {
                time: -0.0,
                id: "second".into(),
            },
        ],
        ..Default::default()
    };
    let mut p = Player::new(Arc::new(doc.compile(1).unwrap()));
    p.set_looping(true);
    p.play();
    assert_eq!(ids(&p.advance(1.).unwrap()), ["first", "second"]);
    p.set_speed(-1.).unwrap();
    p.seek(0.5, SeekEvents::Silent).unwrap();
    assert_eq!(ids(&p.advance(0.5).unwrap()), ["first", "second"]);
}
