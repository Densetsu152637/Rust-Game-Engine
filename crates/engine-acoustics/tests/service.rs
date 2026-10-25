//! Tests 16 (CPU determinism), 19 (backend state machine with fake backends) and
//! 8 (service half: the audio thread never waits for propagation).

use engine_acoustics::reference::*;
use engine_acoustics::*;
use engine_audio::{AudioConfig, AudioEngine, Clip, PlayOptions};
use std::result::Result;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn shared_types_are_send_and_sync() {
    assert_send_sync::<AcousticsService>();
    assert_send_sync::<AcousticWorld>();
    assert_send_sync::<ProxyMapping>();
    assert_send_sync::<PropagationResult>();
    assert_send_sync::<AcousticStatus>();
}

fn audio() -> (AudioEngine, Vec<engine_audio::VoiceHandle>) {
    let engine = AudioEngine::offline(AudioConfig::default()).unwrap();
    let clip = Clip::new(48_000, 1, vec![0.25; 4800]).unwrap();
    let voices = (0..2)
        .map(|_| {
            engine
                .control()
                .play(
                    clip.clone(),
                    PlayOptions {
                        looped: true,
                        ..PlayOptions::default()
                    },
                )
                .unwrap()
        })
        .collect();
    (engine, voices)
}

fn request(voices: &[engine_audio::VoiceHandle], reverb_rays: u32) -> PropagationRequest {
    let positions = [SHOEBOX_SOURCE, [8.5, 4.0, 6.5]];
    PropagationRequest {
        sequence: 7,
        listener: ListenerPose {
            position: SHOEBOX_LISTENER,
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
        },
        sources: voices
            .iter()
            .zip(positions)
            .map(|(v, position)| SourceRequest {
                id: v.get(),
                voice: Some(*v),
                position,
                radius_m: 0.25,
                occlusion: true,
                reflections: true,
                reverb: true,
                hero: false,
                priority: 1.0,
            })
            .collect(),
        settings: PropagationSettings {
            reverb_rays,
            histogram_length_s: 1.0,
            ..PropagationSettings::default()
        },
    }
}

fn config(
    mode: ServiceMode,
    preference: BackendPreference,
    backends: Vec<Box<dyn PropagationBackend>>,
) -> ServiceConfig {
    ServiceConfig {
        preference,
        mode,
        update_hz: 15.0,
        reverb_update_hz: 5.0,
        settings: PropagationSettings::default(),
        backends,
    }
}

/// A frame digest captured by the parameter hook.
type Frames = Arc<Mutex<Vec<String>>>;
fn capture(frames: &Frames) -> ParamHook {
    let frames = frames.clone();
    Box::new(move |_result, frame| {
        let mut s = format!("{:?}", frame.reverb);
        for v in &frame.voices {
            s += &format!(
                "|{} {:?} {:?} {}",
                v.voice.get(),
                v.direct,
                v.direction,
                v.tap_count
            );
            for t in &v.taps[..usize::from(v.tap_count)] {
                s += &format!(" {:?}", t);
            }
        }
        frames.lock().unwrap().push(s);
    })
}

fn deterministic_run(
    world: &AcousticWorld,
    voices: &[engine_audio::VoiceHandle],
    publisher: engine_audio::acoustic::AcousticPublisher,
) -> (Vec<PropagationResult>, Vec<String>) {
    let mut service = AcousticsService::new(
        config(ServiceMode::Synchronous, BackendPreference::Cpu, Vec::new()),
        publisher,
    )
    .unwrap();
    let frames = Frames::default();
    service.set_param_hook(Some(capture(&frames)));
    service.set_world(world.clone());
    service.submit(request(voices, 8192)).unwrap();
    let mut results = Vec::new();
    for _ in 0..4 {
        let mut r = service.step_blocking().unwrap().clone();
        r.stats.elapsed_us = 0; // Wall-clock time is the only non-deterministic field.
        r.budget_exceeded = false;
        results.push(r);
    }
    let status = service.status();
    assert_eq!(status.state, AcousticState::Active);
    assert_eq!(status.active, Some(BackendKind::Cpu));
    assert_eq!(status.publishes, 4);
    let frames = frames.lock().unwrap().clone();
    (results, frames)
}

