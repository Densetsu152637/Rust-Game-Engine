use engine_audio::{
    AudioConfig, AudioEngine, AudioSources, PlayOptions, ProceduralSourceConfig, SourceAction,
    SourceStreamConfig, VoiceState,
    graph::{self, ParameterUpdate},
};
use engine_graph::{GraphDocument, GraphEdge, GraphNode, GraphVariable, PortRef, PortType};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
fn graph() -> std::sync::Arc<graph::CompiledGraph> {
    let mut d = GraphDocument::new("audio");
    d.nodes = vec![
        GraphNode {
            id: "v".into(),
            node_type: "parameter".into(),
            ports: vec![],
            params: BTreeMap::from([("variable".into(), "amplitude".into())]),
            position: None,
        },
        GraphNode {
            id: "out".into(),
            node_type: "output".into(),
            ports: vec![],
            params: BTreeMap::new(),
            position: None,
        },
    ];
    d.variables = vec![GraphVariable {
        name: "amplitude".into(),
        ty: PortType::Float,
        default: Some(0.25.into()),
        description: String::new(),
    }];
    for channel in ["left", "right"] {
        d.edges.push(GraphEdge {
            from: PortRef::new("v", "signal"),
            to: PortRef::new("out", channel),
        });
    }
    graph::compile(&d).unwrap()
}
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone.mp3")
}
fn until(mut test: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !test() {
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn procedural_sources_share_handles_observers_initial_controls_and_restart_cleanup() {
    let mut e = AudioEngine::offline(AudioConfig {
        sample_rate: 8000,
        max_voices: 1,
    })
    .unwrap();
    let mut sources = AudioSources::new(e.control(), "owner").unwrap();
    let source = sources
        .create_procedural(
            graph(),
            PlayOptions::default(),
            ProceduralSourceConfig {
                queue_frames: 4,
                duration_frames: Some(32),
                parameters: BTreeMap::from([("amplitude".into(), 0.5)]),
            },
        )
        .unwrap();
    sources
        .observe("start", source, SourceAction::Play)
        .unwrap();
    sources.observe("stop", source, SourceAction::Stop).unwrap();
    assert_eq!(sources.state(source), VoiceState::Stopped);
    sources.notify("start").unwrap();
    let first = sources.voice(source).unwrap();
    until(|| {
        sources
            .stream_snapshot(source)
            .unwrap()
            .unwrap()
            .queued_frames
            == 4
    });
    let mut out = [0.; 2];
    e.mix(&mut out).unwrap();
    assert_eq!(out, [0.5; 2]);
    sources.control(source, SourceAction::Pause).unwrap();
    assert_eq!(sources.state(source), VoiceState::Paused);
    sources.notify("start").unwrap();
    assert_ne!(sources.voice(source), Some(first));
    assert_eq!(e.voice_count(), 1);
    sources.control(source, SourceAction::Pause).unwrap();
    sources
        .set_parameters(
            source,
            &[ParameterUpdate {
                name: "amplitude",
                value: 0.75,
                ramp_frames: 0,
            }],
        )
        .unwrap();
    let next = sources.seek(source, 0).unwrap();
    until(|| {
        sources
            .stream_snapshot(source)
            .unwrap()
            .unwrap()
            .queued_frames
            == 4
    });
    assert_eq!(
        sources.stream_snapshot(source).unwrap().unwrap().generation,
        next
    );
    sources.control(source, SourceAction::Resume).unwrap();
    e.mix(&mut out).unwrap();
    assert_eq!(out, [0.75; 2]);
    sources.notify("stop").unwrap();
    assert_eq!(e.voice_count(), 0);
    assert!(sources.contains(source));
}
#[test]
fn decoded_sources_seek_pause_drop_and_reject_looping_before_opening_files() {
    let e = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut sources = AudioSources::new(e.control(), "owner").unwrap();
    let invalid = sources.create_stream(
        "does-not-exist",
        PlayOptions {
            looped: true,
            ..Default::default()
        },
        SourceStreamConfig::default(),
    );
    assert_eq!(invalid.unwrap_err().code, "audio.source.loop");
    let source = sources
        .create_stream(
            fixture(),
            PlayOptions::default(),
            SourceStreamConfig {
                queue_frames: 4,
                ..Default::default()
            },
        )
        .unwrap();
    sources.control(source, SourceAction::Play).unwrap();
    until(|| {
        sources
            .stream_snapshot(source)
            .unwrap()
            .unwrap()
            .queued_frames
            == 4
    });
    sources.control(source, SourceAction::Pause).unwrap();
    let generation = sources.seek(source, 1000).unwrap();
    assert_eq!(
        sources.stream_snapshot(source).unwrap().unwrap().generation,
        generation
    );
    assert_eq!(
        sources
            .stream_snapshot(source)
            .unwrap()
            .unwrap()
            .source_frame,
        1000.
    );
    assert!(
        sources
            .configure(
                source,
                PlayOptions {
                    looped: true,
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert_eq!(sources.state(source), VoiceState::Paused);
    drop(sources);
    assert_eq!(e.voice_count(), 0);
}
#[test]
fn finished_producers_refresh_to_reusable_stopped_sources() {
    let mut e = AudioEngine::offline(AudioConfig {
        sample_rate: 8000,
        max_voices: 1,
    })
    .unwrap();
    let mut sources = AudioSources::new(e.control(), "owner").unwrap();
    let source = sources
        .create_procedural(
            graph(),
            PlayOptions::default(),
            ProceduralSourceConfig {
                queue_frames: 2,
                duration_frames: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
    sources.control(source, SourceAction::Play).unwrap();
    until(|| {
        sources
            .stream_snapshot(source)
            .unwrap()
            .unwrap()
            .queued_frames
            == 2
    });
    e.mix(&mut [0.; 8]).unwrap();
    for _ in 0..100 {
        e.mix(&mut [0.; 8]).unwrap();
        if sources.state(source) == VoiceState::Stopped {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    sources.refresh();
    assert!(sources.voice(source).is_none());
    sources
        .set_parameters(
            source,
            &[ParameterUpdate {
                name: "amplitude",
                value: 0.8,
                ramp_frames: 0,
            }],
        )
        .unwrap();
    sources.control(source, SourceAction::Play).unwrap();
    until(|| {
        sources
            .stream_snapshot(source)
            .unwrap()
            .unwrap()
            .queued_frames
            == 2
    });
    let mut out = [0.; 2];
    e.mix(&mut out).unwrap();
    assert_eq!(out, [0.8; 2]);
}

#[test]
fn asynchronous_dsp_failure_survives_worker_refresh_as_a_coded_source_error() {
    let d=GraphDocument::from_json(br#"{"version":1,"kind":"audio","nodes":[{"id":"osc","node_type":"oscillator","params":{"frequency":4000}},{"id":"out","node_type":"output"}],"edges":[{"from":{"node":"osc","port":"signal"},"to":{"node":"out","port":"left"}},{"from":{"node":"osc","port":"signal"},"to":{"node":"out","port":"right"}}]}"#).unwrap();
    let mut e = AudioEngine::offline(AudioConfig {
        sample_rate: 8000,
        max_voices: 1,
    })
    .unwrap();
    let mut sources = AudioSources::new(e.control(), "owner").unwrap();
    let source = sources
        .create_procedural(
            graph::compile(&d).unwrap(),
            PlayOptions::default(),
            ProceduralSourceConfig::default(),
        )
        .unwrap();
    sources.control(source, SourceAction::Play).unwrap();
    until(|| sources.failure(source).is_some());
    assert_eq!(
        sources.failure(source).unwrap().code,
        "audio.graph.frequency"
    );
    let mut out = [1.; 8];
    e.mix(&mut out).unwrap();
    assert_eq!(out, [0.; 8]);
    sources.refresh();
    assert!(sources.voice(source).is_none());
    assert_eq!(
        sources.failure(source).unwrap().code,
        "audio.graph.frequency"
    );
    assert!(sources.contains(source));
}
