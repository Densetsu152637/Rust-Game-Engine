//! Frames in flight: per-slot command pool, fence, semaphore, uniform arena and
//! descriptor pools, plus the pure bookkeeping around them (uniform placement,
//! deferred release, redundant-bind tracking).
//!
//! ## Synchronization design
//!
//! Frame `n` uses slot `n % frames_in_flight`. Before a slot is reused its fence
//! is waited, which proves the GPU finished the frame that last used it; only
//! then are its command pool, descriptor pools and uniform arena touched again.
//! Resources that outlive a frame (meshes, textures, programs, pipelines) are
//! retired through a [`DeferredQueue`]: an object retired when `n` frames had
//! been submitted may be destroyed once frames `0..n` are known complete.

use crate::error::VkCheck;
use crate::instance::Gpu;
use crate::memory::{Buffer, Location, Memory};
use ash::vk;
use engine_render::{PixelRect, RenderError, Result, UniformSlot};
use std::collections::{HashMap, VecDeque};

// ------------------------------------------------------------ deferred release

/// Objects waiting for in-flight frames to finish before they are destroyed.
pub(crate) struct DeferredQueue<T> {
    items: VecDeque<(u64, T)>,
}

impl<T> Default for DeferredQueue<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
        }
    }
}

impl<T> DeferredQueue<T> {
    /// Queue `item`, which frames `0..submitted` may still be using.
    pub(crate) fn push(&mut self, submitted: u64, item: T) {
        debug_assert!(self.items.back().is_none_or(|(last, _)| *last <= submitted));
        self.items.push_back((submitted, item));
    }

    /// Remove every item whose frames are complete (`completed` frames done).
    pub(crate) fn drain_ready(&mut self, completed: u64) -> Vec<T> {
        let mut ready = Vec::new();
        while self
            .items
            .front()
            .is_some_and(|(submitted, _)| *submitted <= completed)
        {
            ready.extend(self.items.pop_front().map(|(_, item)| item));
        }
        ready
    }

    /// Remove everything (the GPU is idle or the device is shutting down).
    pub(crate) fn drain_all(&mut self) -> Vec<T> {
        self.items.drain(..).map(|(_, item)| item).collect()
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }
}

// --------------------------------------------------------------- uniform arena

/// Largest dynamic offset a descriptor can carry.
const MAX_ARENA_BYTES: u64 = u32::MAX as u64;

/// Lays one frame's uniform blocks out in a single arena.
///
/// Each block starts at a multiple of `minUniformBufferOffsetAlignment`. A block
/// identical to the previous block of the same slot reuses its offset, so
/// per-frame uniforms like a camera are written once.
pub(crate) struct UniformPlanner<'a> {
    alignment: u64,
    cursor: u64,
    last: HashMap<UniformSlot, (u32, &'a [u8])>,
    writes: Vec<(u64, &'a [u8])>,
}

impl<'a> UniformPlanner<'a> {
    pub(crate) fn new(alignment: u64) -> Self {
        Self {
            alignment: alignment.max(1),
            cursor: 0,
            last: HashMap::new(),
            writes: Vec::new(),
        }
    }

    /// Offset of `bytes` for `slot`, allocating (and recording a write) unless it
    /// equals the slot's previous block.
    pub(crate) fn place(&mut self, slot: UniformSlot, bytes: &'a [u8]) -> Result<u32> {
        if let Some((offset, previous)) = self.last.get(&slot)
            && *previous == bytes
        {
            return Ok(*offset);
        }
        let start = crate::translate::align_up(self.cursor, self.alignment);
        let end = start + bytes.len() as u64;
        if end > MAX_ARENA_BYTES {
            return Err(RenderError::Backend(
                "uniform data for one frame exceeds 4 GiB".into(),
            ));
        }
        let offset = start as u32;
        self.cursor = end;
        self.last.insert(slot, (offset, bytes));
        self.writes.push((start, bytes));
        Ok(offset)
    }

    /// Bytes the arena must hold.
    pub(crate) fn size(&self) -> u64 {
        self.cursor
    }

    pub(crate) fn writes(&self) -> &[(u64, &'a [u8])] {
        &self.writes
    }
}

/// Capacity to grow an arena to: at least `needed`, doubling, never below 64 KiB.
pub(crate) fn grown_capacity(current: u64, needed: u64) -> u64 {
    const MIN: u64 = 64 * 1024;
    needed.max(current.saturating_mul(2)).max(MIN)
}

// ------------------------------------------------------------- bind tracking

/// Everything a draw binds, as comparable handles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DrawBinds {
    pub pipeline: vk::Pipeline,
    pub layout: vk::PipelineLayout,
    pub vertex: vk::Buffer,
    pub index: Option<vk::Buffer>,
    /// `(set index, descriptor set, dynamic offsets)` for every non-empty set.
    pub sets: Vec<(u32, vk::DescriptorSet, Vec<u32>)>,
    pub scissor: PixelRect,
}

/// Which binds a draw needs relative to the previous one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BindDelta {
    pub pipeline: bool,
    pub vertex: bool,
    pub index: bool,
    pub scissor: bool,
    /// Indices into [`DrawBinds::sets`] that must be (re)bound.
    pub sets: Vec<usize>,
}

