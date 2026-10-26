use engine_audio::{
    AudioConfig, AudioEngine, PlayOptions,
    graph::{self, GraphInstance, ParameterUpdate, ProceduralState, ProceduralStream},
};
use engine_graph::{GraphDocument, GraphEdge, GraphNode, GraphVariable, PortRef, PortType};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
fn node(id: &str, kind: &str, params: &[(&str, f64)]) -> GraphNode {
    GraphNode {
        id: id.into(),
        node_type: kind.into(),
        ports: vec![],
        params: params
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect(),
        position: None,
    }
}
fn edge(document: &mut GraphDocument, from: &str, to: &str, port: &str) {
    document.edges.push(GraphEdge {
        from: PortRef::new(from, "signal"),
        to: PortRef::new(to, port),
    });
}
fn base(source: GraphNode) -> GraphDocument {
    let mut d = GraphDocument::new("audio");
    let id = source.id.as_str().to_owned();
    d.nodes = vec![source, node("out", "output", &[])];
    edge(&mut d, &id, "out", "left");
    edge(&mut d, &id, "out", "right");
    d
}
fn instance(d: &GraphDocument) -> GraphInstance {
    GraphInstance::new(graph::compile(d).unwrap(), 8000).unwrap()
}
fn parameter(d: &mut GraphDocument, id: &str, name: &str, value: f64) {
    d.variables.push(GraphVariable {
        name: name.into(),
        ty: PortType::Float,
        default: Some(value.into()),
        description: String::new(),
    });
    let mut n = node(id, "parameter", &[]);
    n.params.insert("variable".into(), name.into());
    d.nodes.push(n);
}
fn until(mut condition: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < limit, "producer timed out");
        thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn sine_frequency_chunking_and_independent_instances() {
    let d = base(node("osc", "oscillator", &[("frequency", 1000.)]));
    let program = graph::compile(&d).unwrap();
    let mut a = GraphInstance::new(program.clone(), 8000).unwrap();
    let mut b = GraphInstance::new(program, 8000).unwrap();
    let mut out = [[0.; 2]; 800];
    a.render(&mut out).unwrap();
    let crossings = out
        .windows(2)
        .filter(|p| p[0][0] <= 0. && p[1][0] > 0.)
        .count();
    assert_eq!(crossings, 100);
    let mut chunks = [[0.; 2]; 800];
    for chunk in chunks.chunks_mut(13) {
        b.render(chunk).unwrap();
    }
    assert_eq!(out, chunks);
    a.reset();
    let mut reset = [[0.; 2]; 8];
    a.render(&mut reset).unwrap();
    assert_eq!(&reset, &out[..8]);
    assert_eq!(b.generated_frames(), 800);
}
#[test]
fn controls_ramp_atomically_and_instances_do_not_share_state() {
    let mut d = base(node("v", "parameter", &[]));
    d.nodes[0].params.insert("variable".into(), "volume".into());
    d.variables.push(GraphVariable {
        name: "volume".into(),
        ty: PortType::Float,
        default: Some(0.0.into()),
        description: String::new(),
    });
    let program = graph::compile(&d).unwrap();
    let mut a = GraphInstance::new(program.clone(), 8000).unwrap();
    let mut b = GraphInstance::new(program, 8000).unwrap();
    a.set_parameters(&[ParameterUpdate {
        name: "volume",
        value: 1.,
        ramp_frames: 4,
    }])
    .unwrap();
    let mut out = [[0.; 2]; 4];
    a.render(&mut out).unwrap();
    assert_eq!(out, [[0.25; 2], [0.5; 2], [0.75; 2], [1.; 2]]);
    assert!(
        a.set_parameters(&[
            ParameterUpdate {
                name: "volume",
                value: 0.,
                ramp_frames: 0
            },
            ParameterUpdate {
                name: "missing",
                value: 1.,
                ramp_frames: 0
            }
        ])
        .is_err()
    );
    a.render(&mut out[..1]).unwrap();
    assert_eq!(out[0], [1.; 2]);
    b.render(&mut out).unwrap();
    assert_eq!(out, [[0.; 2]; 4]);
    assert!(
        a.set_parameters(&[ParameterUpdate {
            name: "volume",
            value: f32::NAN,
            ramp_frames: 0
        }])
        .is_err()
    );
    assert!(
        a.set_parameters(&[ParameterUpdate {
            name: "volume",
            value: 1.,
            ramp_frames: 65537
        }])
        .is_err()
    );
}
#[test]
fn mix_gain_and_seeded_noise_produce_real_bounded_pcm() {
    let mut d = base(node("sum", "mix", &[]));
    d.nodes.push(node("a", "constant", &[("value", 0.25)]));
    d.nodes.push(node("b", "constant", &[("value", 0.5)]));
    edge(&mut d, "a", "sum", "a");
    edge(&mut d, "b", "sum", "b");
    let mut i = instance(&d);
    let mut out = [[0.; 2]; 8];
    i.render(&mut out).unwrap();
    assert_eq!(out, [[0.75; 2]; 8]);
    let mut noise = base(node("noise", "noise", &[]));
    noise.nodes.push(node("gain", "gain", &[("gain", 0.1)]));
    noise.edges.clear();
    edge(&mut noise, "noise", "gain", "input");
    edge(&mut noise, "gain", "out", "left");
    edge(&mut noise, "gain", "out", "right");
    let mut a = instance(&noise);
    let mut b = instance(&noise);
    let mut x = [[0.; 2]; 1024];
    let mut y = x;
    a.render(&mut x).unwrap();
    b.render(&mut y).unwrap();
    assert_eq!(x, y);
    let energy = x.iter().map(|v| v[0] * v[0]).sum::<f32>() / 1024.;
    assert!((0.002..0.005).contains(&energy));
    assert!(x.iter().all(|v| v[0].abs() <= 0.1));
}
#[test]
fn adsr_has_exact_attack_decay_sustain_and_release() {
    let mut d = base(node(
        "env",
        "adsr",
        &[
            ("attack", 0.0005),
            ("decay", 0.0005),
            ("sustain", 0.5),
            ("release", 0.0005),
        ],
    ));
    parameter(&mut d, "gate", "gate", 1.);
    edge(&mut d, "gate", "env", "gate");
    let mut i = instance(&d);
    let mut out = [[0.; 2]; 10];
    i.render(&mut out).unwrap();
    assert_eq!(out[..4], [[0.25; 2], [0.5; 2], [0.75; 2], [1.; 2]]);
    assert_eq!(out[4..8], [[0.875; 2], [0.75; 2], [0.625; 2], [0.5; 2]]);
    assert_eq!(out[8..], [[0.5; 2]; 2]);
    i.set_parameters(&[ParameterUpdate {
        name: "gate",
        value: 0.,
        ramp_frames: 0,
    }])
    .unwrap();
    i.render(&mut out[..5]).unwrap();
    assert_eq!(
        out[..5],
        [[0.375; 2], [0.25; 2], [0.125; 2], [0.; 2], [0.; 2]]
    );
}
#[test]
fn delay_feedback_and_lowpass_match_signal_equations() {
    let mut d = base(node(
        "delay",
        "delay",
        &[("seconds", 0.00025), ("feedback", 0.5)],
    ));
    parameter(&mut d, "impulse", "sample", 1.);
    edge(&mut d, "impulse", "delay", "input");
    let mut i = instance(&d);
    let mut out = [[0.; 2]; 7];
    i.render(&mut out[..1]).unwrap();
    i.set_parameters(&[ParameterUpdate {
        name: "sample",
        value: 0.,
        ramp_frames: 0,
    }])
    .unwrap();
    i.render(&mut out[1..]).unwrap();
    assert_eq!(
        out,
        [
            [0.; 2], [0.; 2], [1.; 2], [0.; 2], [0.5; 2], [0.; 2], [0.25; 2]
        ]
    );
    let mut d = base(node("filter", "lowpass", &[("cutoff", 1000.)]));
    d.nodes.push(node("step", "constant", &[("value", 1.)]));
    edge(&mut d, "step", "filter", "input");
    let mut i = instance(&d);
    let mut out = [[0.; 2]; 64];
    i.render(&mut out).unwrap();
    let a = 1. - (-std::f32::consts::TAU * 1000. / 8000.).exp();
    assert!((out[0][0] - a).abs() < 1e-6);
    assert!(out.windows(2).all(|p| p[1][0] >= p[0][0]));
    assert!((out[63][0] - 1.).abs() < 1e-6);
}
#[test]
fn rejects_cycles_unused_nodes_math_and_storage_limits() {
    let mut cycle = base(node("a", "gain", &[]));
    edge(&mut cycle, "a", "a", "input");
    assert!(graph::compile(&cycle).is_err());
    let mut unused = base(node("a", "constant", &[]));
    unused.nodes.push(node("unused", "noise", &[]));
    assert!(graph::compile(&unused).is_err());
    let mut bad = base(node("osc", "oscillator", &[]));
    parameter(&mut bad, "hz", "frequency", -1.);
    edge(&mut bad, "hz", "osc", "frequency");
    let mut i = instance(&bad);
    let mut out = [[1.; 2]; 8];
    assert_eq!(
        i.render(&mut out).unwrap_err().code,
        "audio.graph.frequency"
    );
    assert_eq!(out, [[0.; 2]; 8]);
    assert!(i.render(&mut out).is_err());
    i.reset();
    assert!(
        i.set_parameters(&[ParameterUpdate {
            name: "frequency",
            value: 440.,
            ramp_frames: 0
        }])
        .is_ok()
    );
    assert!(i.render(&mut out).is_ok());
    let mut delay = base(node("delay0", "delay", &[("seconds", 2.)]));
    delay.nodes.push(node("source", "constant", &[]));
    edge(&mut delay, "source", "delay0", "input");
    let mut previous = "delay0".to_owned();
    for at in 1..4 {
        let id = format!("delay{at}");
        delay.nodes.push(node(&id, "delay", &[("seconds", 2.)]));
        edge(&mut delay, &previous, &id, "input");
        previous = id;
    }
    delay.edges.retain(|e| e.to.node.as_str() != "out");
    edge(&mut delay, &previous, "out", "left");
    edge(&mut delay, &previous, "out", "right");
    assert!(GraphInstance::new(graph::compile(&delay).unwrap(), 192000).is_err());
    let mut d = base(node("a", "constant", &[]));
    for at in 0..257 {
        d.nodes.push(node(&format!("n{at}"), "noise", &[]));
    }
    assert!(graph::compile(&d).is_err());
}
#[test]
fn producer_stream_pause_reset_updates_eof_clock_and_teardown() {
    let mut d = base(node("p", "parameter", &[]));
    d.nodes[0].params.insert("variable".into(), "value".into());
    d.variables.push(GraphVariable {
        name: "value".into(),
        ty: PortType::Float,
        default: Some(0.25.into()),
        description: String::new(),
    });
    let program = graph::compile(&d).unwrap();
    let mut e = AudioEngine::offline(AudioConfig {
        sample_rate: 8000,
        max_voices: 8,
    })
    .unwrap();
    let mut stream =
        ProceduralStream::spawn(program, e.control(), PlayOptions::default(), 4, Some(32)).unwrap();
    until(|| stream.status().unwrap().state == ProceduralState::Backpressure);
    assert!(e.pause(stream.voice()));
    let mut output = [0.; 8];
    e.mix(&mut output).unwrap();
    assert_eq!(
        e.stream_snapshot(stream.voice())
            .unwrap()
            .unwrap()
            .source_frame,
        0.
    );
    for at in 0..1000 {
        stream
            .set_parameters(&[ParameterUpdate {
                name: "value",
                value: (at % 2) as f32,
                ramp_frames: 0,
            }])
            .unwrap();
    }
    let generation = stream.reset().unwrap();
    stream
        .set_parameters(&[ParameterUpdate {
            name: "value",
            value: 0.5,
            ramp_frames: 0,
        }])
        .unwrap();
    until(|| {
        e.stream_snapshot(stream.voice())
            .unwrap()
            .unwrap()
            .queued_frames
            == 4
    });
    assert_eq!(
        e.stream_snapshot(stream.voice())
            .unwrap()
            .unwrap()
            .generation,
        generation
    );
    assert!(e.resume(stream.voice()));
    let mut audible_frames = 0;
    for _ in 0..100 {
        e.mix(&mut output).unwrap();
        audible_frames += output
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|f| f[0] != 0.)
            .count();
        if e.stream_snapshot(stream.voice()).unwrap().is_none() {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(audible_frames, 32);
    assert_eq!(stream.status().unwrap().generated_frames, 32);
    assert!(e.stream_snapshot(stream.voice()).unwrap().is_none());
    assert_eq!(stream.status().unwrap().state, ProceduralState::Finished);
    stream.stop().unwrap();
    assert!(stream.set_parameters(&[]).is_err());
    let program = graph::compile(&base(node("c", "constant", &[("value", 0.1)]))).unwrap();
    let stream = ProceduralStream::spawn(
        Arc::clone(&program),
        e.control(),
        PlayOptions::default(),
        2,
        None,
    )
    .unwrap();
    until(|| stream.status().unwrap().state == ProceduralState::Backpressure);
    drop(stream);
    assert_eq!(e.voice_count(), 0);
}

#[test]
fn parameter_updates_become_audible_with_bounded_lead_and_finish_drains() {
    let mut d = base(node("p", "parameter", &[]));
    d.nodes[0].params.insert("variable".into(), "value".into());
    d.variables.push(GraphVariable {
        name: "value".into(),
        ty: PortType::Float,
        default: Some(0.0.into()),
        description: String::new(),
    });
    let mut e = AudioEngine::offline(AudioConfig {
        sample_rate: 8000,
        max_voices: 8,
    })
    .unwrap();
    let stream = ProceduralStream::spawn(
        graph::compile(&d).unwrap(),
        e.control(),
        PlayOptions::default(),
        2,
        None,
    )
    .unwrap();
    until(|| stream.status().unwrap().state == ProceduralState::Backpressure);
    stream
        .set_parameters(&[ParameterUpdate {
            name: "value",
            value: 0.5,
            ramp_frames: 0,
        }])
        .unwrap();
    let mut heard = false;
    for _ in 0..600 {
        let mut out = [0.; 2];
        e.mix(&mut out).unwrap();
        if out[0] == 0.5 {
            assert!(
                e.stream_snapshot(stream.voice())
                    .unwrap()
                    .unwrap()
                    .source_frame
                    <= 258.
            );
            heard = true;
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(heard, "updated parameter never reached mixed PCM");
    stream.finish().unwrap();
    for _ in 0..600 {
        e.mix(&mut [0.; 8]).unwrap();
        if e.stream_snapshot(stream.voice()).unwrap().is_none() {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(stream.status().unwrap().state, ProceduralState::Finished);
    assert_eq!(e.voice_count(), 0);
}
#[test]
fn update_storm_and_intermediate_math_are_bounded() {
    let mut d = base(node("p", "parameter", &[]));
    d.nodes[0].params.insert("variable".into(), "value".into());
    d.variables.push(GraphVariable {
        name: "value".into(),
        ty: PortType::Float,
        default: Some(0.0.into()),
        description: String::new(),
    });
    let mut i = instance(&d);
    let one = ParameterUpdate {
        name: "value",
        value: 1.,
        ramp_frames: 0,
    };
    assert!(i.set_parameters(&[one; 65]).is_err());
    assert!(i.set_parameters(&[one; 2]).is_err());
    let mut oversized = vec![[1.; 2]; graph::MAX_RENDER_FRAMES + 1];
    assert_eq!(
        i.render(&mut oversized).unwrap_err().code,
        "audio.graph.block_limit"
    );
    let mut d = base(node("sum", "mix", &[]));
    d.nodes.push(node("one", "constant", &[("value", 1e6)]));
    edge(&mut d, "one", "sum", "a");
    edge(&mut d, "one", "sum", "b");
    let mut i = instance(&d);
    let mut out = [[1.; 2]; 3];
    assert_eq!(i.render(&mut out).unwrap_err().code, "audio.graph.math");
    assert_eq!(out, [[0.; 2]; 3]);
}

#[test]
fn immediate_reset_then_stop_reports_terminal_new_epoch() {
    let program = graph::compile(&base(node("c", "constant", &[("value", 0.25)]))).unwrap();
    let e = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut stream =
        ProceduralStream::spawn(program, e.control(), PlayOptions::default(), 2, None).unwrap();
    let generation = stream.reset().unwrap();
    stream.stop().unwrap();
    let status = stream.status().unwrap();
    assert_eq!(status.generation, generation);
    assert_eq!(status.state, ProceduralState::Stopped);
    assert_eq!(e.voice_count(), 0);
}
