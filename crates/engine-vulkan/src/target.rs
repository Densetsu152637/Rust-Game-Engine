//! Offscreen render targets (RGBA8 sRGB color + D32 depth) and readback.
//!
//! A target's color image is left in `TRANSFER_SRC_OPTIMAL` by the render pass,
//! so reading it back only needs a buffer copy.

use crate::error::VkCheck;
use crate::instance::Gpu;
use crate::memory::{Image, Location};
use crate::resources::{Uploader, create_image_view, transition};
use crate::translate::{self, COLOR_FORMAT, DEPTH_FORMAT, ReadbackOrder};
use ash::vk;
use engine_render::{ReadbackImage, RenderError, Result};

/// A D32 depth image with its view.
pub(crate) struct DepthBuffer {
    image: Image,
    pub view: vk::ImageView,
}

impl DepthBuffer {
    pub(crate) fn create(gpu: &mut Gpu, width: u32, height: u32) -> Result<Self> {
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(DEPTH_FORMAT)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = gpu.memory().create_image("depth", &info)?;
        match create_image_view(
            &gpu.device,
            image.raw,
            DEPTH_FORMAT,
            vk::ImageAspectFlags::DEPTH,
        ) {
            Ok(view) => Ok(Self { image, view }),
            Err(error) => {
                gpu.memory().destroy_image(image);
                Err(error)
            }
        }
    }

    /// Destroy the depth buffer. The GPU must no longer use it.
    pub(crate) fn destroy(self, gpu: &mut Gpu) {
        // SAFETY: the caller guarantees no pending work uses the view.
        unsafe { gpu.device.destroy_image_view(self.view, None) };
        gpu.memory().destroy_image(self.image);
    }
}

/// Create a framebuffer over `[color, depth]` for `pass`.
pub(crate) fn create_framebuffer(
    device: &ash::Device,
    pass: vk::RenderPass,
    color: vk::ImageView,
    depth: vk::ImageView,
    width: u32,
    height: u32,
) -> Result<vk::Framebuffer> {
    let attachments = [color, depth];
    let info = vk::FramebufferCreateInfo::default()
        .render_pass(pass)
        .attachments(&attachments)
        .width(width)
        .height(height)
        .layers(1);
    // SAFETY: the views and pass belong to this device and match in format.
    unsafe { device.create_framebuffer(&info, None) }.vk("vkCreateFramebuffer")
}

/// An offscreen color+depth target.
pub(crate) struct TargetObj {
    color: Image,
    color_view: vk::ImageView,
    depth: DepthBuffer,
    pub framebuffer: vk::Framebuffer,
    pub width: u32,
    pub height: u32,
}

impl TargetObj {
    pub(crate) fn create(
        gpu: &mut Gpu,
        uploader: &Uploader,
        pass: vk::RenderPass,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(COLOR_FORMAT)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let color = gpu.memory().create_image("offscreen color", &info)?;
        let mut parts = PartialTarget {
            color: Some(color),
            ..Default::default()
        };
        match parts.finish(gpu, uploader, pass, width, height) {
            Ok(target) => Ok(target),
            Err(error) => {
                parts.destroy(gpu);
                Err(error)
            }
        }
    }

    /// Destroy the target. The GPU must no longer use it.
    pub(crate) fn destroy(self, gpu: &mut Gpu) {
        // SAFETY: the caller guarantees no pending work uses these objects.
        unsafe {
            gpu.device.destroy_framebuffer(self.framebuffer, None);
            gpu.device.destroy_image_view(self.color_view, None);
        }
        self.depth.destroy(gpu);
        gpu.memory().destroy_image(self.color);
    }

