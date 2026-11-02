//! HDR display output of [`super::GpuRenderer`]: while enabled, frames render
//! into an engine-owned `Rgba16Float` display-linear image (`1.0` = paper
//! white, UI included, unclamped) and a final pass encodes it into the HDR
//! surface. See docs/rendering-hdr-output.md.

use crate::hdr::{HdrStatus, PQ_CHOICE, SCRGB_CHOICE};
use anyhow::{Result, ensure};
use engine_render_pipeline::{HDR_FORMAT, HdrEncoding, HdrOutputPass, HdrOutputSettings};

/// The format HDR frames render in before the output transform.
pub(super) const LINEAR_FORMAT: wgpu::TextureFormat = HDR_FORMAT;

/// The surface format an encoding requires.
pub(super) fn surface_format(encoding: HdrEncoding) -> wgpu::TextureFormat {
    match encoding {
        HdrEncoding::ScRgb => SCRGB_CHOICE.format,
        HdrEncoding::Pq => PQ_CHOICE.format,
    }
}

/// Enabled HDR output state.
pub(super) struct HdrOutput {
    pub(super) settings: HdrOutputSettings,
    /// The surface (presented) format the final pass writes.
    pub(super) present_format: wgpu::TextureFormat,
    pub(super) status: HdrStatus,
    pass: HdrOutputPass,
    image: Option<LinearImage>,
}

struct LinearImage {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl HdrOutput {
    pub(super) fn new(
        device: &wgpu::Device,
        settings: HdrOutputSettings,
        present_format: wgpu::TextureFormat,
        status: HdrStatus,
    ) -> Result<Self> {
        settings.validate()?;
        ensure!(
            present_format == surface_format(settings.encoding),
            "{} output needs a {:?} surface, not {present_format:?}",
            settings.encoding.label(),
            surface_format(settings.encoding)
        );
        Ok(Self {
            settings,
            present_format,
            status,
            pass: HdrOutputPass::new(device)?,
            image: None,
        })
    }

    /// The display-linear image for a `width` x `height` frame, recreated when
    /// the size changed.
    pub(super) fn linear_view(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> wgpu::TextureView {
        let stale = self
            .image
            .as_ref()
            .is_none_or(|image| image.texture.width() != width || image.texture.height() != height);
        if stale {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("engine hdr display-linear frame"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: LINEAR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            self.image = Some(LinearImage { texture, view });
        }
        self.image.as_ref().expect("created above").view.clone()
    }

    /// The last display-linear image (tests and tools).
    pub(super) fn linear_texture(&self) -> Option<&wgpu::Texture> {
        self.image.as_ref().map(|image| &image.texture)
    }

    /// Encode the display-linear image into `target`.
    pub(super) fn present(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
    ) -> Result<()> {
        let image = self
            .image
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no HDR frame was rendered"))?;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("engine hdr output"),
        });
        self.pass.encode(
            device,
            queue,
            &mut encoder,
            &image.view,
            target,
            self.present_format,
            &self.settings,
        )?;
        queue.submit([encoder.finish()]);
        Ok(())
    }
}