impl BindDelta {
    /// A draw is a new batch when anything had to be rebound.
    pub(crate) fn is_batch(&self) -> bool {
        self.pipeline || self.vertex || self.index || self.scissor || !self.sets.is_empty()
    }
}

/// Compare consecutive draws so redundant binds are skipped and batches counted.
#[derive(Default)]
pub(crate) struct BindTracker {
    previous: Option<DrawBinds>,
    batches: usize,
}

impl BindTracker {
    pub(crate) fn next(&mut self, binds: &DrawBinds) -> BindDelta {
        let delta = match &self.previous {
            None => BindDelta {
                pipeline: true,
                vertex: true,
                index: binds.index.is_some(),
                scissor: true,
                sets: (0..binds.sets.len()).collect(),
            },
            Some(prev) => {
                // A different pipeline layout may invalidate bound sets, so all
                // sets are rebound then; otherwise only changed ones.
                let layout_changed = prev.layout != binds.layout;
                BindDelta {
                    pipeline: prev.pipeline != binds.pipeline,
                    vertex: prev.vertex != binds.vertex,
                    index: binds.index.is_some() && prev.index != binds.index,
                    scissor: prev.scissor != binds.scissor,
                    sets: binds
                        .sets
                        .iter()
                        .enumerate()
                        .filter(|(_, set)| layout_changed || !prev.sets.contains(set))
                        .map(|(i, _)| i)
                        .collect(),
                }
            }
        };
        if delta.is_batch() {
            self.batches += 1;
        }
        self.previous = Some(binds.clone());
        delta
    }

    pub(crate) fn batches(&self) -> usize {
        self.batches
    }
}

// ------------------------------------------------------------ descriptor pools

const POOL_MAX_SETS: u32 = 128;
const POOL_UNIFORMS: u32 = 256;
const POOL_IMAGES: u32 = 128;
const POOL_SAMPLERS: u32 = 128;

/// Descriptor pools of one frame slot; all sets are freed by resetting the pools
/// when the slot is reused. More pools are added when one runs out.
#[derive(Default)]
pub(crate) struct DescriptorPools {
    pools: Vec<vk::DescriptorPool>,
    current: usize,
}

impl DescriptorPools {
    pub(crate) fn reset(&mut self, device: &ash::Device) -> Result<()> {
        for &pool in &self.pools {
            // SAFETY: the slot's fence was waited, so no set from this pool is in use.
            unsafe { device.reset_descriptor_pool(pool, vk::DescriptorPoolResetFlags::empty()) }
                .vk("vkResetDescriptorPool")?;
        }
        self.current = 0;
        Ok(())
    }