#[test]
fn synchronous_cpu_runs_are_bitwise_identical() {
    let world = reference_shoebox(0.3).unwrap();
    let (audio_a, voices_a) = audio();
    let (audio_b, voices_b) = audio();
    // Both engines hand out the same handle values (process-wide counter aside), so
    // compare through the voice order rather than raw ids.
    let (a, fa) = deterministic_run(
        &world,
        &voices_a,
        audio_a.control().acoustic_publisher().unwrap(),
    );
    let (b, fb) = deterministic_run(
        &world,
        &voices_b,
        audio_b.control().acoustic_publisher().unwrap(),
    );
    let strip =
        |rs: &[PropagationResult], vs: &[engine_audio::VoiceHandle]| -> Vec<PropagationResult> {
            rs.iter()
                .cloned()
                .map(|mut r| {
                    for s in &mut r.sources {
                        s.id = vs.iter().position(|v| v.get() == s.id).unwrap() as u64;
                    }
                    r
                })
                .collect()
        };
    assert_eq!(strip(&a, &voices_a), strip(&b, &voices_b));
    let strip_frames = |fs: &[String], vs: &[engine_audio::VoiceHandle]| -> Vec<String> {
        fs.iter()
            .map(|f| {
                let mut f = f.clone();
                for (i, v) in vs.iter().enumerate() {
                    f = f.replace(&format!("|{} ", v.get()), &format!("|v{i} "));
                }
                f
            })
            .collect()
    };
    assert_eq!(strip_frames(&fa, &voices_a), strip_frames(&fb, &voices_b));
    // The late field is traced on steps 0 and 3 (every round(15 / 5) = 3 steps).
    let traced: Vec<bool> = a.iter().map(|r| r.listener_field.is_some()).collect();
    assert_eq!(traced, [true, false, false, true]);
    assert!(a[0].reverb.enabled);
    assert_eq!(a[0].sources.len(), 2);
    assert!(a[0].sources[0].reflections.len() <= 8);
    assert!(!a[0].sources[0].reflections.is_empty());
    assert!(fa[0].contains(&format!("|{} ", voices_a[0].get())));
    // A different seed changes the late field.
    let mut other = request(&voices_a, 8192);
    other.settings.seed = 2;
    let (audio_c, _) = audio();
    let mut service = AcousticsService::new(
        config(ServiceMode::Synchronous, BackendPreference::Cpu, Vec::new()),
        audio_c.control().acoustic_publisher().unwrap(),
    )
    .unwrap();
    service.set_world(world.clone());
    service.submit(other).unwrap();
    let c = service.step_blocking().unwrap();
    assert_ne!(c.listener_field, a[0].listener_field);
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Behaviour {
    Ok,
    Fatal,
    Timeout,
    Sleep(Duration),
}

/// A fake GPU tier that traces with the CPU so its results are real.
struct Fake {
    kind: BackendKind,
    init: Result<(), String>,
    behaviour: Arc<Mutex<Behaviour>>,
    inits: Arc<AtomicU32>,
    cpu: CpuBackend,
}
impl Fake {
    fn new(
        kind: BackendKind,
        init: Result<(), &str>,
    ) -> (Self, Arc<Mutex<Behaviour>>, Arc<AtomicU32>) {
        let behaviour = Arc::new(Mutex::new(Behaviour::Ok));
        let inits = Arc::new(AtomicU32::new(0));
        (
            Self {
                kind,
                init: init.map_err(str::to_owned),
                behaviour: behaviour.clone(),
                inits: inits.clone(),
                cpu: CpuBackend::new(2).unwrap(),
            },
            behaviour,
            inits,
        )
    }
}
impl PropagationBackend for Fake {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            kind: self.kind,
            hardware_rt: self.kind == BackendKind::GpuRayQuery,
            adapter: Some("Fake GPU".into()),
            driver: None,
        }
    }
    fn initialize(&mut self) -> Result<(), BackendError> {
        self.inits.fetch_add(1, Ordering::SeqCst);
        self.init.clone().map_err(|message| BackendError {
            code: "acoustics.backend.unavailable",
            message,
            fatal: false,
        })
    }
    fn sync_world(&mut self, world: &AcousticWorld) -> Result<(), BackendError> {
        self.cpu.sync_world(world)
    }
    fn trace(
        &mut self,
        world: &AcousticWorld,
        settings: &TraceSettings,
        jobs: &[EnergyJob],
        out: &mut Vec<EnergyHistogram>,
    ) -> Result<TraceStats, BackendError> {
        let behaviour = *self.behaviour.lock().unwrap();
        match behaviour {
            Behaviour::Ok => self.cpu.trace(world, settings, jobs, out),
            Behaviour::Fatal => Err(BackendError {
                code: "acoustics.backend.failed",
                message: "device lost".into(),
                fatal: true,
            }),
            Behaviour::Timeout => Err(BackendError {
                code: "acoustics.backend.timeout",
                message: "readback timed out after 100 ms".into(),
                fatal: false,
            }),
            Behaviour::Sleep(d) => {
                std::thread::sleep(d);
                self.cpu.trace(world, settings, jobs, out)
            }
        }
    }
}

