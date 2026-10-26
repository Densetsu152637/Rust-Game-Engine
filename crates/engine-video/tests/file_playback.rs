use engine_audio::{AudioConfig, AudioEngine};
use engine_video::{DecoderTools, PlaybackState, VideoPlayer};
use std::{
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};
fn tools() -> DecoderTools {
    DecoderTools {
        ffmpeg: std::env::var_os("ENGINE_VIDEO_FFMPEG")
            .expect("Set ENGINE_VIDEO_FFMPEG for real decoder tests")
            .into(),
        ffprobe: std::env::var_os("ENGINE_VIDEO_FFPROBE")
            .expect("Set ENGINE_VIDEO_FFPROBE for real decoder tests")
            .into(),
    }
}
fn fixture(audio: bool) -> (tempfile::TempDir, PathBuf, DecoderTools) {
    let t = tools();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mkv");
    let mut c = Command::new(&t.ffmpeg);
    c.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=red:s=16x16:r=30:d=0.5",
        "-f",
        "lavfi",
        "-i",
        "color=blue:s=16x16:r=30:d=0.5",
    ]);
    if audio {
        c.args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
        ]);
    }
    c.args([
        "-filter_complex",
        "[0:v][1:v]concat=n=2:v=1:a=0[v]",
        "-map",
        "[v]",
    ]);
    if audio {
        c.args(["-map", "2:a", "-c:a", "pcm_s16le"]);
    }
    assert!(
        c.args(["-c:v", "ffv1", "-threads", "1"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    (dir, path, t)
}
fn wait_frame(p: &mut VideoPlayer) -> engine_video::VideoFrame {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(f) = p.poll().unwrap() {
            return f;
        }
        assert!(
            Instant::now() < deadline,
            "frame timeout {:?}",
            p.snapshot()
        );
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn real_file_pixels_audio_pause_seek_and_shared_clock() {
    let (_d, path, t) = fixture(true);
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut p = VideoPlayer::open(&path, t, Some(audio.control())).unwrap();
    let clock = p.clock();
    let first = wait_frame(&mut p);
    assert!(first.rgba[0] > 240 && first.rgba[2] < 10);
    assert_eq!(first.seconds, 0.);
    let mut heard = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while clock.seconds().unwrap() < 0.4 {
        let mut pcm = vec![0.; 960 * 2];
        audio.mix(&mut pcm).unwrap();
        heard |= pcm.iter().any(|v| v.abs() > 0.01);
        p.poll().unwrap();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(heard);
    let before = clock.seconds().unwrap();
    assert!(before > 0.1 && before <= 0.5);
    p.pause();
    audio.mix(&mut vec![0.; 4800]).unwrap();
    assert_eq!(clock.seconds().unwrap(), before);
    assert_eq!(p.snapshot().state, PlaybackState::Paused);
    p.seek(0.5).unwrap();
    assert_eq!(clock.seconds(), Some(0.5));
    let blue = wait_frame(&mut p);
    assert!(blue.rgba[2] > 240 && blue.rgba[0] < 10);
    assert_eq!(blue.seconds, 0.5);
    p.seek(0.51).unwrap();
    assert_eq!(clock.seconds(), Some(0.51));
    assert_eq!(wait_frame(&mut p).seconds, 0.5);
    p.resume();
    let deadline = Instant::now() + Duration::from_secs(5);
    while clock.seconds().unwrap() < 1.0 {
        audio.mix(&mut vec![0.; 960 * 2]).unwrap();
        p.poll().unwrap();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    // EOF is observed only after the bounded video queue has been consumed.
    p.poll().unwrap();
    assert_eq!(p.snapshot().state, PlaybackState::Ended);
    assert!(clock.seconds().unwrap() >= 0.95);
    p.stop();
    assert!(p.poll().unwrap().is_none());
    assert_eq!(clock.seconds(), None);
    assert_eq!(p.snapshot().state, PlaybackState::Stopped);
    assert_eq!(audio.voice_count(), 0);
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn silent_file_pause_invalid_input_and_bounded_drop() {
    let (_d, path, t) = fixture(false);
    let mut p = VideoPlayer::open(&path, t.clone(), None).unwrap();
    wait_frame(&mut p);
    p.pause();
    let at = p.clock().seconds().unwrap();
    thread::sleep(Duration::from_millis(30));
    assert_eq!(p.clock().seconds(), Some(at));
    p.seek(0.5).unwrap();
    let f = wait_frame(&mut p);
    assert!(f.rgba[2] > 240);
    let began = Instant::now();
    drop(p);
    assert!(began.elapsed() < Duration::from_secs(2));
    assert!(
        VideoPlayer::open(
            &path,
            DecoderTools {
                ffprobe: "missing-decoder-test".into(),
                ..t
            },
            None
        )
        .err()
        .unwrap()
        .0
        .contains("ffprobe")
    );
}
#[test]
fn missing_tool_and_non_file_inputs_report_failures() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        VideoPlayer::open(dir.path(), DecoderTools::default(), None)
            .err()
            .unwrap()
            .0
            .contains("regular local")
    );
    let path = dir.path().join("not-a-video");
    std::fs::write(&path, b"invalid").unwrap();
    assert!(
        VideoPlayer::open(
            &path,
            DecoderTools {
                ffmpeg: "missing-ffmpeg-test".into(),
                ffprobe: "missing-ffprobe-test".into()
            },
            None
        )
        .err()
        .unwrap()
        .0
        .contains("Cannot start configured ffprobe")
    );
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn normalizes_nonzero_container_origin_and_preserves_delayed_audio() {
    let tools = tools();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("offset.mkv");
    assert!(
        Command::new(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=blue:s=16x16:r=30:d=1",
                "-itsoffset",
                "0.2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=0.8",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c:a",
                "pcm_s16le",
                "-c:v",
                "ffv1",
                "-threads",
                "1",
                "-output_ts_offset",
                "5"
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut p = VideoPlayer::open(&path, tools, Some(audio.control())).unwrap();
    assert!((p.info().duration - 1.).abs() < 0.001);
    let first = wait_frame(&mut p);
    assert_eq!(first.seconds, 0.);
    assert!(first.rgba[2] > 240);
    let mut before = Vec::new();
    let mut after = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while p.clock().seconds().unwrap() < 0.4 {
        let time = p.clock().seconds().unwrap();
        let mut pcm = vec![0.; 480 * 2];
        audio.mix(&mut pcm).unwrap();
        if time < 0.18 {
            before.extend(pcm);
        } else if time > 0.25 {
            after.extend(pcm);
        }
        p.poll().unwrap();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(before.iter().all(|v| *v == 0.));
    assert!(after.iter().any(|v| v.abs() > 0.01));
    p.pause();
    p.seek(0.5).unwrap();
    assert_eq!(p.clock().seconds(), Some(0.5));
    assert!(wait_frame(&mut p).rgba[2] > 240);
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn variable_rate_input_is_resampled_to_documented_thirty_hz_timeline() {
    let tools = tools();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vfr.mkv");
    assert!(
        Command::new(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=16x16:rate=10:duration=1",
                "-vf",
                "setpts=if(lt(N\\,5)\\,N/10/TB\\,(0.5+(N-5)/5)/TB)",
                "-fps_mode",
                "vfr",
                "-c:v",
                "ffv1",
                "-threads",
                "1"
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let mut p = VideoPlayer::open(&path, tools, None).unwrap();
    p.pause();
    let first = wait_frame(&mut p);
    assert_eq!(first.seconds, 0.);
    p.seek(0.4).unwrap();
    let frame = wait_frame(&mut p);
    assert!((frame.seconds - 0.4).abs() < 0.000001);
    assert_eq!(frame.rgba.len(), 16 * 16 * 4);
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn missing_decoder_releases_new_audio_voice() {
    let (_dir, path, mut tools) = fixture(true);
    tools.ffmpeg = "missing-ffmpeg-test".into();
    let audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let error = VideoPlayer::open(&path, tools, Some(audio.control()))
        .err()
        .unwrap();
    assert!(error.0.contains("Cannot start configured ffmpeg"));
    assert_eq!(audio.voice_count(), 0);
}
#[cfg(windows)]
#[test]
fn unc_inputs_are_rejected_before_network_filesystem_access() {
    let error = VideoPlayer::open(
        std::path::Path::new(r"\\example.invalid\share\movie.mp4"),
        DecoderTools::default(),
        None,
    )
    .err()
    .unwrap();
    assert_eq!(error.0, "Network file paths are unsupported");
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn delayed_video_waits_for_its_timestamp_on_audio_master() {
    let tools = tools();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("delayed-video.mkv");
    assert!(
        Command::new(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-itsoffset",
                "0.2",
                "-f",
                "lavfi",
                "-i",
                "color=blue:s=16x16:r=30:d=0.8",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=1",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c:a",
                "pcm_s16le",
                "-c:v",
                "ffv1",
                "-threads",
                "1"
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut p = VideoPlayer::open(&path, tools, Some(audio.control())).unwrap();
    assert!((p.info().video_start - 0.2).abs() < 0.001);
    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        let time = p.clock().seconds().unwrap();
        let frame = p.poll().unwrap();
        if time < 0.18 {
            assert!(frame.is_none());
        }
        if let Some(frame) = frame {
            break frame;
        }
        audio.mix(&mut vec![0.; 960]).unwrap();
        thread::sleep(Duration::from_millis(2));
        assert!(Instant::now() < deadline);
    };
    assert!((frame.seconds - 0.2).abs() < 0.000001);
    assert!(frame.rgba[2] > 240);
    while p.snapshot().state != PlaybackState::Ended {
        audio.mix(&mut vec![0.; 960]).unwrap();
        p.poll().unwrap();
        thread::sleep(Duration::from_millis(2));
        assert!(Instant::now() < deadline);
    }
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn unequal_track_lengths_play_and_seek_through_audio_only_tail() {
    let tools = tools();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tail.mkv");
    assert!(
        Command::new(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=blue:s=16x16:r=30:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=5",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c:a",
                "pcm_s16le",
                "-c:v",
                "ffv1",
                "-threads",
                "1"
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut p = VideoPlayer::open(&path, tools, Some(audio.control())).unwrap();
    let clock = p.clock();
    assert_eq!(p.info().duration, 5.);
    assert!(wait_frame(&mut p).rgba[2] > 240);
    let deadline = Instant::now() + Duration::from_secs(8);
    while clock.seconds().unwrap() < 1.2 {
        audio.mix(&mut vec![0.; 960]).unwrap();
        p.poll().unwrap();
        thread::sleep(Duration::from_millis(2));
        assert!(Instant::now() < deadline);
    }
    assert_eq!(p.snapshot().state, PlaybackState::Playing);
    p.pause();
    p.seek(3.).unwrap();
    let tail = wait_frame(&mut p);
    assert_eq!(tail.seconds, 3.);
    assert!(tail.rgba[2] > 240);
    assert_eq!(clock.seconds(), Some(3.));
    p.resume();
    let mut heard = false;
    while p.snapshot().state != PlaybackState::Ended {
        let mut pcm = vec![0.; 960];
        audio.mix(&mut pcm).unwrap();
        heard |= pcm.iter().any(|s| s.abs() > 0.01);
        p.poll().unwrap();
        thread::sleep(Duration::from_millis(2));
        assert!(Instant::now() < deadline);
    }
    assert!(heard);
    assert_eq!(clock.seconds(), Some(5.));
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn short_audio_is_padded_until_video_ends() {
    let tools = tools();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short-audio.mkv");
    assert!(
        Command::new(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=blue:s=16x16:r=30:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=0.5",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c:a",
                "pcm_s16le",
                "-c:v",
                "ffv1",
                "-threads",
                "1"
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut p = VideoPlayer::open(&path, tools, Some(audio.control())).unwrap();
    wait_frame(&mut p);
    let deadline = Instant::now() + Duration::from_secs(5);
    while p.clock().seconds().unwrap() < 0.75 {
        audio.mix(&mut vec![0.; 960]).unwrap();
        p.poll().unwrap();
        thread::sleep(Duration::from_millis(2));
        assert!(Instant::now() < deadline);
    }
    let mut silence = vec![1.; 960];
    audio.mix(&mut silence).unwrap();
    assert!(silence.iter().all(|s| *s == 0.));
    assert_eq!(p.snapshot().state, PlaybackState::Playing);
    while p.snapshot().state != PlaybackState::Ended {
        audio.mix(&mut vec![0.; 960]).unwrap();
        p.poll().unwrap();
        thread::sleep(Duration::from_millis(2));
        assert!(Instant::now() < deadline);
    }
    assert_eq!(p.clock().seconds(), Some(1.));
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn failed_seek_is_terminal_and_a_restored_file_can_recover() {
    let (_dir, path, tools) = fixture(false);
    let mut p = VideoPlayer::open(&path, tools, None).unwrap();
    p.pause();
    wait_frame(&mut p);
    let clock = p.clock();
    p.stop();
    let renamed = path.with_extension("moved");
    std::fs::rename(&path, &renamed).unwrap();
    assert!(p.seek(0.5).is_err());
    assert_eq!(p.snapshot().state, PlaybackState::Failed);
    assert!(p.snapshot().error.is_some());
    assert!(p.poll().is_err());
    assert_eq!(clock.seconds(), None);
    std::fs::rename(&renamed, &path).unwrap();
    p.seek(0.5).unwrap();
    assert_eq!(p.snapshot().state, PlaybackState::Paused);
    assert!(p.snapshot().error.is_none());
    assert_eq!(clock.seconds(), Some(0.5));
    assert!(wait_frame(&mut p).rgba[2] > 240);
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn audio_shutdown_makes_seek_failure_visible_to_all_consumers() {
    let (_dir, path, tools) = fixture(true);
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut p = VideoPlayer::open(&path, tools, Some(audio.control())).unwrap();
    wait_frame(&mut p);
    audio.shutdown();
    assert!(p.seek(0.5).is_err());
    assert_eq!(p.snapshot().state, PlaybackState::Failed);
    assert!(p.snapshot().error.is_some());
    assert!(p.poll().is_err());
    assert_eq!(p.clock().seconds(), None);
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn real_probe_rejects_malformed_local_media() {
    let tools = tools();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("malformed.mkv");
    std::fs::write(&path, b"this is not a media container").unwrap();
    let error = VideoPlayer::open(&path, tools, None).err().unwrap();
    assert!(error.0.contains("probe failed"), "{error}");
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn late_corrupt_video_packet_fails_and_retires_audio() {
    let tools = tools();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt.mkv");
    assert!(
        Command::new(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=blue:s=16x16:r=30:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=1",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c:a",
                "pcm_s16le",
                "-c:v",
                "png",
                "-threads",
                "1"
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    // Damage only the final video packet's zlib header; container/probe metadata stays valid.
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes
        .windows(8)
        .rposition(|b| b == b"\x89PNG\r\n\x1a\n")
        .unwrap();
    let payload = last + bytes[last..].windows(4).position(|b| b == b"IDAT").unwrap() + 4;
    bytes[payload] = 255;
    std::fs::write(&path, bytes).unwrap();
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut p = VideoPlayer::open(&path, tools, Some(audio.control())).unwrap();
    let clock = p.clock();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut decoded = false;
    loop {
        match p.poll() {
            Ok(Some(frame)) => {
                assert!(frame.rgba[2] > 240);
                decoded = true;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        audio.mix(&mut vec![0.; 960]).unwrap();
        thread::sleep(Duration::from_millis(2));
        assert!(Instant::now() < deadline);
    }
    assert!(decoded);
    assert_eq!(p.snapshot().state, PlaybackState::Failed);
    assert!(p.snapshot().error.is_some());
    assert_eq!(clock.seconds(), None);
    assert_eq!(audio.voice_count(), 0);
    assert!(p.poll().is_err());
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn silent_monotonic_clock_reaches_exact_eof() {
    let (_dir, path, tools) = fixture(false);
    let mut p = VideoPlayer::open(&path, tools, None).unwrap();
    let clock = p.clock();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut previous = 0.;
    let mut final_blue = false;
    while p.snapshot().state != PlaybackState::Ended {
        let now = clock.seconds().unwrap();
        assert!(now >= previous);
        previous = now;
        if let Some(frame) = p.poll().unwrap() {
            final_blue = frame.rgba[2] > 240;
        }
        thread::sleep(Duration::from_millis(2));
        assert!(Instant::now() < deadline);
    }
    assert!(final_blue);
    assert_eq!(clock.seconds(), Some(p.info().duration));
}
