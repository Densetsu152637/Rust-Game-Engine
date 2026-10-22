//! Render target description, offscreen targets and texture readback.

use crate::GpuContext;
use anyhow::{Context, Result, ensure};

/// Size and color format of a render target.
#[derive(Clone, Copy, Debug)]
pub struct RenderTargetInfo {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}

/// Real renderable RGBA target, with padded GPU readback stripped to tightly
/// packed top-left rows. This uses the same render path as a native surface.
pub struct OffscreenTarget {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub info: RenderTargetInfo,
}

impl OffscreenTarget {
    pub fn new(gpu: &GpuContext, width: u32, height: u32) -> Result<Self> {
        ensure!(width > 0 && height > 0, "offscreen size must be positive");
        ensure!(
            width <= gpu.device.limits().max_texture_dimension_2d
                && height <= gpu.device.limits().max_texture_dimension_2d,
            "offscreen size exceeds device limit"
        );
        let info = RenderTargetInfo {
            width,
            height,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
        };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen RGBA target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: info.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        gpu.check_errors()?;
        Ok(Self {
            texture,
            view,
            info,
        })
    }

    pub fn read_rgba(&self, gpu: &GpuContext) -> Result<Vec<u8>> {
        read_texture_rgba(gpu, &self.texture, self.info.width, self.info.height)
    }
}

/// Bytes per row of a tightly packed RGBA8 image row, or `None` on overflow.
pub fn rgba_row_bytes(width: u32) -> Option<u32> {
    width.checked_mul(4)
}

/// `row_bytes` rounded up to wgpu's copy row alignment.
pub fn padded_row_bytes(row_bytes: u32) -> u32 {
    row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

/// Strip per-row padding from a mapped readback buffer.
pub fn unpad_rows(mapped: &[u8], padded_row: usize, row: usize) -> Vec<u8> {
    let rows = mapped.len() / padded_row.max(1);
    let mut pixels = Vec::with_capacity(row * rows);
    for source in mapped.chunks_exact(padded_row) {
        pixels.extend_from_slice(&source[..row]);
    }
    pixels
}

/// Copy a `COPY_SRC` RGBA8 texture to tightly packed rows (top row first). Waits
/// for the GPU to finish all submitted work first.
pub fn read_texture_rgba(
    gpu: &GpuContext,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let row = rgba_row_bytes(width).context("readback row overflow")?;
    let padded = padded_row_bytes(row);
    let size = u64::from(padded) * u64::from(height);
    ensure!(
        size <= gpu.device.limits().max_buffer_size,
        "readback exceeds buffer limit"
    );
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("offscreen readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
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
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    gpu.wait_idle()?;
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .context("readback callback timed out")?
        .context("mapping readback buffer")?;
    let mapped = buffer
        .slice(..)
        .get_mapped_range()
        .context("accessing mapped readback")?;
    let pixels = unpad_rows(&mapped, padded as usize, row as usize);
    drop(mapped);
    buffer.unmap();
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_pad_to_the_copy_alignment() {
        assert_eq!(rgba_row_bytes(64), Some(256));
        assert_eq!(padded_row_bytes(256), 256);
        assert_eq!(padded_row_bytes(4), 256);
        assert_eq!(padded_row_bytes(260), 512);
        assert_eq!(padded_row_bytes(0), 0);
        assert_eq!(rgba_row_bytes(u32::MAX), None);
    }

    #[test]
    fn unpadding_keeps_only_the_leading_row_bytes() {
        // Two rows of 3 pixels (12 bytes) padded to 16.
        let mut mapped = Vec::new();
        for row in 0u8..2 {
            mapped.extend((0..12).map(|i| row * 100 + i));
            mapped.extend([255; 4]);
        }
        let pixels = unpad_rows(&mapped, 16, 12);
        assert_eq!(pixels.len(), 24);
        assert_eq!(&pixels[..12], &(0..12).collect::<Vec<u8>>()[..]);
        assert_eq!(pixels[12], 100);
        assert!(!pixels.contains(&255));
    }

    #[test]
    fn render_target_info_is_copy_and_keeps_its_fields() {
        let info = RenderTargetInfo {
            width: 64,
            height: 32,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
        };
        let copy = info;
        assert_eq!(
            (copy.width, copy.height, copy.format),
            (64, 32, info.format)
        );
    }
}
