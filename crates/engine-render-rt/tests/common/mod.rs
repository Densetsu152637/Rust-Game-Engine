//! Shared helpers of the GPU tests (all `#[ignore]`d; run on a Vulkan adapter
//! with ray query, verified on an RTX 5080, with `--test-threads=1`).
#![allow(dead_code)]

use engine_render_rt::{
    IDENTITY_TRANSFORM, RtDevice, RtIndices, RtInstanceDesc, RtMeshData, RtScene, RtSceneLimits,
    StandaloneOptions,
};
use std::time::Duration;

pub fn open() -> RtDevice {
    match RtDevice::open_standalone(&StandaloneOptions::default()) {
        Ok(rt) => rt,
        Err(status) => panic!("hardware tier unavailable: {status}"),
    }
}

pub fn scene(rt: &RtDevice) -> RtScene {
    RtScene::new(rt, RtSceneLimits::for_device(rt.limits())).expect("scene")
}

pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn data(&self) -> RtMeshData<'_> {
        RtMeshData {
            positions: &self.positions,
            indices: RtIndices::U32(&self.indices),
            normals: None,
        }
    }
}

/// Axis-aligned box, 12 triangles.
pub fn cuboid(min: [f32; 3], max: [f32; 3]) -> Mesh {
    let p = |i: u32| {
        [
            if i & 1 != 0 { max[0] } else { min[0] },
            if i & 2 != 0 { max[1] } else { min[1] },
            if i & 4 != 0 { max[2] } else { min[2] },
        ]
    };
    let faces = [
        [0, 4, 6, 2],
        [1, 3, 7, 5],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 2, 3, 1],
        [4, 5, 7, 6],
    ];
    let indices = faces
        .iter()
        .flat_map(|f| [f[0], f[1], f[2], f[0], f[2], f[3]])
        .collect();
    Mesh {
        positions: (0..8).map(p).collect(),
        indices,
    }
}

/// Square in the y = `y` plane, x and z in [-half, half].
pub fn ground(half: f32, y: f32) -> Mesh {
    Mesh {
        positions: vec![
            [-half, y, -half],
            [half, y, -half],
            [half, y, half],
            [-half, y, half],
        ],
        indices: vec![0, 2, 1, 0, 3, 2],
    }
}