    fn add_pool(&mut self, device: &ash::Device) -> Result<()> {
        let sizes = [
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                .descriptor_count(POOL_UNIFORMS),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(POOL_IMAGES),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::SAMPLER)
                .descriptor_count(POOL_SAMPLERS),
        ];
        let info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(POOL_MAX_SETS)
            .pool_sizes(&sizes);
        // SAFETY: `info` references `sizes`, alive for the call.
        let pool =
            unsafe { device.create_descriptor_pool(&info, None) }.vk("vkCreateDescriptorPool")?;
        self.pools.push(pool);
        Ok(())
    }

    pub(crate) fn allocate(
        &mut self,
        device: &ash::Device,
        layout: vk::DescriptorSetLayout,
    ) -> Result<vk::DescriptorSet> {
        loop {
            if self.current >= self.pools.len() {
                self.add_pool(device)?;
            }
            let layouts = [layout];
            let info = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(self.pools[self.current])
                .set_layouts(&layouts);
            // SAFETY: the pool and layout belong to this device.
            match unsafe { device.allocate_descriptor_sets(&info) } {
                Ok(sets) => return Ok(sets[0]),
                Err(vk::Result::ERROR_OUT_OF_POOL_MEMORY | vk::Result::ERROR_FRAGMENTED_POOL) => {
                    if self.pools.get(self.current + 1).is_none() {
                        self.add_pool(device)?;
                    }
                    self.current += 1;
                }
                Err(result) => {
                    return Err(crate::error::from_vk("vkAllocateDescriptorSets", result));
                }
            }
        }
    }

    pub(crate) fn destroy(&mut self, device: &ash::Device) {
        for pool in self.pools.drain(..) {
            // SAFETY: the device is idle; destroying a pool frees its sets.
            unsafe { device.destroy_descriptor_pool(pool, None) };
        }
        self.current = 0;
    }
}

// ------------------------------------------------------------------ frame slot

/// Key of a cached descriptor set within one frame:
/// `(shader id, set index, bound texture id)`; `None` is the white texture.
pub(crate) type SetKey = (u64, u32, Option<u64>);

/// Per-slot state for one frame in flight.
pub(crate) struct FrameSlot {
    pub pool: vk::CommandPool,
    pub cmd: vk::CommandBuffer,
    pub fence: vk::Fence,
    pub image_available: vk::Semaphore,
    pub descriptors: DescriptorPools,
    pub arena: Option<Buffer>,
    pub set_cache: HashMap<SetKey, vk::DescriptorSet>,
    /// Internal frame number currently using this slot, until its fence is waited.
    pub in_flight: Option<u64>,
}

impl FrameSlot {
    pub(crate) fn new(device: &ash::Device, queue_family: u32) -> Result<Self> {
        let mut slot = Self {
            pool: vk::CommandPool::null(),
            cmd: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            image_available: vk::Semaphore::null(),
            descriptors: DescriptorPools::default(),
            arena: None,
            set_cache: HashMap::new(),
            in_flight: None,
        };
        // On error the partly built slot is dropped by the caller's teardown, so
        // push it first and fill it in place.
        match slot.populate(device, queue_family) {
            Ok(()) => Ok(slot),
            Err(error) => {
                slot.destroy_vulkan(device);
                Err(error)
            }
        }
    }

    fn populate(&mut self, device: &ash::Device, queue_family: u32) -> Result<()> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        // SAFETY: valid create-info for this device.
        self.pool =
            unsafe { device.create_command_pool(&pool_info, None) }.vk("vkCreateCommandPool")?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the pool was just created from this device.
        self.cmd =
            unsafe { device.allocate_command_buffers(&alloc) }.vk("vkAllocateCommandBuffers")?[0];
        let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        // SAFETY: valid create-info for this device.
        self.fence = unsafe { device.create_fence(&fence_info, None) }.vk("vkCreateFence")?;
        self.image_available = create_semaphore(device)?;
        Ok(())
    }

    /// Wait until the GPU finished the frame that last used this slot. Returns
    /// that frame's number so the caller can advance its completed counter.
    pub(crate) fn wait(&mut self, device: &ash::Device) -> Result<Option<u64>> {
        let Some(frame) = self.in_flight else {
            return Ok(None);
        };
        // SAFETY: the fence belongs to this device.
        unsafe { device.wait_for_fences(&[self.fence], true, u64::MAX) }.vk("vkWaitForFences")?;
        self.in_flight = None;
        Ok(Some(frame))
    }

    /// Make the arena at least `needed` bytes. The slot's frame must be complete.
    pub(crate) fn ensure_arena(&mut self, memory: &mut Memory, needed: u64) -> Result<()> {
        let current = self.arena.as_ref().map_or(0, |a| a.size);
        if needed == 0 || current >= needed {
            return Ok(());
        }
        let buffer = memory.create_buffer(
            "uniform arena",
            grown_capacity(current, needed),
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            Location::Upload,
        )?;
        if let Some(old) = self.arena.replace(buffer) {
            // The slot's previous frame is complete, so nothing uses the old arena.
            memory.destroy_buffer(old);
        }
        Ok(())
    }

    /// Replace the acquire semaphore (after a swapchain change an unconsumed
    /// signal could otherwise linger).
    pub(crate) fn recreate_semaphore(&mut self, device: &ash::Device) -> Result<()> {
        let fresh = create_semaphore(device)?;
        // SAFETY: the device is idle when swapchains are recreated.
        unsafe { device.destroy_semaphore(self.image_available, None) };
        self.image_available = fresh;
        Ok(())
    }

    fn destroy_vulkan(&mut self, device: &ash::Device) {
        self.descriptors.destroy(device);
        // SAFETY: the device is idle; destroying null handles is a no-op, and
        // destroying the pool frees its command buffer.
        unsafe {
            device.destroy_semaphore(self.image_available, None);
            device.destroy_fence(self.fence, None);
            device.destroy_command_pool(self.pool, None);
        }
        self.image_available = vk::Semaphore::null();
        self.fence = vk::Fence::null();
        self.pool = vk::CommandPool::null();
        self.cmd = vk::CommandBuffer::null();
        self.set_cache.clear();
    }

    /// Destroy everything. The device must be idle.
    pub(crate) fn destroy(&mut self, gpu: &mut Gpu) {
        if let Some(arena) = self.arena.take() {
            gpu.memory().destroy_buffer(arena);
        }
        self.destroy_vulkan(&gpu.device);
    }
}

