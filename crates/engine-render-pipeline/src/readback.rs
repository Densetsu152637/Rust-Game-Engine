//! Blocking readback of `Rgba16Float` textures for tests and tools.

use crate::pipeline::HdrImage;
use anyhow::{Context, Result, ensure};
use engine_render_wgpu::GpuContext;

/// IEEE 754 half to `f32`.
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let mantissa = f32::from(bits & 0x03ff);
    match exponent {
        0 => sign * mantissa * 2f32.powi(-24),
        31 if mantissa == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15),
    }
}

/// Wait for the GPU and read every texel of an `Rgba16Float` texture.
pub fn read_rgba16f(gpu: &GpuContext, texture: &wgpu::Texture) -> Result<HdrImage> {
    ensure!(
        texture.format() == wgpu::TextureFormat::Rgba16Float,
        "HDR readback needs an Rgba16Float texture"
    );
    let (width, height) = (texture.width(), texture.height());
    let row = width * 8;
    let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("engine HDR readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    gpu.wait_idle()?;
    receiver
        .recv()
        .context("HDR readback was not mapped")?
        .context("mapping the HDR readback")?;
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for y in 0..height as usize {
        let start = y * padded as usize;
        for texel in mapped[start..start + row as usize].chunks_exact(8) {
            let channel = |i: usize| f16_to_f32(u16::from_le_bytes([texel[i], texel[i + 1]]));
            pixels.push([channel(0), channel(2), channel(4), channel(6)]);
        }
    }
    drop(mapped);
    buffer.unmap();
    Ok(HdrImage {
        size: [width, height],
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_decode_exactly() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0x4400), 4.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert!(f16_to_f32(0x7e00).is_nan());
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
    }
}
