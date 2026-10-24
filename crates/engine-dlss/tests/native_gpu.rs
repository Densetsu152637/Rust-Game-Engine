#![cfg(ngx_available)]
use engine_dlss::{Frame, Quality, Session, SessionOptions};

#[test]
#[ignore = "requires official NGX SDK/runtime and an NVIDIA Vulkan GPU; set NGX_RUNTIME_DIR and NGX_LOG_DIR"]
fn evaluates_real_dlss_super_resolution() {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(descriptor);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("Vulkan adapter");
    let options = SessionOptions {
        project_id: "c8091997-e7fa-48f6-8ef7-bf2211936fb5".into(),
        runtime_dir: std::env::var_os("NGX_RUNTIME_DIR")
            .expect("NGX_RUNTIME_DIR")
            .into(),
        log_dir: std::env::var_os("NGX_LOG_DIR").expect("NGX_LOG_DIR").into(),
        output_size: [768, 512],
        quality: Quality::Quality,
    };
    let (device, queue) =
        engine_dlss::create_device(&adapter, &wgpu::DeviceDescriptor::default(), &options)
            .expect("DLSS-compatible Vulkan device");
    // SAFETY: sole NGX integration, matched queue/device and the instance remains
    // alive; all rendering and teardown are serialized in this test.
    let mut session = Session::new(&device, &queue, options).expect("initialize real NGX");
    eprintln!("NGX initialized: {:?}", session.status());
    for i in 0..3 {
        fill_scene(&device, &queue, &session);
        session
            .evaluate(Frame {
                reset: i == 0,
                ..Frame::default()
            })
            .expect("actual NGX GPU evaluation");
    }
    assert_eq!(session.status().evaluated_frames, 3);
    let bytes_per_row = 768 * 8;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("DLSS output readback"),
        size: (bytes_per_row * 512) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        session.output_texture().as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(512),
            },
        },
        wgpu::Extent3d {
            width: 768,
            height: 512,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).expect("readback receiver");
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("readback completion");
    rx.recv().expect("readback callback").expect("readback map");
    let data = buffer.slice(..).get_mapped_range().expect("mapped range");
    let center = ((256 * 768 + 384) * 8) as usize;
    let r = u16::from_le_bytes([data[center], data[center + 1]]);
    let g = u16::from_le_bytes([data[center + 2], data[center + 3]]);
    eprintln!("DLSS center FP16 channels: r={r:#06x}, g={g:#06x}");
    assert!(
        (0x3800..0x3c00).contains(&r),
        "SR must produce the scene's red channel, not empty output"
    );
    assert!(
        (0x2800..0x3400).contains(&g),
        "SR must preserve the scene's lower green channel"
    );
    drop(data);
    buffer.unmap();
    let context = session.context().clone();
    let mut replacement = Session::with_context(context, [768, 512], Quality::Balanced)
        .expect("shared context feature");
    drop(session);
    fill_scene(&device, &queue, &replacement);
    replacement
        .evaluate(Frame::default())
        .expect("context survives first feature drop");
    assert_eq!(replacement.status().evaluated_frames, 1);
    replacement.inputs()[0].texture().destroy();
    let error = replacement
        .evaluate(Frame::default())
        .expect_err("destroyed staging must fail safely");
    assert!(error.to_string().contains("staging texture was destroyed"));
    assert_eq!(replacement.status().evaluated_frames, 1);
}

fn fill_scene(device: &wgpu::Device, queue: &wgpu::Queue, session: &Session) {
    let inputs = session.inputs();
    let colors = [
        wgpu::Color {
            r: 0.75,
            g: 0.125,
            b: 0.25,
            a: 1.,
        },
        wgpu::Color {
            r: 0.5,
            g: 0.,
            b: 0.,
            a: 0.,
        },
        wgpu::Color::TRANSPARENT,
    ];
    let attachments: Vec<_> = inputs
        .into_iter()
        .zip(colors)
        .map(|(view, color)| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
                    store: wgpu::StoreOp::Store,
                },
            })
        })
        .collect();
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("DLSS uniform scene inputs"),
            color_attachments: &attachments,
            ..Default::default()
        });
    }
    queue.submit([encoder.finish()]);
}
