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
