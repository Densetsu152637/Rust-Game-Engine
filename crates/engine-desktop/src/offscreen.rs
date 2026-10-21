use crate::{GpuContext, RenderTargetInfo};
use anyhow::{Context, Result, ensure};

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
        let row = self
            .info
            .width
            .checked_mul(4)
            .context("readback row overflow")?;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let size = u64::from(padded) * u64::from(self.info.height);
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
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.info.height),
                },
            },
            wgpu::Extent3d {
                width: self.info.width,
                height: self.info.height,
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
        let mut pixels = Vec::with_capacity(row as usize * self.info.height as usize);
        for source in mapped.chunks_exact(padded as usize) {
            pixels.extend_from_slice(&source[..row as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(pixels)
    }
}