pub(crate) fn create_semaphore(device: &ash::Device) -> Result<vk::Semaphore> {
    let info = vk::SemaphoreCreateInfo::default();
    // SAFETY: valid create-info for this device.
    unsafe { device.create_semaphore(&info, None) }.vk("vkCreateSemaphore")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(group: u32, binding: u32) -> UniformSlot {
        UniformSlot::new(group, binding)
    }

    #[test]
    fn deferred_items_wait_for_their_frames_and_release_in_order() {
        let mut queue = DeferredQueue::default();
        queue.push(0, "a"); // retired before any frame was submitted
        queue.push(2, "b"); // frames 0 and 1 may use it
        queue.push(2, "c");
        queue.push(5, "d");
        assert_eq!(queue.len(), 4);
        assert_eq!(queue.drain_ready(0), vec!["a"]);
        assert!(queue.drain_ready(1).is_empty(), "frame 1 not complete yet");
        assert_eq!(queue.drain_ready(2), vec!["b", "c"]);
        assert_eq!(queue.drain_ready(4), Vec::<&str>::new());
        assert_eq!(queue.drain_ready(9), vec!["d"]);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn drain_all_empties_the_queue_regardless_of_progress() {
        let mut queue = DeferredQueue::default();
        queue.push(1, 10);
        queue.push(7, 20);
        assert_eq!(queue.drain_all(), vec![10, 20]);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn uniform_blocks_are_aligned_and_never_overlap() {
        let camera = [1u8; 64];
        let tint = [2u8; 16];
        let other = [3u8; 80];
        let mut planner = UniformPlanner::new(256);
        assert_eq!(planner.place(slot(0, 0), &camera).unwrap(), 0);
        assert_eq!(planner.place(slot(0, 1), &tint).unwrap(), 256);
        assert_eq!(planner.place(slot(0, 0), &other).unwrap(), 512);
        assert_eq!(planner.size(), 592);
        let writes = planner.writes();
        assert_eq!(writes.len(), 3);
        for pair in writes.windows(2) {
            assert!(pair[0].0 + pair[0].1.len() as u64 <= pair[1].0);
        }
        assert!(writes.iter().all(|(offset, _)| offset % 256 == 0));
    }

    #[test]
    fn identical_consecutive_blocks_for_a_slot_share_one_write() {
        let camera = [7u8; 64];
        let mut planner = UniformPlanner::new(64);
        let first = planner.place(slot(0, 0), &camera).unwrap();
        let second = planner.place(slot(0, 0), &camera).unwrap();
        assert_eq!(first, second);
        assert_eq!(planner.writes().len(), 1);
        assert_eq!(planner.size(), 64);
        // Another slot with the same bytes still gets its own block.
        let third = planner.place(slot(0, 1), &camera).unwrap();
        assert_eq!(third, 64);
        // Changing the data, then reverting, allocates again.
        let changed = [8u8; 64];
        planner.place(slot(0, 0), &changed).unwrap();
        let back = planner.place(slot(0, 0), &camera).unwrap();
        assert_eq!(back, 192);
    }

    #[test]
    fn alignment_of_one_packs_blocks_tightly_and_zero_is_treated_as_one() {
        let bytes = [0u8; 5];
        let other = [1u8; 3];
        let mut planner = UniformPlanner::new(0);
        assert_eq!(planner.place(slot(0, 0), &bytes).unwrap(), 0);
        assert_eq!(planner.place(slot(0, 1), &other).unwrap(), 5);
        assert_eq!(planner.size(), 8);
    }

    #[test]
    fn arena_growth_doubles_and_has_a_floor() {
        assert_eq!(grown_capacity(0, 100), 64 * 1024);
        assert_eq!(grown_capacity(64 * 1024, 70_000), 128 * 1024);
        assert_eq!(grown_capacity(64 * 1024, 1 << 20), 1 << 20);
    }

    fn binds(pipeline: u64, vertex: u64, offsets: &[u32], scissor: PixelRect) -> DrawBinds {
        use ash::vk::Handle;
        DrawBinds {
            pipeline: vk::Pipeline::from_raw(pipeline),
            layout: vk::PipelineLayout::from_raw(1),
            vertex: vk::Buffer::from_raw(vertex),
            index: None,
            sets: vec![(0, vk::DescriptorSet::from_raw(9), offsets.to_vec())],
            scissor,
        }
    }

    #[test]
    fn first_draw_binds_everything_and_identical_draws_bind_nothing() {
        let rect = PixelRect::new(0, 0, 8, 8);
        let a = binds(1, 1, &[0], rect);
        let mut tracker = BindTracker::default();
        let first = tracker.next(&a);
        assert!(first.pipeline && first.vertex && first.scissor);
        assert_eq!(first.sets, vec![0]);
        let again = tracker.next(&a);
        assert_eq!(again, BindDelta::default());
        assert!(!again.is_batch());
        assert_eq!(tracker.batches(), 1);
    }

    #[test]
    fn each_kind_of_change_is_reported_and_counts_one_batch() {
        let rect = PixelRect::new(0, 0, 8, 8);
        let mut tracker = BindTracker::default();
        tracker.next(&binds(1, 1, &[0], rect));
        let pipeline = tracker.next(&binds(2, 1, &[0], rect));
        assert!(pipeline.pipeline && !pipeline.vertex && pipeline.sets.is_empty());
        let mesh = tracker.next(&binds(2, 5, &[0], rect));
        assert!(mesh.vertex && !mesh.pipeline);
        let uniform = tracker.next(&binds(2, 5, &[256], rect));
        assert_eq!(uniform.sets, vec![0]);
        assert!(!uniform.pipeline && !uniform.vertex);
        let scissor = tracker.next(&binds(2, 5, &[256], PixelRect::new(1, 1, 2, 2)));
        assert!(scissor.scissor && scissor.sets.is_empty());
        assert_eq!(tracker.batches(), 5);
    }

    #[test]
    fn a_new_pipeline_layout_rebinds_every_set() {
        use ash::vk::Handle;
        let rect = PixelRect::new(0, 0, 8, 8);
        let mut tracker = BindTracker::default();
        let a = binds(1, 1, &[0], rect);
        tracker.next(&a);
        let mut b = a.clone();
        b.pipeline = vk::Pipeline::from_raw(2);
        b.layout = vk::PipelineLayout::from_raw(2);
        let delta = tracker.next(&b);
        assert_eq!(
            delta.sets,
            vec![0],
            "identical set must rebind after a layout change"
        );
    }

    #[test]
    fn index_buffer_binds_only_for_indexed_draws_when_it_changes() {
        use ash::vk::Handle;
        let rect = PixelRect::new(0, 0, 8, 8);
        let mut tracker = BindTracker::default();
        let mut a = binds(1, 1, &[0], rect);
        a.index = Some(vk::Buffer::from_raw(3));
        assert!(tracker.next(&a).index);
        assert!(!tracker.next(&a).index);
        let mut b = a.clone();
        b.index = Some(vk::Buffer::from_raw(4));
        assert!(tracker.next(&b).index);
        let mut c = a;
        c.index = None;
        assert!(!tracker.next(&c).index);
    }
}
