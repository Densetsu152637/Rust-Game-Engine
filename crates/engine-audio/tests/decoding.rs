use engine_audio::{
    AudioConfig, AudioDecoder, AudioEngine, DecodeLimits, DecodeProgress, DecodeWorkerState,
    DecodedStream, PlayOptions, StreamConfig, StreamGeneration, StreamState,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new(extension: &str, bytes: &[u8]) -> Self {
        let p = std::env::temp_dir().join(format!(
            "engine-audio-decode-{}-{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            extension
        ));
        std::fs::write(&p, bytes).unwrap();
        Self(p)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn crc8(bytes: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in bytes {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 7 } else { c << 1 };
        }
    }
    c
}
fn crc16(bytes: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in bytes {
        c ^= u16::from(b) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
        }
    }
    c
}
// Real FLAC constant subframes: each 64-frame block uses only one encoded sample.
fn flac() -> Vec<u8> {
    let mut bytes = b"fLaC\x80\x00\x00\x22".to_vec();
    let mut info = [0u8; 34];
    info[..2].copy_from_slice(&64u16.to_be_bytes());
    info[2..4].copy_from_slice(&64u16.to_be_bytes());
    let packed = (8000u64 << 44) | (15u64 << 36) | 128;
    info[10..18].copy_from_slice(&packed.to_be_bytes());
    bytes.extend(info);
    for (index, value) in [8192i16, -8192].iter().enumerate() {
        let mut frame = vec![0xff, 0xf8, 0x64, 0x08, index as u8, 63];
        frame.push(crc8(&frame));
        frame.push(0);
        frame.extend(value.to_be_bytes());
        frame.extend(crc16(&frame).to_be_bytes());
        bytes.extend(frame);
    }
    bytes
}
// MPEG-1 Layer III, 128 kbit/s, 44.1 kHz stereo. Empty granules encode silence.
fn mp3() -> Vec<u8> {
    let mut b = vec![];
    for _ in 0..4 {
        let mut frame = vec![0; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0]);
        b.extend(frame);
    }
    b
}
fn wav(frames: usize) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(vec![]);
    {
        let mut w = hound::WavWriter::new(
            &mut cursor,
            hound::WavSpec {
                channels: 1,
                sample_rate: 8000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for i in 0..frames {
            w.write_sample(if i % 2 == 0 { 8192i16 } else { -8192i16 })
                .unwrap();
        }
        w.finalize().unwrap();
    }
    cursor.into_inner()
}
fn engine(rate: u32) -> AudioEngine {
    AudioEngine::offline(AudioConfig {
        sample_rate: rate,
        max_voices: 8,
    })
    .unwrap()
}
fn queue(
    e: &AudioEngine,
    d: &AudioDecoder,
    capacity: usize,
) -> (engine_audio::VoiceHandle, engine_audio::StreamWriter) {
    e.play_stream(
        StreamConfig {
            sample_rate: d.info().sample_rate,
            channels: d.info().channels,
            capacity_frames: capacity,
        },
        PlayOptions::default(),
    )
    .unwrap()
}
fn drain(
    d: &mut AudioDecoder,
    e: &mut AudioEngine,
    w: &mut engine_audio::StreamWriter,
    g: StreamGeneration,
) -> Vec<f32> {
    let mut samples = vec![];
    for _ in 0..100_000 {
        let p = d.pump(w, g).unwrap();
        let before = w.snapshot().unwrap().submitted_output_frames;
        let mut out = [0.; 2];
        e.mix(&mut out).unwrap();
        if w.snapshot().unwrap().submitted_output_frames > before {
            samples.push(out[0]);
        }
        if p == DecodeProgress::Finished && w.snapshot().unwrap().state == StreamState::Drained {
            return samples;
        }
    }
    panic!("decoder did not drain")
}
#[test]
fn flac_is_incremental_lossless_and_retains_partial_writes() {
    let file = Fixture::new("flac", &flac());
    let mut d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
    assert_eq!(
        (
            d.info().sample_rate,
            d.info().channels,
            d.info().total_frames
        ),
        (8000, 1, Some(128))
    );
    assert_eq!(d.info().codec, "flac");
    let mut e = engine(8000);
    let (_, mut w) = queue(&e, &d, 3);
    let samples = drain(&mut d, &mut e, &mut w, StreamGeneration(0));
    assert_eq!(samples.len(), 128);
    assert!(samples[..64].iter().all(|&s| s == 0.25));
    assert!(samples[64..].iter().all(|&s| s == -0.25));
    assert_eq!(w.snapshot().unwrap().source_seconds, 0.016);
}
#[test]
fn real_mp3_silence_and_pcm_wav_decode_to_expected_duration() {
    for (extension, bytes, rate, count) in [("mp3", mp3(), 44100, 4608), ("wav", wav(67), 8000, 67)]
    {
        let file = Fixture::new(extension, &bytes);
        let mut d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
        let mut e = engine(rate);
        let (_, mut w) = queue(&e, &d, 7);
        let samples = drain(&mut d, &mut e, &mut w, StreamGeneration(0));
        assert_eq!(samples.len(), count);
        if extension == "mp3" {
            assert!(samples.iter().all(|&s| s == 0.));
        } else {
            assert_eq!(&samples[..4], &[0.25, -0.25, 0.25, -0.25]);
        }
    }
}
#[test]
fn seek_discards_pending_packet_and_rejects_stale_generation() {
    let file = Fixture::new("flac", &flac());
    let mut d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
    let mut e = engine(8000);
    let (v, mut w) = queue(&e, &d, 4);
    d.pump(&mut w, StreamGeneration(0)).unwrap();
    let mut out = [0.; 4];
    e.mix(&mut out).unwrap();
    let next = e.seek_stream(v, 64).unwrap();
    d.seek(64).unwrap();
    assert_eq!(
        d.pump(&mut w, StreamGeneration(0)).unwrap_err().code,
        "audio.stream.stale"
    );
    let samples = drain(&mut d, &mut e, &mut w, next);
    assert_eq!(samples, vec![-0.25; 64]);
    assert_eq!(w.snapshot().unwrap().source_frame, 128.);
}
#[test]
fn malformed_truncated_and_resource_limited_inputs_fail_explicitly() {
    let garbage = Fixture::new("bin", b"not audio");
    assert!(AudioDecoder::open(&garbage.0, DecodeLimits::default()).is_err());
    let bytes = flac();
    let truncated = Fixture::new("flac", &bytes[..bytes.len() - 5]);
    let mut d = AudioDecoder::open(&truncated.0, DecodeLimits::default()).unwrap();
    let mut e = engine(8000);
    let (_, mut w) = queue(&e, &d, 128);
    let mut failed = false;
    for _ in 0..10 {
        if d.pump(&mut w, StreamGeneration(0)).is_err() {
            failed = true;
            break;
        }
        e.mix(&mut [0.; 256]).unwrap();
    }
    assert!(failed);
    assert_eq!(w.snapshot().unwrap().state, StreamState::Failed);
    let good = Fixture::new("flac", &bytes);
    assert!(
        AudioDecoder::open(
            &good.0,
            DecodeLimits {
                max_file_bytes: 8,
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        AudioDecoder::open(
            &good.0,
            DecodeLimits {
                max_packet_frames: 32,
                ..Default::default()
            }
        )
        .is_err()
    );
    let pcm = Fixture::new("wav", &wav(64));
    let mut d = AudioDecoder::open(
        &pcm.0,
        DecodeLimits {
            max_packet_bytes: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let e = engine(8000);
    let (_, mut w) = queue(&e, &d, 4);
    assert!(d.pump(&mut w, StreamGeneration(0)).is_err());
}
fn until(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !ready() {
        assert!(Instant::now() < end, "worker timed out");
        thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn worker_seek_and_drop_cancel_full_queue_without_waiting_for_playback() {
    let file = Fixture::new("flac", &flac());
    let d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
    let mut e = engine(8000);
    let mut worker = DecodedStream::spawn(d, e.control(), PlayOptions::default(), 4).unwrap();
    until(|| worker.status().unwrap().state == DecodeWorkerState::Backpressure);
    let generation = worker.seek(64).unwrap();
    until(|| {
        e.stream_snapshot(worker.voice())
            .unwrap()
            .unwrap()
            .queued_frames
            > 0
    });
    let mut out = [0.; 2];
    e.mix(&mut out).unwrap();
    assert_eq!(out, [-0.25, -0.25]);
    assert_eq!(
        e.stream_snapshot(worker.voice())
            .unwrap()
            .unwrap()
            .generation,
        generation
    );
    let now = Instant::now();
    worker.cancel().unwrap();
    assert!(now.elapsed() < Duration::from_secs(2));
    assert_eq!(e.voice_count(), 0);
    assert_eq!(worker.status().unwrap().state, DecodeWorkerState::Cancelled);
    assert!(worker.seek(0).is_err());
    let d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
    let worker = DecodedStream::spawn(d, e.control(), PlayOptions::default(), 2).unwrap();
    until(|| worker.status().unwrap().state == DecodeWorkerState::Backpressure);
    drop(worker);
    assert_eq!(e.voice_count(), 0);
}
#[test]
fn stopped_playback_cancelled_decoder_and_wrong_queue_format_do_not_decode_more() {
    let file = Fixture::new("wav", &wav(64));
    let mut d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
    let e = engine(8000);
    let (v, mut w) = queue(&e, &d, 4);
    e.stop(v);
    assert_eq!(
        d.pump(&mut w, StreamGeneration(0)).unwrap_err().code,
        "audio.stream.stopped"
    );
    let mut d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
    let (_, mut w) = queue(&e, &d, 4);
    d.cancel();
    assert_eq!(
        d.pump(&mut w, StreamGeneration(0)).unwrap_err().code,
        "audio.decode.cancelled"
    );
    assert!(d.seek(0).is_err());
    let mut d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
    let (_, mut w) = e
        .play_stream(
            StreamConfig {
                sample_rate: 16000,
                channels: 1,
                capacity_frames: 4,
            },
            PlayOptions::default(),
        )
        .unwrap();
    assert_eq!(
        d.pump(&mut w, StreamGeneration(0)).unwrap_err().code,
        "audio.decode.format"
    );
}

fn variable_bitrate_mp3(low_first: bool) -> Vec<u8> {
    let mut bytes = vec![];
    let rates = if low_first {
        [(1u8, 104usize), (14, 1044)]
    } else {
        [(14u8, 1044usize), (1, 104)]
    };
    for (index, size) in rates {
        for _ in 0..20 {
            let mut frame = vec![0; size];
            frame[..4].copy_from_slice(&[0xff, 0xfb, index << 4, 0]);
            bytes.extend(frame);
        }
    }
    bytes
}
#[test]
fn headerless_vbr_estimates_never_trim_pcm_or_bound_seek() {
    for low_first in [false, true] {
        let file = Fixture::new("mp3", &variable_bitrate_mp3(low_first));
        let mut d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
        assert_eq!(d.info().total_frames, None);
        assert!(d.info().estimated_total_frames.is_some());
        let actual = 40 * 1152;
        assert_ne!(d.info().estimated_total_frames, Some(actual));
        let mut e = engine(44100);
        let (_, mut w) = queue(&e, &d, 7);
        assert_eq!(
            drain(&mut d, &mut e, &mut w, StreamGeneration(0)),
            vec![0.; actual as usize]
        );
        d.seek(actual - 50).unwrap();
        let (_, mut w) = queue(&e, &d, 4);
        assert_eq!(
            drain(&mut d, &mut e, &mut w, StreamGeneration(0)),
            vec![0.; 50]
        );
        d.seek(actual).unwrap();
        let (_, mut w) = queue(&e, &d, 4);
        assert!(drain(&mut d, &mut e, &mut w, StreamGeneration(0)).is_empty());
    }
}
fn reference(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}
fn compare_pcm(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    let maximum = actual
        .iter()
        .zip(expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0., f32::max);
    assert!(
        maximum < 0.0002,
        "maximum decoder/reference difference {maximum}"
    );
}
#[test]
fn nonzero_mp3_and_vorbis_match_reference_and_seek_with_gapless_trimming() {
    for (extension, bytes, pcm) in [
        (
            "mp3",
            include_bytes!("fixtures/tone.mp3").as_slice(),
            include_bytes!("fixtures/tone-mp3.f32").as_slice(),
        ),
        (
            "ogg",
            include_bytes!("fixtures/tone.ogg").as_slice(),
            include_bytes!("fixtures/tone-vorbis.f32").as_slice(),
        ),
    ] {
        let file = Fixture::new(extension, bytes);
        let mut d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
        assert_eq!(d.info().total_frames, Some(8820));
        let expected = reference(pcm);
        assert_eq!(expected.len(), 8820);
        let mut e = engine(44100);
        let (_, mut w) = queue(&e, &d, 11);
        let actual = drain(&mut d, &mut e, &mut w, StreamGeneration(0));
        compare_pcm(&actual, &expected);
        assert!(actual.iter().any(|s| s.abs() > 0.05));
        d.seek(1000).unwrap();
        let (_, mut w) = queue(&e, &d, 7);
        compare_pcm(
            &drain(&mut d, &mut e, &mut w, StreamGeneration(0)),
            &expected[1000..],
        );
        let truncated = Fixture::new(extension, &bytes[..bytes.len() - 20]);
        match AudioDecoder::open(&truncated.0, DecodeLimits::default()) {
            Err(_) => {}
            Ok(mut d) => {
                let (_, mut w) = queue(&e, &d, 128);
                let mut rejected = false;
                for _ in 0..1000 {
                    match d.pump(&mut w, StreamGeneration(0)) {
                        Err(_) => {
                            rejected = true;
                            break;
                        }
                        Ok(DecodeProgress::Finished) => break,
                        _ => {
                            e.mix(&mut [0.; 256]).unwrap();
                        }
                    }
                }
                assert!(rejected, "truncated {extension} accepted as clean EOF");
            }
        }
    }
}
#[test]
fn caller_driven_cancel_retires_queued_pcm_and_cannot_fail_a_new_epoch() {
    let file = Fixture::new("wav", &wav(64));
    let mut d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
    let mut e = engine(8000);
    let (v, mut w) = queue(&e, &d, 8);
    d.pump(&mut w, StreamGeneration(0)).unwrap();
    d.cancel();
    assert_eq!(
        d.pump(&mut w, StreamGeneration(0)).unwrap_err().code,
        "audio.decode.cancelled"
    );
    assert_eq!(w.snapshot().unwrap().state, StreamState::Failed);
    let mut out = [1.; 16];
    e.mix(&mut out).unwrap();
    assert_eq!(out, [0.; 16]);
    assert!(e.stream_snapshot(v).unwrap().is_none());
    let mut d = AudioDecoder::open(&file.0, DecodeLimits::default()).unwrap();
    let (v, mut w) = queue(&e, &d, 8);
    d.pump(&mut w, StreamGeneration(0)).unwrap();
    let next = e.seek_stream(v, 32).unwrap();
    d.cancel();
    assert_eq!(
        d.pump(&mut w, StreamGeneration(0)).unwrap_err().code,
        "audio.decode.cancelled"
    );
    assert_ne!(w.snapshot().unwrap().state, StreamState::Failed);
    w.write(next, &[0.5]).unwrap();
    e.mix(&mut [0.; 2]).unwrap();
    assert_eq!(w.snapshot().unwrap().source_frame, 33.);
}

#[test]
fn resident_loading_preserves_samples_and_checks_declared_and_cumulative_pcm_caps() {
    let file = Fixture::new("flac", &flac());
    let clip = engine_audio::load_resident(&file.0, DecodeLimits::default(), 512).unwrap();
    assert_eq!(clip.frames(), 128);
    assert!(clip.samples()[..64].iter().all(|s| *s == 0.25));
    assert!(clip.samples()[64..].iter().all(|s| *s == -0.25));
    assert_eq!(
        engine_audio::load_resident(&file.0, DecodeLimits::default(), 508)
            .unwrap_err()
            .code,
        "audio.resident.limit"
    );
    let unknown = Fixture::new("mp3", &variable_bitrate_mp3(true));
    assert_eq!(
        AudioDecoder::open(&unknown.0, DecodeLimits::default())
            .unwrap()
            .info()
            .total_frames,
        None
    );
    assert_eq!(
        engine_audio::load_resident(&unknown.0, DecodeLimits::default(), 10000)
            .unwrap_err()
            .code,
        "audio.resident.limit"
    );
    let tone = Fixture::new("mp3", include_bytes!("fixtures/tone.mp3"));
    let clip = engine_audio::load_resident(&tone.0, DecodeLimits::default(), 8820 * 4).unwrap();
    compare_pcm(
        clip.samples(),
        &reference(include_bytes!("fixtures/tone-mp3.f32")),
    );
}
