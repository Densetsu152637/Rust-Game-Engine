//! Buffers and images backed by `gpu-allocator`.
//!
//! [`Buffer`] and [`Image`] are plain owners without `Drop`: destruction needs
//! the device and the allocator, so it is explicit (`Memory::destroy_*`) and
//! driven by the deferred-release queue when the GPU may still be reading.

use crate::error::{AllocCheck, VkCheck};
use ash::vk;
use engine_render::{RenderError, Result};
use gpu_allocator::vulkan::{
    Allocation, AllocationCreateDesc, AllocationScheme, Allocator, AllocatorCreateDesc,
};
use gpu_allocator::{AllocationSizes, AllocatorDebugSettings, MemoryLocation};

/// Where an allocation should live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Location {
    /// Host-visible and persistently mapped; written by the CPU.
    Upload,
    /// Host-visible and persistently mapped; read by the CPU.
    Readback,
}

impl Location {
    fn to_allocator(self) -> MemoryLocation {
        match self {
            Self::Upload => MemoryLocation::CpuToGpu,
            Self::Readback => MemoryLocation::GpuToCpu,
        }
    }
}

pub(crate) struct Buffer {
    pub raw: vk::Buffer,
    pub size: u64,
    allocation: Option<Allocation>,
}

impl Buffer {
    /// Mapped bytes of a host-visible buffer.
    pub(crate) fn mapped(&mut self) -> Result<&mut [u8]> {
        self.allocation
            .as_mut()
            .and_then(Allocation::mapped_slice_mut)
            .ok_or_else(|| RenderError::Backend("buffer is not host visible".into()))
    }

    /// Mapped bytes of a host-visible buffer, read-only.
    pub(crate) fn mapped_ref(&self) -> Result<&[u8]> {
        self.allocation
            .as_ref()
            .and_then(Allocation::mapped_slice)
            .ok_or_else(|| RenderError::Backend("buffer is not host visible".into()))
    }

    /// Copy `bytes` to `offset`; the range must fit inside the buffer.
    pub(crate) fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<()> {
        let mapped = self.mapped()?;
        let end = offset
            .checked_add(bytes.len())
            .filter(|end| *end <= mapped.len())
            .ok_or_else(|| RenderError::Backend("buffer write exceeds the buffer".into()))?;
        mapped[offset..end].copy_from_slice(bytes);
        Ok(())
    }
}

pub(crate) struct Image {
    pub raw: vk::Image,
    allocation: Option<Allocation>,
}

/// The allocator plus a device handle, so creation and destruction need no
/// extra parameters.
pub(crate) struct Memory {
    allocator: Allocator,
    device: ash::Device,
}

impl Memory {
    pub(crate) fn new(
        instance: &ash::Instance,
        device: &ash::Device,
        physical: vk::PhysicalDevice,
    ) -> Result<Self> {
        const MIB: u64 = 1024 * 1024;
        let allocator = Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device: physical,
            debug_settings: AllocatorDebugSettings::default(),
            buffer_device_address: false,
            allocation_sizes: AllocationSizes::new(32 * MIB, 16 * MIB),
        })
        .alloc("gpu-allocator initialisation")?;
        Ok(Self {
            allocator,
            device: device.clone(),
        })
    }

    pub(crate) fn create_buffer(
        &mut self,
        name: &str,
        size: u64,
        usage: vk::BufferUsageFlags,
        location: Location,
    ) -> Result<Buffer> {
        let info = vk::BufferCreateInfo::default()
            .size(size.max(4))
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: `info` is a valid create-info for this device.
        let raw = unsafe { self.device.create_buffer(&info, None) }.vk("vkCreateBuffer")?;
        // SAFETY: `raw` was just created from this device.
        let requirements = unsafe { self.device.get_buffer_memory_requirements(raw) };
        let allocation = self.allocator.allocate(&AllocationCreateDesc {
            name,
            requirements,
            location: location.to_allocator(),
            linear: true,
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        });
        let allocation = match allocation {
            Ok(allocation) => allocation,
            Err(error) => {
                // SAFETY: the buffer is unused and unbound.
                unsafe { self.device.destroy_buffer(raw, None) };
                return Err(crate::error::from_alloc(name, error));
            }
        };
        // SAFETY: the allocation satisfies the buffer's requirements and the
        // memory/offset pair is exclusively ours.
        let bound = unsafe {
            self.device
                .bind_buffer_memory(raw, allocation.memory(), allocation.offset())
        };
        if let Err(result) = bound {
            let _ = self.allocator.free(allocation);
            // SAFETY: the buffer is unused.
            unsafe { self.device.destroy_buffer(raw, None) };
            return Err(crate::error::from_vk("vkBindBufferMemory", result));
        }
        Ok(Buffer {
            raw,
            size: info.size,
            allocation: Some(allocation),
        })
    }

    /// Destroy a buffer. The GPU must no longer use it.
    pub(crate) fn destroy_buffer(&mut self, mut buffer: Buffer) {
        // SAFETY: the caller guarantees no pending GPU work uses the buffer.
        unsafe { self.device.destroy_buffer(buffer.raw, None) };
        if let Some(allocation) = buffer.allocation.take() {
            let _ = self.allocator.free(allocation);
        }
    }

    pub(crate) fn create_image(
        &mut self,
        name: &str,
        info: &vk::ImageCreateInfo<'_>,
    ) -> Result<Image> {
        // SAFETY: `info` is a valid create-info for this device.
        let raw = unsafe { self.device.create_image(info, None) }.vk("vkCreateImage")?;
        // SAFETY: `raw` was just created from this device.
        let requirements = unsafe { self.device.get_image_memory_requirements(raw) };
        let allocation = self.allocator.allocate(&AllocationCreateDesc {
            name,
            requirements,
            location: MemoryLocation::GpuOnly,
            linear: false,
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        });
        let allocation = match allocation {
            Ok(allocation) => allocation,
            Err(error) => {
                // SAFETY: the image is unused and unbound.
                unsafe { self.device.destroy_image(raw, None) };
                return Err(crate::error::from_alloc(name, error));
            }
        };
        // SAFETY: the allocation satisfies the image's requirements and the
        // memory/offset pair is exclusively ours.
        let bound = unsafe {
            self.device
                .bind_image_memory(raw, allocation.memory(), allocation.offset())
        };
        if let Err(result) = bound {
            let _ = self.allocator.free(allocation);
            // SAFETY: the image is unused.
            unsafe { self.device.destroy_image(raw, None) };
            return Err(crate::error::from_vk("vkBindImageMemory", result));
        }
        Ok(Image {
            raw,
            allocation: Some(allocation),
        })
    }

    /// Destroy an image. The GPU must no longer use it.
    pub(crate) fn destroy_image(&mut self, mut image: Image) {
        // SAFETY: the caller guarantees no pending GPU work uses the image.
        unsafe { self.device.destroy_image(image.raw, None) };
        if let Some(allocation) = image.allocation.take() {
            let _ = self.allocator.free(allocation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_map_to_distinct_allocator_domains() {
        assert!(matches!(
            Location::Upload.to_allocator(),
            MemoryLocation::CpuToGpu
        ));
        assert!(matches!(
            Location::Readback.to_allocator(),
            MemoryLocation::GpuToCpu
        ));
    }

    #[test]
    fn unmapped_buffers_reject_writes_without_panicking() {
        let mut buffer = Buffer {
            raw: vk::Buffer::null(),
            size: 16,
            allocation: None,
        };
        assert!(buffer.write(0, &[1, 2, 3]).is_err());
        assert!(buffer.mapped_ref().is_err());
    }
}
