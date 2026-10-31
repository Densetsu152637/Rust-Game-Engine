//! Blocking GPU diagnostics of a view's compacted camera draws, for tests and
//! tooling. Nothing here runs in normal rendering.
use super::IndirectOutput;
use anyhow::{Context, Result, ensure};
use engine_render_wgpu::GpuContext;
use std::ops::Range;

/// GPU-written survivor count of one indirect batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchSurvivors {
    /// The candidate range the batch compacts.
    pub source: Range<u32>,
    /// The `instance_count` the compaction pass wrote into the indirect command.
    pub survivors: u32,
}

/// Readback of one view's last compacted opaque camera draws.
#[derive(Clone, Debug)]
pub struct VisibilityDiagnostic {
    pub size: [u32; 2],
    pub batches: Vec<BatchSurvivors>,
    /// `Rgba8Unorm` pixels, row-major without padding. Each surviving
    /// instance covers its screen footprint: `rgb` holds the draw-id hash of
    /// the last instance drawn there (low 24 bits as `r | g << 8 | b << 16`)
    /// and `a` counts the surviving instances covering the pixel. Depth
    /// testing is disabled, so a culled instance contributes nothing and
    /// overlap is visible whatever the draw order.
    pub rgba: Vec<u8>,
}

impl VisibilityDiagnostic {
    /// Total instances the GPU kept.
    pub fn survivors(&self) -> u32 {
        self.batches.iter().map(|batch| batch.survivors).sum()
    }

    /// Pixels whose last-drawn surviving instance has the given draw-id
    /// hash. Overlapped instances may be hidden here; see [`Self::coverage`].
    pub fn pixels_with_tag(&self, id_hash: u32) -> usize {
        let tag = id_hash & 0x00ff_ffff;
        self.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| {
                p[3] != 0 && u32::from(p[0]) | u32::from(p[1]) << 8 | u32::from(p[2]) << 16 == tag
            })
            .count()
    }

    /// How many surviving instances cover pixel (`x`, `y`).
    pub fn coverage(&self, x: u32, y: u32) -> u8 {
        self.rgba[((y * self.size[0] + x) * 4 + 3) as usize]
    }
}

pub(crate) fn read_buffer(gpu: &GpuContext, buffer: &wgpu::Buffer) -> Result<Vec<u8>> {
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("engine diagnostic readback"),
        size: buffer.size(),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, buffer.size());
    gpu.queue.submit([encoder.finish()]);
    map(gpu, &staging)
}

fn map(gpu: &GpuContext, staging: &wgpu::Buffer) -> Result<Vec<u8>> {
    let slice = staging.slice(..);
    let (send, receive) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = send.send(result);
    });
    gpu.wait_idle()?;
    receive
        .recv()
        .context("diagnostic readback callback absent")?
        .context("mapping the diagnostic readback")?;
    let mapped = slice
        .get_mapped_range()
        .map_err(|error| anyhow::anyhow!("reading the diagnostic: {error:?}"))?;
    let bytes = mapped.to_vec();
    drop(mapped);
    staging.unmap();
    Ok(bytes)
}

/// Survivor counts the compaction pass wrote into the indirect commands.
pub(crate) fn read_survivors(
    gpu: &GpuContext,
    output: &IndirectOutput,
) -> Result<Vec<BatchSurvivors>> {
    let draws = read_buffer(gpu, &output.draw_args)?;
    let indexed = read_buffer(gpu, &output.indexed_args)?;
    let word = |bytes: &[u8], offset: u64| -> Result<u32> {
        let at = usize::try_from(offset)?;
        let slice = bytes
            .get(at..at + 4)
            .context("indirect argument out of range")?;
        Ok(u32::from_le_bytes(slice.try_into().expect("four bytes")))
    };
    output
        .batches
        .iter()
        .map(|batch| {
            // instance_count follows the vertex/index count in both layouts.
            let survivors = if batch.mesh.index.is_some() {
                word(&indexed, batch.indexed_offset + 4)?
            } else {
                word(&draws, batch.draw_offset + 4)?
            };
            Ok(BatchSurvivors {
                source: batch.source.clone(),
                survivors,
            })
        })
        .collect()
}

/// Copy a `Rgba8Unorm` texture back without row padding.
pub(crate) fn read_rgba8(gpu: &GpuContext, texture: &wgpu::Texture) -> Result<Vec<u8>> {
    ensure!(
        texture.format() == wgpu::TextureFormat::Rgba8Unorm,
        "diagnostic readback needs Rgba8Unorm"
    );
    let (width, height) = (texture.width(), texture.height());
    let row = width * 4;
    let padded =
        row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("engine diagnostic image readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let bytes = map(gpu, &staging)?;
    let mut pixels = Vec::with_capacity((row * height) as usize);
    for y in 0..height as usize {
        let start = y * padded as usize;
        pixels.extend_from_slice(&bytes[start..start + row as usize]);
    }
    Ok(pixels)
}