    /// Wait for the GPU and copy the color image out as tightly packed RGBA8.
    /// The caller has already waited for rendering to finish.
    pub(crate) fn read(&self, gpu: &mut Gpu, uploader: &Uploader) -> Result<ReadbackImage> {
        let bytes = u64::from(self.width) * u64::from(self.height) * 4;
        let buffer = gpu.memory().create_buffer(
            "readback",
            bytes,
            vk::BufferUsageFlags::TRANSFER_DST,
            Location::Readback,
        )?;
        let region = vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: self.width,
                height: self.height,
                depth: 1,
            });
        let (image, raw_buffer) = (self.color.raw, buffer.raw);
        let copied = uploader.run(gpu, |device, cmd| {
            // SAFETY: `cmd` is recording; the image is in TRANSFER_SRC_OPTIMAL
            // (left there by the render pass or the initial clear) and the buffer
            // holds `width * height * 4` bytes.
            unsafe {
                device.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    raw_buffer,
                    &[region],
                );
            }
            let barrier = vk::BufferMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::HOST_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(raw_buffer)
                .offset(0)
                .size(vk::WHOLE_SIZE);
            // SAFETY: `cmd` is recording and the buffer belongs to this device.
            unsafe {
                device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[barrier],
                    &[],
                );
            }
        });
        let image_result = copied.and_then(|()| {
            let mapped = buffer.mapped_ref()?;
            translate::repack_readback(
                mapped,
                self.width,
                self.height,
                self.width as usize * 4,
                ReadbackOrder::for_format(COLOR_FORMAT),
            )
            .map(|rgba8| ReadbackImage {
                width: self.width,
                height: self.height,
                rgba8,
            })
            .ok_or_else(|| RenderError::Backend("readback buffer is smaller than the image".into()))
        });
        gpu.memory().destroy_buffer(buffer);
        image_result
    }
}

/// Target under construction, so a failure part-way frees what exists.
#[derive(Default)]
struct PartialTarget {
    color: Option<Image>,
    color_view: Option<vk::ImageView>,
    depth: Option<DepthBuffer>,
    framebuffer: Option<vk::Framebuffer>,
}

impl PartialTarget {
    fn finish(
        &mut self,
        gpu: &mut Gpu,
        uploader: &Uploader,
        pass: vk::RenderPass,
        width: u32,
        height: u32,
    ) -> Result<TargetObj> {
        let color_raw = self
            .color
            .as_ref()
            .map(|c| c.raw)
            .ok_or_else(|| RenderError::Backend("target color image missing".into()))?;
        self.color_view = Some(create_image_view(
            &gpu.device,
            color_raw,
            COLOR_FORMAT,
            vk::ImageAspectFlags::COLOR,
        )?);
        self.depth = Some(DepthBuffer::create(gpu, width, height)?);
        let (color_view, depth_view) = match (&self.color_view, &self.depth) {
            (Some(color), Some(depth)) => (*color, depth.view),
            _ => return Err(RenderError::Backend("target is incomplete".into())),
        };
        self.framebuffer = Some(create_framebuffer(
            &gpu.device,
            pass,
            color_view,
            depth_view,
            width,
            height,
        )?);
        // Give the image defined contents (transparent black) and leave it in the
        // layout the render pass also leaves it in.
        uploader.run(gpu, |device, cmd| {
            transition(
                device,
                cmd,
                color_raw,
                vk::ImageAspectFlags::COLOR,
                (
                    vk::ImageLayout::UNDEFINED,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::AccessFlags::empty(),
                ),
                (
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::AccessFlags::TRANSFER_WRITE,
                ),
            );
            let clear = vk::ClearColorValue { float32: [0.0; 4] };
            // SAFETY: `cmd` is recording and the image is in TRANSFER_DST_OPTIMAL.
            unsafe {
                device.cmd_clear_color_image(
                    cmd,
                    color_raw,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &clear,
                    &[crate::resources::subresource_range(
                        vk::ImageAspectFlags::COLOR,
                    )],
                );
            }
            transition(
                device,
                cmd,
                color_raw,
                vk::ImageAspectFlags::COLOR,
                (
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::AccessFlags::TRANSFER_WRITE,
                ),
                (
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::AccessFlags::TRANSFER_READ,
                ),
            );
        })?;
        match (
            self.color.take(),
            self.color_view.take(),
            self.depth.take(),
            self.framebuffer.take(),
        ) {
            (Some(color), Some(color_view), Some(depth), Some(framebuffer)) => Ok(TargetObj {
                color,
                color_view,
                depth,
                framebuffer,
                width,
                height,
            }),
            _ => Err(RenderError::Backend("target is incomplete".into())),
        }
    }

    fn destroy(&mut self, gpu: &mut Gpu) {
        // SAFETY: nothing was submitted with these objects (or the submission
        // completed), so they are unused.
        unsafe {
            if let Some(framebuffer) = self.framebuffer.take() {
                gpu.device.destroy_framebuffer(framebuffer, None);
            }
            if let Some(view) = self.color_view.take() {
                gpu.device.destroy_image_view(view, None);
            }
        }
        if let Some(depth) = self.depth.take() {
            depth.destroy(gpu);
        }
        if let Some(color) = self.color.take() {
            gpu.memory().destroy_image(color);
        }
    }
}
