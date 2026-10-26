use engine_audio::{
    AudioConfig, AudioEngine, PlayOptions, StreamConfig, StreamGeneration, StreamState,
};

fn config(rate: u32, channels: u16, capacity_frames: usize) -> StreamConfig {
    StreamConfig {
        sample_rate: rate,
        channels,
        capacity_frames,
    }
}
fn engine() -> AudioEngine {
    AudioEngine::offline(AudioConfig {
        sample_rate: 8_000,
        max_voices: 8,
    })
    .unwrap()
}
fn render(e: &mut AudioEngine, n: usize) -> Vec<f32> {
    let mut out = vec![0.; n * 2];
    e.mix(&mut out).unwrap();
    out
}

#[test]
fn bounded_queue_backpressure_wrap_and_clock() {
    let mut e = engine();
    let (v, mut w) = e
        .play_stream(config(8_000, 1, 3), PlayOptions::default())
        .unwrap();
    let g = StreamGeneration(0);
    assert_eq!(w.write(g, &[0.1, 0.2, 0.3, 0.4]).unwrap(), 3);
    assert_eq!(w.write(g, &[0.4]).unwrap(), 0);
    assert_eq!(render(&mut e, 2), vec![0.1, 0.1, 0.2, 0.2]);
    // The last interpolation frame remains in the ring until its successor is needed.
    assert_eq!(w.write(g, &[0.4]).unwrap(), 1);
    assert_eq!(render(&mut e, 2), vec![0.3, 0.3, 0.4, 0.4]);
    let s = e.stream_snapshot(v).unwrap().unwrap();
    assert_eq!(s.source_frame, 4.);
    assert_eq!(s.submitted_output_frames, 4);
    assert_eq!(s.capacity_frames, 3);
    assert_eq!(s.presentation_seconds, None);
}
#[test]
fn fractional_interpolation_crosses_producer_chunks_and_drains_eof() {
    let mut e = engine();
    let (_, mut w) = e
        .play_stream(
            config(8_000, 1, 4),
            PlayOptions {
                pitch: 0.5,
                ..Default::default()
            },
        )
        .unwrap();
    let g = StreamGeneration(0);
    w.write(g, &[0.2]).unwrap();
    assert_eq!(render(&mut e, 2), vec![0.2, 0.2, 0., 0.]);
    assert_eq!(w.snapshot().unwrap().source_frame, 0.5);
    assert_eq!(w.snapshot().unwrap().state, StreamState::Starved);
    w.write(g, &[0.6]).unwrap();
    assert_eq!(render(&mut e, 2), vec![0.40000004, 0.40000004, 0.6, 0.6]);
    w.finish(g).unwrap();
    assert_eq!(render(&mut e, 2), vec![0.6, 0.6, 0., 0.]);
    let s = w.snapshot().unwrap();
    assert_eq!(s.state, StreamState::Drained);
    assert_eq!(s.source_frame, 2.);
    assert_eq!(s.submitted_output_frames, 4);
}
#[test]
fn starvation_is_silence_and_holds_clock_until_data_arrives() {
    let mut e = engine();
    let (v, mut w) = e
        .play_stream(config(8_000, 2, 4), PlayOptions::default())
        .unwrap();
    let g = StreamGeneration(0);
    assert_eq!(render(&mut e, 10), vec![0.; 20]);
    assert_eq!(e.voice_count(), 1);
    let s = w.snapshot().unwrap();
    assert_eq!(s.source_frame, 0.);
    assert_eq!(s.underrun_output_frames, 10);
    w.write(g, &[0.1, -0.1, 0.3, -0.3]).unwrap();
    assert_eq!(render(&mut e, 2), vec![0.1, -0.1, 0.3, -0.3]);
    assert!(e.pause(v));
    assert_eq!(render(&mut e, 10), vec![0.; 20]);
    assert_eq!(w.snapshot().unwrap().state, StreamState::Paused);
    assert_eq!(w.snapshot().unwrap().source_frame, 2.);
    w.write(g, &[0.5, -0.5]).unwrap();
    assert!(e.resume(v));
    w.finish(g).unwrap();
    assert_eq!(render(&mut e, 1), vec![0.5, -0.5]);
    assert_eq!(w.snapshot().unwrap().state, StreamState::Drained);
}
#[test]
fn rate_pitch_seek_generation_and_stop_invalidate_old_work() {
    let mut e = engine();
    let (v, mut w) = e
        .play_stream(config(16_000, 1, 8), PlayOptions::default())
        .unwrap();
    let g = StreamGeneration(0);
    w.write(g, &[0.1, 0.2, 0.3, 0.4, 0.5]).unwrap();
    assert_eq!(render(&mut e, 2), vec![0.1, 0.1, 0.3, 0.3]);
    assert_eq!(w.snapshot().unwrap().source_frame, 4.);
    let next = e.seek_stream(v, 160).unwrap();
    assert_eq!(next, StreamGeneration(1));
    assert_eq!(w.write(g, &[0.8]).unwrap_err().code, "audio.stream.stale");
    assert!(w.finish(g).is_err());
    let s = w.snapshot().unwrap();
    assert_eq!(s.source_seconds, 0.01);
    assert_eq!(s.queued_frames, 0);
    assert_eq!(s.submitted_output_frames, 0);
    w.write(next, &[0.7, 0.8]).unwrap();
    w.finish(next).unwrap();
    assert_eq!(render(&mut e, 1), vec![0.7, 0.7]);
    assert_eq!(w.snapshot().unwrap().source_frame, 162.);
    let (v, mut w) = e
        .play_stream(config(8_000, 1, 4), PlayOptions::default())
        .unwrap();
    assert!(e.stop(v));
    assert_eq!(w.snapshot().unwrap().state, StreamState::Stopped);
    assert_eq!(w.write(g, &[0.1]).unwrap_err().code, "audio.stream.stopped");
}
#[test]
fn producer_drop_eof_failure_shutdown_and_validation() {
    let mut e = engine();
    let (_, mut w) = e
        .play_stream(config(8_000, 1, 4), PlayOptions::default())
        .unwrap();
    w.write(StreamGeneration(0), &[0.2, 0.4]).unwrap();
    drop(w);
    assert_eq!(render(&mut e, 3), vec![0.2, 0.2, 0.4, 0.4, 0., 0.]);
    assert_eq!(e.voice_count(), 0);
    let (_, mut w) = e
        .play_stream(config(8_000, 1, 4), PlayOptions::default())
        .unwrap();
    assert!(w.write(StreamGeneration(0), &[f32::NAN]).is_err());
    w.fail(
        StreamGeneration(0),
        engine_audio::AudioError {
            code: "decode.bad",
            message: "bad data".into(),
        },
    )
    .unwrap();
    assert_eq!(render(&mut e, 1), vec![0.; 2]);
    assert_eq!(w.snapshot().unwrap().state, StreamState::Failed);
    let (_, w) = e
        .play_stream(config(8_000, 1, 4), PlayOptions::default())
        .unwrap();
    e.shutdown();
    assert_eq!(w.snapshot().unwrap().state, StreamState::Stopped);
    let e = engine();
    for c in [
        config(1, 1, 4),
        config(8_000, 3, 4),
        config(8_000, 1, 1),
        config(8_000, 1, usize::MAX),
    ] {
        assert!(e.play_stream(c, PlayOptions::default()).is_err());
    }
}
#[test]
fn acoustic_stream_starvation_does_not_end_input_and_spatial_stereo_is_mono() {
    let mut e = engine();
    let mut options = PlayOptions::default();
    options.spatial.enabled = true;
    options.acoustics.enabled = true;
    options.acoustics.propagation_delay = false;
    let (_, mut w) = e.play_stream(config(8_000, 2, 8), options).unwrap();
    assert_eq!(render(&mut e, 32), vec![0.; 64]);
    assert_eq!(e.voice_count(), 1);
    w.write(StreamGeneration(0), &[0.5, -0.5, 0.5, -0.5])
        .unwrap();
    assert_eq!(render(&mut e, 2), vec![0.; 4]);
    assert_eq!(w.snapshot().unwrap().source_frame, 2.);
    assert_eq!(w.snapshot().unwrap().state, StreamState::Playing);
}