fn sync_service(
    preference: BackendPreference,
    backends: Vec<Box<dyn PropagationBackend>>,
) -> (AcousticsService, AudioEngine) {
    let audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut service = AcousticsService::new(
        ServiceConfig {
            reverb_update_hz: 15.0,
            ..config(ServiceMode::Synchronous, preference, backends)
        },
        audio.control().acoustic_publisher().unwrap(),
    )
    .unwrap();
    service.set_world(reference_shoebox(0.5).unwrap());
    service.submit(request(&[], 1024)).unwrap();
    (service, audio)
}

#[test]
fn denied_ray_query_falls_back_to_compute_with_the_reason() {
    let (rq, _, _) = Fake::new(
        BackendKind::GpuRayQuery,
        Err("adapter lacks EXPERIMENTAL_RAY_QUERY (deny_ray_query test hook)"),
    );
    let (compute, _, _) = Fake::new(BackendKind::GpuCompute, Ok(()));
    let (mut service, _audio) = sync_service(
        BackendPreference::GpuRayQuery,
        vec![Box::new(rq), Box::new(compute)],
    );
    assert_eq!(service.status().state, AcousticState::Starting);
    let r = service.step_blocking().unwrap();
    assert_eq!(r.backend, BackendKind::GpuCompute);
    let status = service.status();
    assert_eq!(status.state, AcousticState::Fallback);
    assert_eq!(status.active, Some(BackendKind::GpuCompute));
    assert!(!status.hardware_rt);
    assert!(
        status
            .fallback_reason
            .unwrap()
            .contains("EXPERIMENTAL_RAY_QUERY")
    );
}

#[test]
fn self_test_failure_falls_back_to_cpu_naming_the_step() {
    let (rq, _, _) = Fake::new(
        BackendKind::GpuRayQuery,
        Err("self-test step quad_hits failed: 39 of 40 hits"),
    );
    let (compute, _, _) = Fake::new(
        BackendKind::GpuCompute,
        Err("self-test step shoebox_mean_free_path failed: 3.1% > 2%"),
    );
    let (mut service, _audio) = sync_service(
        BackendPreference::Auto,
        vec![Box::new(rq), Box::new(compute)],
    );
    assert_eq!(service.step_blocking().unwrap().backend, BackendKind::Cpu);
    let status = service.status();
    assert_eq!(status.state, AcousticState::Fallback);
    let reason = status.fallback_reason.unwrap();
    assert!(
        reason.contains("quad_hits") && reason.contains("shoebox_mean_free_path"),
        "{reason}"
    );
}

#[test]
fn three_timeouts_demote_and_parameters_keep_publishing() {
    let (rq, behaviour, _) = Fake::new(BackendKind::GpuRayQuery, Ok(()));
    let (mut service, _audio) = sync_service(BackendPreference::GpuRayQuery, vec![Box::new(rq)]);
    assert_eq!(
        service.step_blocking().unwrap().backend,
        BackendKind::GpuRayQuery
    );
    assert_eq!(service.status().state, AcousticState::Active);
    assert!(service.status().hardware_rt);
    *behaviour.lock().unwrap() = Behaviour::Timeout;
    for step in 0..2 {
        let r = service.step_blocking().unwrap();
        assert_eq!(r.backend, BackendKind::GpuRayQuery, "step {step}");
        assert!(
            r.listener_field.is_none(),
            "a timed-out late field keeps the last one"
        );
    }
    assert!(service.drain_events().is_empty());
    let r = service.step_blocking().unwrap();
    assert_eq!(r.backend, BackendKind::Cpu);
    assert!(
        r.listener_field.is_some(),
        "the CPU traced the late field in the same step"
    );
    let status = service.status();
    assert_eq!(status.gpu_timeouts, 3);
    assert_eq!(status.publishes, 4, "every step published");
    assert_eq!(status.state, AcousticState::Fallback);
    let events = service.drain_events();
    assert!(
        matches!(
            &events[..],
            [ServiceEvent::BackendChanged { from: Some(BackendKind::GpuRayQuery), to: Some(BackendKind::Cpu), reason }]
                if reason.contains("timed out")
        ),
        "{events:?}"
    );
}

#[test]
fn fatal_errors_demote_immediately_and_preference_reprobes() {
    let (rq, behaviour, inits) = Fake::new(BackendKind::GpuRayQuery, Ok(()));
    let (mut service, _audio) = sync_service(BackendPreference::Auto, vec![Box::new(rq)]);
    *behaviour.lock().unwrap() = Behaviour::Fatal;
    assert_eq!(service.step_blocking().unwrap().backend, BackendKind::Cpu);
    assert!(
        service
            .status()
            .fallback_reason
            .unwrap()
            .contains("device lost")
    );
    // No automatic promotion; an explicit re-probe returns to the GPU tier.
    *behaviour.lock().unwrap() = Behaviour::Ok;
    assert_eq!(service.step_blocking().unwrap().backend, BackendKind::Cpu);
    service.set_preference(BackendPreference::Auto).unwrap();
    assert_eq!(inits.load(Ordering::SeqCst), 2);
    assert_eq!(
        service.step_blocking().unwrap().backend,
        BackendKind::GpuRayQuery
    );
}