/// Unit icosphere with `subdivisions` midpoint subdivisions (20 x 4^n triangles).
pub fn icosphere(subdivisions: u32) -> Mesh {
    let t = (1.0 + 5f32.sqrt()) / 2.0;
    let mut positions: Vec<[f32; 3]> = [
        [-1.0, t, 0.0],
        [1.0, t, 0.0],
        [-1.0, -t, 0.0],
        [1.0, -t, 0.0],
        [0.0, -1.0, t],
        [0.0, 1.0, t],
        [0.0, -1.0, -t],
        [0.0, 1.0, -t],
        [t, 0.0, -1.0],
        [t, 0.0, 1.0],
        [-t, 0.0, -1.0],
        [-t, 0.0, 1.0],
    ]
    .iter()
    .map(|p| normalize(*p))
    .collect();
    let mut faces: Vec<[u32; 3]> = vec![
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    for _ in 0..subdivisions {
        let mut cache = std::collections::HashMap::new();
        let mut midpoint = |a: u32, b: u32, positions: &mut Vec<[f32; 3]>| {
            *cache.entry((a.min(b), a.max(b))).or_insert_with(|| {
                let (pa, pb) = (positions[a as usize], positions[b as usize]);
                positions.push(normalize([pa[0] + pb[0], pa[1] + pb[1], pa[2] + pb[2]]));
                (positions.len() - 1) as u32
            })
        };
        faces = faces
            .iter()
            .flat_map(|&[a, b, c]| {
                let ab = midpoint(a, b, &mut positions);
                let bc = midpoint(b, c, &mut positions);
                let ca = midpoint(c, a, &mut positions);
                [[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]
            })
            .collect();
    }
    Mesh {
        positions,
        indices: faces.into_iter().flatten().collect(),
    }
}

pub fn normalize(p: [f32; 3]) -> [f32; 3] {
    let l = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
    [p[0] / l, p[1] / l, p[2] / l]
}

pub fn translation(x: f32, y: f32, z: f32) -> [f32; 12] {
    let mut t = IDENTITY_TRANSFORM;
    t[3] = x;
    t[7] = y;
    t[11] = z;
    t
}

pub fn instance(mesh: engine_render_rt::MeshId, transform: [f32; 12]) -> RtInstanceDesc {
    RtInstanceDesc {
        mesh,
        transform,
        mask: 0xff,
        custom_index: 0,
        material_index: 0,
    }
}

pub fn texture(
    rt: &RtDevice,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    rt.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("test texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

/// An `Rgba32Float` texture holding `texels` (row-major).
pub fn rgba32f_texture(
    rt: &RtDevice,
    width: u32,
    height: u32,
    texels: &[[f32; 4]],
) -> wgpu::Texture {
    let texture = texture(
        rt,
        wgpu::TextureFormat::Rgba32Float,
        width,
        height,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    rt.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(texels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 16),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

pub fn submit(rt: &RtDevice, encoder: wgpu::CommandEncoder) {
    let index = rt.queue().submit(Some(encoder.finish()));
    rt.device()
        .poll(wgpu::PollType::Wait {
            submission_index: Some(index),
            timeout: Some(Duration::from_secs(10)),
        })
        .expect("GPU work");
}

/// Reads a whole texture; `bytes_per_texel x width` must be a multiple of 256.
pub fn read_texture(rt: &RtDevice, texture: &wgpu::Texture, bytes_per_texel: u32) -> Vec<u8> {
    let (width, height) = (texture.width(), texture.height());
    let row = width * bytes_per_texel;
    assert_eq!(row % 256, 0, "test textures use 256-byte aligned rows");
    let size = u64::from(row * height);
    let buffer = rt.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("test readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = rt.device().create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    submit(rt, encoder);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    rt.device()
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .expect("map");
    let bytes = slice.get_mapped_range().expect("mapped").to_vec();
    buffer.unmap();
    bytes
}

/// IEEE half to f32.
pub fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = (h >> 10) & 0x1f;
    let mantissa = f32::from(h & 0x3ff);
    sign * match exponent {
        0 => mantissa / 1024.0 * 2f32.powi(-14),
        31 => {
            if mantissa == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        e => (1.0 + mantissa / 1024.0) * 2f32.powi(i32::from(e) - 15),
    }
}

/// Quad `p0 p1 p2 p3` (two triangles, two-sided for rays).
pub fn quad(p: [[f32; 3]; 4]) -> Mesh {
    Mesh {
        positions: p.to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

/// Uploads `bytes` (row-major, `bytes_per_texel` each) into the whole of `texture`.
pub fn upload(rt: &RtDevice, texture: &wgpu::Texture, bytes: &[u8], bytes_per_texel: u32) {
    let (width, height) = (texture.width(), texture.height());
    assert_eq!(bytes.len() as u32, width * height * bytes_per_texel);
    rt.queue().write_texture(
        texture.as_image_copy(),
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * bytes_per_texel),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// A sampled float texture of `format` holding `bytes`.
pub fn input_texture(
    rt: &RtDevice,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    bytes: &[u8],
) -> wgpu::Texture {
    let t = texture(
        rt,
        format,
        width,
        height,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let bytes_per_texel = format.block_copy_size(None).expect("plain format");
    upload(rt, &t, bytes, bytes_per_texel);
    t
}

/// A kernel output: storage, sampled and readable.
pub fn output_texture(
    rt: &RtDevice,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    texture(
        rt,
        format,
        width,
        height,
        wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
    )
}

pub fn view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&Default::default())
}

/// One G-buffer texel: world position, world normal, geometry present.
#[derive(Clone, Copy, Debug)]
pub struct Texel {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub geometry: bool,
}

impl Texel {
    pub fn surface(position: [f32; 3], normal: [f32; 3]) -> Self {
        Self {
            position,
            normal,
            geometry: true,
        }
    }

    pub const BACKGROUND: Self = Self {
        position: [0.0; 3],
        normal: [0.0; 3],
        geometry: false,
    };
}

/// `Rgba32Float` world positions and normals.
pub struct GBuffer {
    pub width: u32,
    pub height: u32,
    pub position: wgpu::Texture,
    pub normal: wgpu::Texture,
    pub position_view: wgpu::TextureView,
    pub normal_view: wgpu::TextureView,
}

impl GBuffer {
    pub fn new(rt: &RtDevice, width: u32, height: u32, texels: &[Texel]) -> Self {
        assert_eq!(texels.len() as u32, width * height);
        let positions: Vec<[f32; 4]> = texels
            .iter()
            .map(|t| {
                let p = t.position;
                [p[0], p[1], p[2], if t.geometry { 1.0 } else { 0.0 }]
            })
            .collect();
        let normals: Vec<[f32; 4]> = texels
            .iter()
            .map(|t| [t.normal[0], t.normal[1], t.normal[2], 0.0])
            .collect();
        let position = rgba32f_texture(rt, width, height, &positions);
        let normal = rgba32f_texture(rt, width, height, &normals);
        Self {
            width,
            height,
            position_view: view(&position),
            normal_view: view(&normal),
            position,
            normal,
        }
    }

    /// Rewrites every texel (same size).
    pub fn update(&self, rt: &RtDevice, texels: &[Texel]) {
        let positions: Vec<[f32; 4]> = texels
            .iter()
            .map(|t| {
                let p = t.position;
                [p[0], p[1], p[2], if t.geometry { 1.0 } else { 0.0 }]
            })
            .collect();
        let normals: Vec<[f32; 4]> = texels
            .iter()
            .map(|t| [t.normal[0], t.normal[1], t.normal[2], 0.0])
            .collect();
        upload(rt, &self.position, bytemuck::cast_slice(&positions), 16);
        upload(rt, &self.normal, bytemuck::cast_slice(&normals), 16);
    }

    pub fn views(&self) -> engine_render_rt::RtGBuffer<'_> {
        engine_render_rt::RtGBuffer {
            world_position: &self.position_view,
            world_normal: &self.normal_view,
            width: self.width,
            height: self.height,
        }
    }
}

/// Reads an `Rgba16Float` texture (width a multiple of 32).
pub fn read_rgba16f(rt: &RtDevice, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    bytemuck::cast_slice::<u8, [u16; 4]>(&read_texture(rt, texture, 8))
        .iter()
        .map(|c| c.map(half_to_f32))
        .collect()
}

/// Reads a single-channel texture of `format` (`R8Unorm`, `R16Float` or `R32Float`).
pub fn read_scalar(rt: &RtDevice, texture: &wgpu::Texture) -> Vec<f32> {
    match texture.format() {
        wgpu::TextureFormat::R8Unorm => read_texture(rt, texture, 1)
            .iter()
            .map(|&v| f32::from(v) / 255.0)
            .collect(),
        wgpu::TextureFormat::R16Float => {
            bytemuck::cast_slice::<u8, u16>(&read_texture(rt, texture, 2))
                .iter()
                .map(|&h| half_to_f32(h))
                .collect()
        }
        wgpu::TextureFormat::R32Float => {
            bytemuck::cast_slice(&read_texture(rt, texture, 4)).to_vec()
        }
        other => panic!("not a scalar format: {other:?}"),
    }
}

/// The best scalar storage format of the device: `R16Float` when supported.
pub fn scalar_format(rt: &RtDevice) -> engine_render_rt::RtScalarFormat {
    if rt.supports_storage_format(wgpu::TextureFormat::R16Float) {
        engine_render_rt::RtScalarFormat::R16Float
    } else {
        engine_render_rt::RtScalarFormat::R32Float
    }
}

pub fn rmse(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len());
    let sum: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
        .sum();
    (sum / a.len() as f64).sqrt()
}

pub fn mean(values: &[f32]) -> f64 {
    values.iter().map(|&v| f64::from(v)).sum::<f64>() / values.len() as f64
}

/// Deterministic xorshift generator for CPU-side noise.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Ray / axis-aligned box intersection (slab test): the entry distance in
/// `[t_min, t_max]`, or `None`.
pub fn ray_box(
    origin: [f32; 3],
    dir: [f32; 3],
    min: [f32; 3],
    max: [f32; 3],
    t_max: f32,
) -> Option<f32> {
    let (mut near, mut far) = (0.0f32, t_max);
    for a in 0..3 {
        if dir[a].abs() < 1e-12 {
            if origin[a] < min[a] || origin[a] > max[a] {
                return None;
            }
            continue;
        }
        let t0 = (min[a] - origin[a]) / dir[a];
        let t1 = (max[a] - origin[a]) / dir[a];
        near = near.max(t0.min(t1));
        far = far.min(t0.max(t1));
        if near > far {
            return None;
        }
    }
    Some(near)
}
