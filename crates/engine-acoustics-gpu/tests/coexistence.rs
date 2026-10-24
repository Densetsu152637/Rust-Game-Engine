//! GPU propagation alongside rendering in the same process on the same GPU
//! (docs/spatial-audio-design.md test 20, library-level measurement). A separate
//! wgpu device renders 1920x1080 offscreen frames (submit plus wait) while the
//! acoustics tier traces on its own device from a worker thread. The desktop
//! measurement with the real renderer belongs to the late integration.
//! Ignored by default; run with
//! `cargo test -p engine-acoustics-gpu --release --test coexistence -- --ignored --nocapture`.

mod common;

use common::*;
use engine_acoustics::PropagationBackend;
use engine_acoustics_gpu::{GpuBackend, GpuOptions};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const SHADER: &str = r#"
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    var c = vec3<f32>(pos.xy * 0.001, 0.5);
    for (var i = 0; i < 48; i += 1) {
        c = fract(sin(c * 12.9898 + vec3<f32>(f32(i))) * 43758.5453);
    }
    return vec4<f32>(c, 1.0);
}
"#;

struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    view: wgpu::TextureView,
}

impl Renderer {
    fn new() -> Self {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .expect("render adapter");
        println!("render adapter: {:?}", adapter.get_info().name);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("render device");
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 1920,
                height: 1080,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Self {
            device,
            queue,
            pipeline,
            view,
        }
    }

    fn frame(&self) -> Duration {
        let start = Instant::now();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.draw(0..3, 0..1);
        }
        let index = self.queue.submit(Some(encoder.finish()));
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: Some(Duration::from_secs(5)),
            })
            .expect("render frame");
        start.elapsed()
    }

    /// Frame times in ms for `frames` frames, sorted.
    fn measure(&self, frames: usize) -> Vec<f64> {
        let mut times: Vec<f64> = (0..frames)
            .map(|_| self.frame().as_secs_f64() * 1e3)
            .collect();
        times.sort_by(f64::total_cmp);
        times
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Runs the tracer on a worker until `stop`, `rays` x `bounces` every `period`.
fn tracer(
    stop: Arc<AtomicBool>,
    updates: Arc<AtomicU64>,
    rays: u32,
    bounces: u16,
    period: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let world = shoebox_world(REFERENCE_ALPHA, 1.0, 1);
        let mut backend = GpuBackend::ray_query(GpuOptions {
            readback_timeout_ms: 2_000,
            ..Default::default()
        });
        backend.initialize().expect("ray-query tier");
        backend.sync_world(&world).expect("sync");
        let settings = settings(2.0);
        let mut out = Vec::new();
        let mut seed = 1;
        while !stop.load(Ordering::Relaxed) {
            let start = Instant::now();
            let jobs = [job([3.1, 2.2, 4.3], [3.1, 2.2, 4.3], rays, bounces, seed)];
            backend
                .trace(&world, &settings, &jobs, &mut out)
                .expect("trace");
            updates.fetch_add(1, Ordering::Relaxed);
            seed += 1;
            if let Some(rest) = period.checked_sub(start.elapsed()) {
                std::thread::sleep(rest);
            }
        }
    })
}

fn with_tracer(
    renderer: &Renderer,
    rays: u32,
    bounces: u16,
    period: Duration,
    frames: usize,
) -> (Vec<f64>, u64) {
    let stop = Arc::new(AtomicBool::new(false));
    let updates = Arc::new(AtomicU64::new(0));
    let handle = tracer(stop.clone(), updates.clone(), rays, bounces, period);
    // Let the tracer initialize (self-test) before measuring.
    while updates.load(Ordering::Relaxed) == 0 {
        std::thread::sleep(Duration::from_millis(5));
    }
    let times = renderer.measure(frames);
    stop.store(true, Ordering::Relaxed);
    handle.join().expect("tracer thread");
    (times, updates.load(Ordering::Relaxed))
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn test20_tracing_beside_rendering_costs_at_most_1ms_p95() {
    const FRAMES: usize = 300;
    let renderer = Renderer::new();
    renderer.measure(30); // warm-up
    let baseline = renderer.measure(FRAMES);
    // Defaults: 4,096 rays x 64 bounces, late field at 4 Hz.
    let (defaults, default_updates) =
        with_tracer(&renderer, 4096, 64, Duration::from_millis(250), FRAMES);
    // Stress: 65,536 rays x 256 bounces back to back.
    let (stress, stress_updates) = with_tracer(&renderer, 65_536, 256, Duration::ZERO, FRAMES);
    let report = |label: &str, t: &[f64]| {
        println!(
            "{label}: median {:.3} ms, p95 {:.3} ms, max {:.3} ms",
            percentile(t, 0.5),
            percentile(t, 0.95),
            t[t.len() - 1]
        );
    };
    report("render only", &baseline);
    report(
        &format!("with default tracer ({default_updates} updates)"),
        &defaults,
    );
    report(
        &format!("with stress tracer ({stress_updates} updates)"),
        &stress,
    );
    let increase = percentile(&defaults, 0.95) - percentile(&baseline, 0.95);
    println!(
        "p95 increase: defaults {increase:.3} ms, stress {:.3} ms",
        percentile(&stress, 0.95) - percentile(&baseline, 0.95)
    );
    assert!(
        increase <= 1.0,
        "default tracer adds {increase:.3} ms at p95"
    );
}
