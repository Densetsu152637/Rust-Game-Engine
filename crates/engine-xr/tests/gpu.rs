//! GPU checks of the XR render helpers on an ordinary wgpu Vulkan device (no
//! headset needed). Ignored by default: they need a Vulkan GPU.
use engine_xr::{
    Eye, Fov, MirrorBlit, Pose, TestPattern, XrFrameContext, XrRenderTarget, XrView, XrViewRenderer,
};

fn gpu() -> (wgpu::Device, wgpu::Queue) {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(descriptor);
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("Vulkan adapter");
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device")
}

fn target(device: &wgpu::Device, size: [u32; 2], usage: wgpu::TextureUsages) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("xr test target"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: usage | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

/// RGBA8 pixels of `texture` (width must be a multiple of 64).
fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<[u8; 4]> {
    let (width, height) = (texture.width(), texture.height());
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(width * height * 4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("readback");
    rx.recv().unwrap().expect("map");
    let data = buffer.slice(..).get_mapped_range().expect("mapped range");
    data.as_chunks::<4>().0.to_vec()
}

fn unorm(linear: [f32; 3]) -> [u8; 4] {
    let [r, g, b] = linear.map(|c| (c * 255.0).round() as u8);
    [r, g, b, 255]
}
fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
}

#[test]
#[ignore = "requires a Vulkan GPU"]
fn test_pattern_renders_eye_tinted_checkerboard_through_the_adapter_trait() {
    let (device, queue) = gpu();
    let size = [256, 128];
    let textures = [
        target(&device, size, wgpu::TextureUsages::empty()),
        target(&device, size, wgpu::TextureUsages::empty()),
    ];
    let views: Vec<_> = textures
        .iter()
        .map(|t| t.create_view(&Default::default()))
        .collect();
    let xr_views = Eye::BOTH.map(|eye| {
        XrView::new(
            eye,
            Pose::IDENTITY,
            Fov::symmetric(1.6, 1.0),
            0.05,
            100.0,
            size,
        )
        .unwrap()
    });
    let targets: Vec<_> = (0..2)
        .map(|i| XrRenderTarget {
            view: &xr_views[i],
            texture: &textures[i],
            color: &views[i],
            format: wgpu::TextureFormat::Rgba8Unorm,
        })
        .collect();
    let mut pattern = TestPattern::new(&device);
    pattern
        .render_views(&XrFrameContext {
            device: &device,
            queue: &queue,
            predicted_display_time_ns: 0,
            targets: &targets,
        })
        .unwrap();
    assert_eq!(pattern.frames(), 1);
    let cell = TestPattern::CELL as usize;
    for (eye, texture) in textures.iter().enumerate() {
        let pixels = read(&device, &queue, texture);
        let at = |x: usize, y: usize| pixels[y * size[0] as usize + x];
        let dark = unorm(TestPattern::DARK);
        let tint = unorm(TestPattern::TINTS[eye]);
        assert!(close(at(0, 0), dark), "eye {eye} (0,0) = {:?}", at(0, 0));
        assert!(
            close(at(cell + 1, 1), tint),
            "eye {eye} = {:?}",
            at(cell + 1, 1)
        );
        assert!(
            close(at(1, cell + 1), tint),
            "eye {eye} = {:?}",
            at(1, cell + 1)
        );
        assert!(
            close(at(cell + 1, cell + 1), dark),
            "eye {eye} = {:?}",
            at(cell + 1, cell + 1)
        );
    }
}

#[test]
#[ignore = "requires a Vulkan GPU"]
fn mirror_blit_scales_an_eye_into_a_desktop_texture() {
    let (device, queue) = gpu();
    // Source: left half red, right half blue, 128x64.
    let source = target(
        &device,
        [128, 64],
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let mut texels = Vec::with_capacity(128 * 64 * 4);
    for _y in 0..64 {
        for x in 0..128 {
            texels.extend_from_slice(if x < 64 {
                &[255, 0, 0, 255]
            } else {
                &[0, 0, 255, 255]
            });
        }
    }
    queue.write_texture(
        source.as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(128 * 4),
            rows_per_image: Some(64),
        },
        source.size(),
    );
    let destination = target(&device, [64, 32], wgpu::TextureUsages::empty());
    let mut blit = MirrorBlit::new(&device);
    let mut encoder = device.create_command_encoder(&Default::default());
    blit.encode(
        &device,
        &mut encoder,
        &source.create_view(&Default::default()),
        &destination.create_view(&Default::default()),
        wgpu::TextureFormat::Rgba8Unorm,
    );
    queue.submit([encoder.finish()]);
    let pixels = read(&device, &queue, &destination);
    let at = |x: usize, y: usize| pixels[y * 64 + x];
    assert!(close(at(4, 16), [255, 0, 0, 255]), "{:?}", at(4, 16));
    assert!(close(at(60, 16), [0, 0, 255, 255]), "{:?}", at(60, 16));
    assert!(close(at(16, 0), [255, 0, 0, 255]) && close(at(48, 31), [0, 0, 255, 255]));
}
