//! Vertex/index buffers, sampled textures, the sampler and one-shot uploads.
//!
//! Meshes live in host-visible, persistently mapped memory: updating one is a
//! memcpy into a fresh buffer (the old one is retired by the deferred queue), so
//! frequently changing sprite/text meshes never stall on a transfer. Textures
//! are device-local and uploaded through a staging buffer with explicit layout
//! transitions.

use crate::error::VkCheck;
use crate::instance::Gpu;
use crate::memory::{Buffer, Image, Location};
use crate::translate::COLOR_FORMAT;
use ash::vk;
use engine_render::{MeshResource, RenderError, Result, Texture2D, VertexLayout};

/// Runs short command sequences synchronously (texture upload, target clear,
/// readback) on its own pool and fence.
pub(crate) struct Uploader {
    pool: vk::CommandPool,
    fence: vk::Fence,
}

impl Uploader {
    pub(crate) fn empty() -> Self {
        Self {
            pool: vk::CommandPool::null(),
            fence: vk::Fence::null(),
        }
    }

    pub(crate) fn init(&mut self, device: &ash::Device, queue_family: u32) -> Result<()> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        // SAFETY: valid create-info for this device.
        self.pool =
            unsafe { device.create_command_pool(&pool_info, None) }.vk("vkCreateCommandPool")?;
        // SAFETY: valid default create-info for this device.
        self.fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }
            .vk("vkCreateFence")?;
        Ok(())
    }

    /// Record with `record`, submit, and wait for completion.
    pub(crate) fn run(
        &self,
        gpu: &Gpu,
        record: impl FnOnce(&ash::Device, vk::CommandBuffer),
    ) -> Result<()> {
        let device = &gpu.device;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the pool belongs to this device.
        let cmd =
            unsafe { device.allocate_command_buffers(&alloc) }.vk("vkAllocateCommandBuffers")?[0];
        let result = self.submit_and_wait(gpu, cmd, record);
        // SAFETY: the submission completed (or never started), so the buffer is idle.
        unsafe { device.free_command_buffers(self.pool, &[cmd]) };
        result
    }

    fn submit_and_wait(
        &self,
        gpu: &Gpu,
        cmd: vk::CommandBuffer,
        record: impl FnOnce(&ash::Device, vk::CommandBuffer),
    ) -> Result<()> {
        let device = &gpu.device;
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: `cmd` is a fresh primary buffer in the initial state.
        unsafe { device.begin_command_buffer(cmd, &begin) }.vk("vkBeginCommandBuffer")?;
        record(device, cmd);
        // SAFETY: `cmd` is in the recording state.
        unsafe { device.end_command_buffer(cmd) }.vk("vkEndCommandBuffer")?;
        let buffers = [cmd];
        let submit = vk::SubmitInfo::default().command_buffers(&buffers);
        // SAFETY: the queue, fence and command buffer belong to this device and
        // the fence is unsignaled.
        unsafe { device.queue_submit(gpu.queue, &[submit], self.fence) }.vk("vkQueueSubmit")?;
        // SAFETY: the fence was just submitted.
        let waited = unsafe { device.wait_for_fences(&[self.fence], true, u64::MAX) };
        // SAFETY: after a successful wait the fence is signaled and unused.
        let reset = unsafe { device.reset_fences(&[self.fence]) };
        waited.vk("vkWaitForFences")?;
        reset.vk("vkResetFences")
    }

    pub(crate) fn destroy(&mut self, device: &ash::Device) {
        // SAFETY: the device is idle; null handles are ignored.
        unsafe {
            device.destroy_fence(self.fence, None);
            device.destroy_command_pool(self.pool, None);
        }
        self.fence = vk::Fence::null();
        self.pool = vk::CommandPool::null();
    }
}

