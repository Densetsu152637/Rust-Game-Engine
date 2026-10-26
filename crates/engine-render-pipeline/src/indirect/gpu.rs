use super::{BrdfLut, DiffusePolynomial, EnvironmentData, IndirectBudget, RadianceImage};
use crate::post::lut::f32_to_f16_bits;
use anyhow::{Result, ensure};
use engine_render_wgpu::GpuContext;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING
    .union(wgpu::TextureUsages::COPY_DST)
    .union(wgpu::TextureUsages::COPY_SRC);

/// Checks the resources this module actually creates, not a future material
/// layout. Errors are explicit; callers may report a constant-ambient fallback.
/// No optional device feature is silently requested after device creation.
pub fn check_capabilities(gpu: &GpuContext) -> Result<()> {
    let features = gpu.adapter.get_texture_format_features(FORMAT);
    ensure!(
        features.allowed_usages.contains(USAGE)
            && features
                .flags
                .contains(wgpu::TextureFormatFeatureFlags::FILTERABLE),
        "indirect unavailable: adapter needs filterable Rgba16Float with sampled/copy usages"
    );
    ensure!(
        gpu.device.limits().max_texture_array_layers >= 6,
        "indirect unavailable: device needs six cube texture layers"
    );
    Ok(())
}

#[derive(Debug)]
pub struct GpuRadianceImage {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

#[derive(Debug)]
pub struct GpuEnvironment {
    /// Six D2 layers in +X,-X,+Y,-Y,+Z,-Z order, with a complete mip chain.
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub diffuse: DiffusePolynomial,
}

#[derive(Debug)]
pub struct GpuBrdfLut {
    /// R=A, G=B, B=0, A=1. Coordinates: x=NdotV, y=roughness.
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

/// One bounded immutable prepared set. Share its resources across views and
/// resize; rebuild only when source data changes. There is no implicit cache,
/// camera exposure, material binding, or temporal state here.
pub struct GpuIndirectResources {
    pub environments: Vec<GpuEnvironment>,
    pub lightmaps: Vec<GpuRadianceImage>,
    pub brdf: GpuBrdfLut,
    pub sampler: wgpu::Sampler,
    pub budget: IndirectBudget,
}

impl GpuIndirectResources {
    /// Validate the ENTIRE set before creating or uploading anything. Successful
    /// return means resources/uploads were issued, not that GPU work completed.
    /// Readback or the host's normal error handling establishes that separately.
    pub fn prepare(
        gpu: &GpuContext,
        environments: &[EnvironmentData],
        lightmaps: &[RadianceImage],
        brdf: &BrdfLut,
    ) -> Result<Self> {
        check_capabilities(gpu)?;
        let mut budget = IndirectBudget::default();
        let max = gpu.device.limits().max_texture_dimension_2d;
        for data in environments {
            budget.include_environment(data)?;
            ensure!(
                data.edge() <= max,
                "environment exceeds device texture dimension limit {max}"
            );
        }
        for data in lightmaps {
            budget.include_lightmap(data)?;
            ensure!(
                data.size().iter().all(|&n| n <= max),
                "lightmap exceeds device texture dimension limit {max}"
            );
        }
        ensure!(
            brdf.size() <= max,
            "BRDF LUT exceeds device texture dimension limit {max}"
        );
        let lut_bytes = u64::from(brdf.size()) * u64::from(brdf.size()) * 8;
        budget.add_bytes(lut_bytes, lut_bytes)?;
        let environments = environments
            .iter()
            .enumerate()
            .map(|(index, data)| {
                let texture = texture(
                    gpu,
                    &format!("indirect environment {index}"),
                    [data.edge(); 2],
                    6,
                    data.mips().len() as u32,
                );
                for (mip, faces) in data.mips().iter().enumerate() {
                    for (face, image) in faces.iter().enumerate() {
                        write(
                            gpu,
                            &texture,
                            mip as u32,
                            face as u32,
                            image.size(),
                            image.pixels().iter().map(|p| [p[0], p[1], p[2], 1.0]),
                        );
                    }
                }
                let view = texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::Cube),
                    ..Default::default()
                });
                GpuEnvironment {
                    texture,
                    view,
                    diffuse: data.diffuse().clone(),
                }
            })
            .collect();
        let lightmaps = lightmaps
            .iter()
            .enumerate()
            .map(|(index, data)| {
                let texture = texture(
                    gpu,
                    &format!("indirect lightmap {index}"),
                    data.size(),
                    1,
                    1,
                );
                write(
                    gpu,
                    &texture,
                    0,
                    0,
                    data.size(),
                    data.pixels().iter().map(|p| [p[0], p[1], p[2], 1.0]),
                );
                let view = texture.create_view(&Default::default());
                GpuRadianceImage { texture, view }
            })
            .collect();
        let texture = texture(gpu, "indirect BRDF LUT", [brdf.size(); 2], 1, 1);
        write(
            gpu,
            &texture,
            0,
            0,
            [brdf.size(); 2],
            brdf.values().iter().map(|p| [p[0], p[1], 0.0, 1.0]),
        );
        let view = texture.create_view(&Default::default());
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("indirect linear clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        gpu.check_errors()?;
        Ok(Self {
            environments,
            lightmaps,
            brdf: GpuBrdfLut { texture, view },
            sampler,
            budget,
        })
    }
}

fn texture(gpu: &GpuContext, label: &str, size: [u32; 2], layers: u32, mips: u32) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: layers,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: USAGE,
        view_formats: &[],
    })
}

fn write(
    gpu: &GpuContext,
    texture: &wgpu::Texture,
    mip: u32,
    layer: u32,
    size: [u32; 2],
    pixels: impl Iterator<Item = [f32; 4]>,
) {
    let bytes: Vec<u8> = pixels
        .flat_map(|pixel| {
            pixel
                .into_iter()
                .flat_map(|v| f32_to_f16_bits(v).to_le_bytes())
        })
        .collect();
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: mip,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: wgpu::TextureAspect::All,
        },
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size[0] * 8),
            rows_per_image: Some(size[1]),
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
}