#[test]
fn cpu_preference_never_initializes_gpu_backends() {
    let (rq, _, rq_inits) = Fake::new(BackendKind::GpuRayQuery, Ok(()));
    let (compute, _, compute_inits) = Fake::new(BackendKind::GpuCompute, Ok(()));
    let (mut service, _audio) = sync_service(
        BackendPreference::Cpu,
        vec![Box::new(rq), Box::new(compute)],
    );
    assert_eq!(service.step_blocking().unwrap().backend, BackendKind::Cpu);
    assert_eq!(service.status().state, AcousticState::Active);
    assert_eq!(
        rq_inits.load(Ordering::SeqCst) + compute_inits.load(Ordering::SeqCst),
        0
    );
}

#[test]
fn requests_are_validated_before_queueing() {
    let (mut service, _audio) = sync_service(BackendPreference::Cpu, Vec::new());
    let mut bad = request(&[], 1024);
    bad.settings.max_reflections = 17;
    assert_eq!(
        service.submit(bad).unwrap_err().code,
        "acoustics.request.invalid"
    );
    let mut bad = request(&[], 1024);
    bad.listener.forward = [0.0, 1.0, 0.0];
    assert_eq!(
        service.submit(bad).unwrap_err().code,
        "acoustics.request.invalid"
    );
    let mut bad = request(&[], 1024);
    bad.sources = vec![
        SourceRequest {
            id: 1,
            voice: None,
            position: [1.0; 3],
            radius_m: 0.0,
            occlusion: true,
            reflections: false,
            reverb: false,
            hero: false,
            priority: 1.0,
        };
        2
    ];
    assert_eq!(
        service.submit(bad).unwrap_err().code,
        "acoustics.request.invalid"
    );
    // Threaded-only and synchronous-only calls are rejected.
    let audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut threaded = AcousticsService::new(
        config(ServiceMode::Threaded, BackendPreference::Cpu, Vec::new()),
        audio.control().acoustic_publisher().unwrap(),
    )
    .unwrap();
    assert_eq!(
        threaded.step_blocking().unwrap_err().code,
        "acoustics.request.invalid"
    );
    threaded.shutdown();
}

#[test]
fn a_sleeping_backend_never_blocks_mixing_or_submits() {
    let (rq, behaviour, _) = Fake::new(BackendKind::GpuCompute, Ok(()));
    *behaviour.lock().unwrap() = Behaviour::Sleep(Duration::from_millis(500));
    let mut audio = AudioEngine::offline(AudioConfig::default()).unwrap();
    let mut service = AcousticsService::new(
        ServiceConfig {
            reverb_update_hz: 15.0,
            ..config(
                ServiceMode::Threaded,
                BackendPreference::GpuCompute,
                vec![Box::new(rq)],
            )
        },
        audio.control().acoustic_publisher().unwrap(),
    )
    .unwrap();
    service.set_world(reference_shoebox(0.5).unwrap());
    service.submit(request(&[], 1024)).unwrap();
    std::thread::sleep(Duration::from_millis(50)); // The worker is now inside trace.
    let mut buffer = vec![0.0f32; 1024];
    let mut worst = Duration::ZERO;
    for i in 0..200 {
        let t = Instant::now();
        audio.mix(&mut buffer).unwrap();
        worst = worst.max(t.elapsed());
        if i % 50 == 0 {
            let t = Instant::now();
            service.submit(request(&[], 1024)).unwrap();
            let _ = service.latest();
            let _ = service.status();
            assert!(
                t.elapsed() < Duration::from_millis(5),
                "service calls never wait"
            );
        }
    }
    println!("worst mix of 512 frames while tracing: {worst:?}");
    assert!(worst < Duration::from_millis(5));
    // Parameters publish once the slow trace finishes.
    let deadline = Instant::now() + Duration::from_secs(10);
    while service.status().publishes == 0 {
        assert!(Instant::now() < deadline, "no publish after the trace");
        std::thread::sleep(Duration::from_millis(20));
    }
    let summary = loop {
        if let Some(s) = service.latest() {
            break s;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(summary.backend, BackendKind::GpuCompute);
    service.shutdown();
    assert_eq!(service.status().state, AcousticState::Disabled);
}