pub(crate) fn create_image_view(
    device: &ash::Device,
    image: vk::Image,
    format: vk::Format,
    aspect: vk::ImageAspectFlags,
) -> Result<vk::ImageView> {
    let info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });
    // SAFETY: `image` belongs to this device and supports `format`.
    unsafe { device.create_image_view(&info, None) }.vk("vkCreateImageView")
}

pub(crate) fn subresource_range(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// Record an image layout transition with explicit stages and accesses.
#[allow(clippy::too_many_arguments)]
pub(crate) fn transition(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    from: (vk::ImageLayout, vk::PipelineStageFlags, vk::AccessFlags),
    to: (vk::ImageLayout, vk::PipelineStageFlags, vk::AccessFlags),
) {
    let barrier = vk::ImageMemoryBarrier::default()
        .old_layout(from.0)
        .new_layout(to.0)
        .src_access_mask(from.2)
        .dst_access_mask(to.2)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(subresource_range(aspect));
    // SAFETY: `cmd` is recording and `image` belongs to this device.
    unsafe {
        device.cmd_pipeline_barrier(
            cmd,
            from.1,
            to.1,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
    }
}

// ---------------------------------------------------------------- textures

/// A sampled RGBA8 sRGB texture.
pub(crate) struct TextureObj {
    image: Image,
    pub view: vk::ImageView,
}

impl TextureObj {
    pub(crate) fn create(gpu: &mut Gpu, uploader: &Uploader, texture: &Texture2D) -> Result<Self> {
        texture.validate()?;
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(COLOR_FORMAT)
            .extent(vk::Extent3D {
                width: texture.width,
                height: texture.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = gpu.memory().create_image("texture", &info)?;
        let mut staging = match gpu.memory().create_buffer(
            "texture staging",
            texture.rgba8.len() as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
            Location::Upload,
        ) {
            Ok(buffer) => buffer,
            Err(error) => {
                gpu.memory().destroy_image(image);
                return Err(error);
            }
        };
        let result = staging
            .write(0, &texture.rgba8)
            .and_then(|()| upload_to_image(gpu, uploader, &staging, image.raw, texture));
        gpu.memory().destroy_buffer(staging);
        let view = match result.and_then(|()| {
            create_image_view(
                &gpu.device,
                image.raw,
                COLOR_FORMAT,
                vk::ImageAspectFlags::COLOR,
            )
        }) {
            Ok(view) => view,
            Err(error) => {
                gpu.memory().destroy_image(image);
                return Err(error);
            }
        };
        Ok(Self { image, view })
    }

    /// Destroy the texture. The GPU must no longer use it.
    pub(crate) fn destroy(self, gpu: &mut Gpu) {
        // SAFETY: the caller guarantees no pending work samples the view.
        unsafe { gpu.device.destroy_image_view(self.view, None) };
        gpu.memory().destroy_image(self.image);
    }
}

fn upload_to_image(
    gpu: &Gpu,
    uploader: &Uploader,
    staging: &Buffer,
    image: vk::Image,
    texture: &Texture2D,
) -> Result<()> {
    let region = vk::BufferImageCopy::default()
        .buffer_offset(0)
        .buffer_row_length(0)
        .buffer_image_height(0)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width: texture.width,
            height: texture.height,
            depth: 1,
        });
    let staging_raw = staging.raw;
    uploader.run(gpu, |device, cmd| {
        transition(
            device,
            cmd,
            image,
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
        // SAFETY: `cmd` is recording; the buffer holds `width * height * 4`
        // bytes and the image is in TRANSFER_DST_OPTIMAL.
        unsafe {
            device.cmd_copy_buffer_to_image(
                cmd,
                staging_raw,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
        }
        transition(
            device,
            cmd,
            image,
            vk::ImageAspectFlags::COLOR,
            (
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags::TRANSFER,
                vk::AccessFlags::TRANSFER_WRITE,
            ),
            (
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::VERTEX_SHADER,
                vk::AccessFlags::SHADER_READ,
            ),
        );
    })
}

/// Nearest-filtered, clamp-to-edge sampler (the engine's only sampler).
pub(crate) fn create_sampler(device: &ash::Device) -> Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::NEAREST)
        .min_filter(vk::Filter::NEAREST)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .max_lod(0.0)
        .border_color(vk::BorderColor::FLOAT_TRANSPARENT_BLACK);
    // SAFETY: valid create-info for this device.
    unsafe { device.create_sampler(&info, None) }.vk("vkCreateSampler")
}

// ------------------------------------------------------------------- meshes

/// GPU buffers of one mesh.
pub(crate) struct MeshObj {
    vertex: Buffer,
    index: Option<Buffer>,
    pub layout: VertexLayout,
    pub draw_count: u32,
}

impl MeshObj {
    pub(crate) fn create(gpu: &mut Gpu, mesh: &MeshResource) -> Result<Self> {
        mesh.validate()?;
        let mut vertex = gpu.memory().create_buffer(
            "vertex buffer",
            mesh.vertices.len() as u64,
            vk::BufferUsageFlags::VERTEX_BUFFER,
            Location::Upload,
        )?;
        if let Err(error) = vertex.write(0, &mesh.vertices) {
            gpu.memory().destroy_buffer(vertex);
            return Err(error);
        }
        let index = if mesh.is_indexed() {
            match create_index_buffer(gpu, &mesh.indices) {
                Ok(buffer) => Some(buffer),
                Err(error) => {
                    gpu.memory().destroy_buffer(vertex);
                    return Err(error);
                }
            }
        } else {
            None
        };
        Ok(Self {
            vertex,
            index,
            layout: mesh.layout.clone(),
            draw_count: mesh.draw_count(),
        })
    }

    pub(crate) fn vertex_buffer(&self) -> vk::Buffer {
        self.vertex.raw
    }

    pub(crate) fn index_buffer(&self) -> Option<vk::Buffer> {
        self.index.as_ref().map(|b| b.raw)
    }

    /// Destroy the buffers. The GPU must no longer use them.
    pub(crate) fn destroy(self, gpu: &mut Gpu) {
        gpu.memory().destroy_buffer(self.vertex);
        if let Some(index) = self.index {
            gpu.memory().destroy_buffer(index);
        }
    }
}

fn create_index_buffer(gpu: &mut Gpu, indices: &[u32]) -> Result<Buffer> {
    let mut buffer = gpu.memory().create_buffer(
        "index buffer",
        indices.len() as u64 * 4,
        vk::BufferUsageFlags::INDEX_BUFFER,
        Location::Upload,
    )?;
    let written = buffer.mapped().map(|mapped| {
        for (chunk, index) in mapped.as_chunks_mut::<4>().0.iter_mut().zip(indices) {
            *chunk = index.to_le_bytes();
        }
    });
    match written {
        Ok(()) => Ok(buffer),
        Err(error) => {
            gpu.memory().destroy_buffer(buffer);
            Err(error)
        }
    }
}

/// Reject sizes a device cannot hold before touching Vulkan.
pub(crate) fn check_texture_size(texture: &Texture2D, max_dimension: u32) -> Result<()> {
    if texture.width > max_dimension || texture.height > max_dimension {
        return Err(RenderError::InvalidResource(format!(
            "texture {}x{} exceeds the device limit {max_dimension}",
            texture.width, texture.height
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_textures_are_rejected_before_any_vulkan_call() {
        let texture = Texture2D::new(4, 2, vec![0; 32]).unwrap();
        assert!(check_texture_size(&texture, 4).is_ok());
        assert!(check_texture_size(&texture, 3).is_err());
    }

    #[test]
    fn subresource_range_covers_the_single_level_and_layer() {
        let range = subresource_range(vk::ImageAspectFlags::DEPTH);
        assert_eq!(range.aspect_mask, vk::ImageAspectFlags::DEPTH);
        assert_eq!((range.level_count, range.layer_count), (1, 1));
    }
}
