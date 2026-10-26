use crate::{DecoderTools, MediaInfo, Result, VideoError, VideoFrame};
use engine_audio::{AudioError, StreamWriter};
use std::{
    io::{ErrorKind, Read},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const FORMATS: &str = "mov,matroska,webm";
fn command(executable: &Path) -> Command {
    let mut c = Command::new(executable);
    c.stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    c
}
fn input(c: &mut Command, path: &Path) {
    c.args([
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        FORMATS,
        "-i",
    ])
    .arg(path);
}
struct ProbeProcess(Child);
impl Drop for ProbeProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
pub(crate) fn probe(tools: &DecoderTools, path: &Path) -> Result<MediaInfo> {
    let mut c = command(&tools.ffprobe);
    c.args(["-v", "error", "-max_alloc", "67108864"]);
    input(&mut c, path);
    c.args([
        "-show_entries",
        "stream=codec_type,width,height,start_time,duration:format=format_name,start_time,duration",
        "-of",
        "json",
    ])
    .stdout(Stdio::piped());
    let mut child = ProbeProcess(
        c.spawn()
            .map_err(|e| VideoError(format!("Cannot start configured ffprobe: {e}")))?,
    );
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| VideoError("Missing probe pipe".into()))?;
    let read = thread::Builder::new()
        .name("media-probe".into())
        .spawn(move || {
            let mut data = vec![];
            stdout
                .by_ref()
                .take(65537)
                .read_to_end(&mut data)
                .map(|_| data)
        })?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.0.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.0.kill();
                let _ = child.0.wait();
                let _ = read.join();
                return Err(error.into());
            }
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            let _ = child.0.wait();
            let _ = read.join();
            return Err(VideoError(
                "Media probe timed out or exceeded output limit".into(),
            ));
        }
        thread::sleep(Duration::from_millis(5));
    };
    let bytes = read
        .join()
        .map_err(|_| VideoError("Probe reader panicked".into()))??;
    if !status.success() || bytes.len() > 65536 {
        return Err(VideoError(
            "Unsupported or invalid local media (probe failed)".into(),
        ));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| VideoError(e.to_string()))?;
    let streams = v["streams"]
        .as_array()
        .ok_or_else(|| VideoError("No media streams".into()))?;
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or_else(|| VideoError("No video stream".into()))?;
    let width = video["width"].as_u64().unwrap_or(0);
    let height = video["height"].as_u64().unwrap_or(0);
    let start = v["format"]["start_time"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.);
    let video_start = video["start_time"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(start)
        - start;
    let reported_duration = v["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.);
    // Matroska Duration is the segment end timestamp. MOV reports elapsed duration;
    // prefer stream ends when all selected streams expose their timing.
    let selected: Vec<_> = [
        streams.iter().find(|s| s["codec_type"] == "video"),
        streams.iter().find(|s| s["codec_type"] == "audio"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let ends: Option<Vec<f64>> = selected
        .iter()
        .map(|s| {
            Some(
                s["start_time"].as_str()?.parse::<f64>().ok()?
                    + s["duration"].as_str()?.parse::<f64>().ok()?,
            )
        })
        .collect();
    let duration = if let Some(ends) = ends {
        ends.into_iter().fold(f64::NEG_INFINITY, f64::max) - start
    } else if v["format"]["format_name"]
        .as_str()
        .is_some_and(|f| f.contains("matroska"))
    {
        reported_duration - start
    } else {
        reported_duration
    };
    if !start.is_finite() {
        return Err(VideoError("Invalid container time origin".into()));
    }
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || width * height > 8_388_608
        || !video_start.is_finite()
        || video_start < 0.
        || video_start >= duration
        || !duration.is_finite()
        || duration <= 0.
        || duration > 86400.
    {
        return Err(VideoError(
            "Media exceeds limits: 4096px edge, 8M pixels, duration (0,86400] seconds".into(),
        ));
    }
    Ok(MediaInfo {
        width: width as u32,
        height: height as u32,
        duration,
        video_start,
        has_audio: streams.iter().any(|s| s["codec_type"] == "audio"),
    })
}
pub(crate) struct Workers {
    pub frames: Mutex<Receiver<VideoFrame>>,
    cancel: Arc<AtomicBool>,
    children: Vec<Arc<Mutex<Child>>>,
    threads: Vec<JoinHandle<()>>,
    error: Arc<Mutex<Option<String>>>,
}
fn fail(error: &Mutex<Option<String>>, message: String) {
    if let Ok(mut e) = error.lock()
        && e.is_none()
    {
        *e = Some(message);
    }
}
fn send(sender: &SyncSender<VideoFrame>, mut frame: VideoFrame, cancel: &AtomicBool) -> bool {
    loop {
        if cancel.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(frame) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Full(f)) => {
                frame = f;
                thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return false,
        }
    }
}
impl Workers {
    pub fn start(
        tools: &DecoderTools,
        path: &Path,
        info: &MediaInfo,
        offset: f64,
        writer: Option<StreamWriter>,
        tail: Arc<Mutex<Option<engine_audio::StreamSnapshot>>>,
    ) -> Result<Self> {
        let video_offset = (offset * 30.).floor() / 30.;
        let first_video = video_offset.max((info.video_start * 30.).round() / 30.);
        let (tx, frames) = mpsc::sync_channel(3);
        let mut w = Self {
            frames: Mutex::new(frames),
            cancel: Arc::new(AtomicBool::new(false)),
            children: vec![],
            threads: vec![],
            error: Arc::new(Mutex::new(None)),
        };
        let mut c = command(&tools.ffmpeg);
        c.args([
            "-nostdin",
            "-xerror",
            "-filter_threads",
            "1",
            "-v",
            "error",
            "-threads",
            "1",
            "-max_alloc",
            "67108864",
        ]);
        input(&mut c, path);
        c.args([
            "-ss",
            &video_offset.to_string(),
            "-t",
            &(info.duration - video_offset).to_string(),
            "-map",
            "0:v:0",
            "-an",
            "-vf",
            &format!(
                "scale={}:{}:flags=bilinear,tpad=stop_mode=clone:stop_duration={},fps=fps=30:start_time={}",
                info.width, info.height, info.duration, info.video_start
            ),
            "-fps_mode",
            "passthrough",
            "-threads",
            "1",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdout(Stdio::piped());
        let (child, mut out) = w.spawn(c)?;
        let cancel = w.cancel.clone();
        let error = w.error.clone();
        let size = info.width as usize * info.height as usize * 4;
        w.threads.push(
            thread::Builder::new()
                .name("media-decode".into())
                .spawn(move || {
                    let mut index = 0u64;
                    loop {
                        let mut rgba = vec![0; size];
                        match out.read(&mut rgba[..1]) {
                            Ok(0) => break,
                            Ok(_) => {}
                            Err(e) => {
                                if !cancel.load(Ordering::Acquire) {
                                    fail(&error, e.to_string());
                                }
                                return;
                            }
                        }
                        if let Err(e) = out.read_exact(&mut rgba[1..]) {
                            if !cancel.load(Ordering::Acquire) {
                                fail(&error, format!("Truncated video frame: {e}"));
                            }
                            return;
                        }
                        if !send(
                            &tx,
                            VideoFrame {
                                seconds: first_video + index as f64 / 30.,
                                rgba,
                            },
                            &cancel,
                        ) {
                            return;
                        }
                        index += 1;
                    }
                    if index == 0 && !cancel.load(Ordering::Acquire) {
                        fail(&error, "Decoder produced no video frames".into());
                    }
                    check_exit(&child, &cancel, &error, "video");
                })?,
        );
        if let Some(mut writer) = writer {
            let generation = writer.snapshot()?.generation;
            let mut c = command(&tools.ffmpeg);
            c.args([
                "-nostdin",
                "-xerror",
                "-filter_threads",
                "1",
                "-v",
                "error",
                "-threads",
                "1",
                "-max_alloc",
                "67108864",
            ]);
            input(&mut c, path);
            c.args([
                "-ss",
                &offset.to_string(),
                "-map",
                "0:a:0",
                "-vn",
                "-af",
                &format!(
                    "aresample=48000:async=1:first_pts=0,apad=whole_dur={}",
                    info.duration
                ),
                "-t",
                &(info.duration - offset).to_string(),
                "-ac",
                "2",
                "-ar",
                "48000",
                "-f",
                "f32le",
                "pipe:1",
            ])
            .stdout(Stdio::piped());
            let (child, mut out) = w.spawn(c)?;
            let cancel = w.cancel.clone();
            let error = w.error.clone();
            w.threads
                .push(
                    thread::Builder::new()
                        .name("media-decode".into())
                        .spawn(move || {
                            let result = (|| -> Result<()> {
                                loop {
                                    let mut bytes = [0u8; 8192];
                                    let mut count = 0;
                                    while count < bytes.len() {
                                        match out.read(&mut bytes[count..]) {
                                            Ok(0) => break,
                                            Ok(n) => count += n,
                                            Err(e) if e.kind() == ErrorKind::Interrupted => {
                                                continue;
                                            }
                                            Err(e) => return Err(e.into()),
                                        }
                                    }
                                    if count == 0 {
                                        break;
                                    }
                                    if count % 8 != 0 {
                                        return Err(VideoError("Truncated PCM frame".into()));
                                    }
                                    let pcm: Vec<f32> = bytes[..count]
                                        .as_chunks::<4>()
                                        .0
                                        .iter()
                                        .map(|b| f32::from_le_bytes(*b).clamp(-1., 1.))
                                        .collect();
                                    let mut at = 0;
                                    while at < pcm.len() {
                                        if cancel.load(Ordering::Acquire) {
                                            return Ok(());
                                        }
                                        let n = writer.write(generation, &pcm[at..])?;
                                        at += n * 2;
                                        if n == 0 {
                                            thread::sleep(Duration::from_millis(2));
                                        }
                                    }
                                }
                                check_exit(&child, &cancel, &error, "audio");
                                if let Some(message) = error.lock().ok().and_then(|e| e.clone()) {
                                    writer.fail(
                                        generation,
                                        AudioError {
                                            code: "video.decode",
                                            message,
                                        },
                                    )?;
                                } else {
                                    writer.finish(generation)?;
                                    while !cancel.load(Ordering::Acquire) {
                                        let snapshot = writer.snapshot()?;
                                        let done = matches!(
                                            snapshot.state,
                                            engine_audio::StreamState::Drained
                                                | engine_audio::StreamState::Stopped
                                                | engine_audio::StreamState::Failed
                                        );
                                        if let Ok(mut t) = tail.lock() {
                                            *t = Some(snapshot);
                                        }
                                        if done {
                                            break;
                                        }
                                        thread::sleep(Duration::from_millis(2));
                                    }
                                }
                                Ok(())
                            })();
                            if let Err(e) = result
                                && !cancel.load(Ordering::Acquire)
                            {
                                fail(&error, e.to_string());
                                let _ = writer.fail(
                                    generation,
                                    AudioError {
                                        code: "video.decode",
                                        message: e.to_string(),
                                    },
                                );
                            }
                        })?,
                );
        }
        Ok(w)
    }
    fn spawn(&mut self, mut c: Command) -> Result<(Arc<Mutex<Child>>, std::process::ChildStdout)> {
        let mut child = c
            .spawn()
            .map_err(|e| VideoError(format!("Cannot start configured ffmpeg: {e}")))?;
        let out = match child.stdout.take() {
            Some(out) => out,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(VideoError("Missing decoder pipe".into()));
            }
        };
        let child = Arc::new(Mutex::new(child));
        self.children.push(child.clone());
        Ok((child, out))
    }
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        for child in &self.children {
            if let Ok(mut c) = child.lock() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        self.children.clear();
    }
}
fn check_exit(
    child: &Mutex<Child>,
    cancel: &AtomicBool,
    error: &Mutex<Option<String>>,
    stream: &str,
) {
    while !cancel.load(Ordering::Acquire) {
        let status = match child.lock() {
            Ok(mut c) => c.try_wait(),
            Err(_) => {
                fail(error, "Decoder process lock poisoned".into());
                return;
            }
        };
        match status {
            Ok(Some(status)) => {
                if !status.success() {
                    fail(
                        error,
                        format!("FFmpeg {stream} decoder exited with {status}"),
                    );
                }
                return;
            }
            Ok(None) => thread::sleep(Duration::from_millis(2)),
            Err(e) => {
                fail(error, e.to_string());
                return;
            }
        }
    }
}
impl Drop for Workers {
    fn drop(&mut self) {
        self.stop();
    }
}
