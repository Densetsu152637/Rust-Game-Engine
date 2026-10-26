use engine_project::{
    captions::{Captions, PlayOptions, parse_caption_track},
    rendering3d::TextureStreams,
    video::{VideoPlayerComponent, caption_clock, poll_texture},
};
use engine_video::{DecoderTools, VideoPlayer};
use std::{
    process::Command,
    thread,
    time::{Duration, Instant},
};
#[test]
fn video_component_rejects_invalid_identifiers() {
    assert!(
        VideoPlayerComponent {
            stream: "bad/name".into(),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(VideoPlayerComponent::default().validate().is_ok());
    assert!(
        VideoPlayerComponent {
            asset: "asset:intro".into(),
            ..Default::default()
        }
        .validate()
        .is_ok()
    );
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn real_video_texture_and_captions_share_seek_pause_clock() {
    let tools = DecoderTools {
        ffmpeg: std::env::var_os("ENGINE_VIDEO_FFMPEG").unwrap().into(),
        ffprobe: std::env::var_os("ENGINE_VIDEO_FFPROBE").unwrap().into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mkv");
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
                "ffv1",
                "-threads",
                "1"
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let mut audio =
        engine_audio::AudioEngine::offline(engine_audio::AudioConfig::default()).unwrap();
    let mut p = VideoPlayer::open(&path, tools, Some(audio.control())).unwrap();
    p.pause();
    let mut streams = TextureStreams::default();
    streams.create_cpu("movie", 16, 16).unwrap();
    let mut captions = Captions::default();
    captions.insert_track("track",parse_caption_track(br#"{"version":1,"speakers":[],"cues":[{"id":"cue","start":0.5,"end":1.0,"text":"Blue"}]}"#).unwrap()).unwrap();
    let id = captions
        .play(
            "track",
            PlayOptions {
                clock: Some(caption_clock(&p)),
                ..Default::default()
            },
        )
        .unwrap();
    captions.advance(10.);
    assert!(captions.active().is_empty());
    p.seek(0.5).unwrap();
    captions.advance(0.);
    assert_eq!(captions.position(id), Some(0.5));
    assert_eq!(captions.active()[0].text, "Blue");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !poll_texture(&mut p, &mut streams, "movie").unwrap() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let frame = streams.get("movie").unwrap();
    assert_eq!(frame.revision, 1);
    assert!(frame.texture.pixels[2] > 240);
    assert!(frame.texture.pixels[0] < 10);
    captions.advance(10.);
    assert_eq!(captions.position(id), Some(0.5));
    p.resume();
    let deadline = Instant::now() + Duration::from_secs(5);
    while p.clock().seconds().unwrap() < 0.55 {
        audio.mix(&mut vec![0.; 960]).unwrap();
        p.poll().unwrap();
        thread::sleep(Duration::from_millis(2));
        assert!(Instant::now() < deadline);
    }
    captions.advance(0.);
    assert_eq!(captions.position(id), p.clock().seconds());
    let held = captions.position(id);
    p.stop();
    captions.advance(10.);
    assert_eq!(captions.position(id), held);
}
#[test]
#[ignore = "requires explicitly configured real FFmpeg and ffprobe"]
fn authored_video_upload_and_owner_removal_close_playback() {
    use engine_project::{ActionFrame, Project, Runtime, video::RuntimeVideos};
    use serde_json::json;
    let tools = DecoderTools {
        ffmpeg: std::env::var_os("ENGINE_VIDEO_FFMPEG").unwrap().into(),
        ffprobe: std::env::var_os("ENGINE_VIDEO_FFPROBE").unwrap().into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("clip.mkv");
    assert!(
        Command::new(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=blue:s=16x16:r=30:d=1",
                "-c:v",
                "ffv1",
                "-threads",
                "1"
            ])
            .arg(&media)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(dir.path().join("scene.json"),json!({"schema":1,"scene_id":"main","entities":[{"id":"movie","name":"Movie","components":{"custom":{"engine.VideoPlayer":{"version":1,"data":{"asset":"intro","stream":"movie","captions":"captions"}}}}}]}).to_string()).unwrap();
    std::fs::write(dir.path().join("project.json"),json!({"schema":1,"name":"video-tests","startup_scene":"scene.json","assets":[{"id":"intro","kind":"video","path":"clip.mkv"},{"id":"captions","kind":"caption_track","path":"captions.json"}]}).to_string()).unwrap();
    std::fs::write(dir.path().join("captions.json"),json!({"version":1,"speakers":[],"cues":[{"id":"blue","start":0.0,"end":1.0,"text":"Blue"}]}).to_string()).unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
    let mut runtime = Runtime::new_with_simulation(project.clone(), None).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(RuntimeVideos::new(tools.clone()));
    runtime.start().unwrap();
    let entity = {
        let world = runtime.app_mut().world_mut();
        world
            .query::<(bevy_ecs::prelude::Entity, &VideoPlayerComponent)>()
            .iter(world)
            .next()
            .unwrap()
            .0
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        runtime.tick(0.01, ActionFrame::default()).unwrap();
        runtime
            .app_mut()
            .world_mut()
            .resource_mut::<RuntimeVideos>()
            .player_mut(entity)
            .unwrap()
            .pause();
        if runtime
            .app()
            .world()
            .resource::<TextureStreams>()
            .get("movie")
            .is_some_and(|f| f.revision > 0)
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<RuntimeVideos>()
        .player_mut(entity)
        .unwrap()
        .stop();
    runtime.tick(0.01, ActionFrame::default()).unwrap();
    assert!(
        runtime
            .app()
            .world()
            .resource::<Captions>()
            .active()
            .is_empty()
    );
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<RuntimeVideos>()
        .player_mut(entity)
        .unwrap()
        .seek(0.5)
        .unwrap();
    runtime.tick(0.01, ActionFrame::default()).unwrap();
    assert!(
        !runtime
            .app()
            .world()
            .resource::<Captions>()
            .active()
            .is_empty()
    );
    runtime.app_mut().world_mut().despawn(entity);
    runtime.tick(0.01, ActionFrame::default()).unwrap();
    assert!(
        runtime
            .app()
            .world()
            .resource::<TextureStreams>()
            .get("movie")
            .is_none()
    );
    assert!(
        runtime
            .app()
            .world()
            .resource::<RuntimeVideos>()
            .player(entity)
            .is_none()
    );
    runtime.stop().unwrap();
    assert!(!runtime.app().world().contains_resource::<RuntimeVideos>());
    // Runtime::tick intentionally shuts the entire runtime down on any error.
    // Recoverable seek is tested on standalone VideoPlayer; here verify fatal cleanup.
    let mut runtime = Runtime::new_with_simulation(project, None).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(RuntimeVideos::new(tools));
    runtime.start().unwrap();
    runtime.tick(0.01, ActionFrame::default()).unwrap();
    let entity = {
        let world = runtime.app_mut().world_mut();
        world
            .query::<(bevy_ecs::prelude::Entity, &VideoPlayerComponent)>()
            .iter(world)
            .next()
            .unwrap()
            .0
    };
    let caption_id = runtime.app().world().resource::<Captions>().active()[0]
        .playback
        .unwrap();
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<RuntimeVideos>()
        .player_mut(entity)
        .unwrap()
        .stop();
    let renamed = media.with_extension("moved");
    std::fs::rename(&media, &renamed).unwrap();
    assert!(
        runtime
            .app_mut()
            .world_mut()
            .resource_mut::<RuntimeVideos>()
            .player_mut(entity)
            .unwrap()
            .seek(0.5)
            .is_err()
    );
    assert!(runtime.tick(0.01, ActionFrame::default()).is_err());
    assert!(
        runtime
            .app()
            .world()
            .resource::<Captions>()
            .position(caption_id)
            .is_none()
    );
    assert!(!runtime.app().world().contains_resource::<RuntimeVideos>());
    assert!(
        runtime
            .app()
            .world()
            .resource::<TextureStreams>()
            .get("movie")
            .is_none()
    );
    std::fs::rename(&renamed, &media).unwrap();
    assert!(runtime.start().is_err());
}
