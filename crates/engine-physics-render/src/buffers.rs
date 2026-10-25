//! Bounded, growing GPU buffers rewritten every frame.

/// Smallest buffer allocated, in bytes.
pub const MIN_BUFFER_BYTES: u64 = 4096;

/// Capacity (bytes) a buffer of `current` capacity needs to hold `needed` bytes without
/// exceeding `cap`: the next power of two, at least [`MIN_BUFFER_BYTES`], clamped to `cap`.
/// `None` when `current` already suffices or `needed` exceeds `cap`.
pub fn grown_capacity(current: u64, needed: u64, cap: u64) -> Option<u64> {
    if needed <= current || needed > cap {
        return None;
    }
    Some(needed.next_power_of_two().max(MIN_BUFFER_BYTES).min(cap))
}

/// A GPU buffer that grows by doubling up to a hard cap. Contents are rewritten whole.
pub struct GrowBuffer {
    label: &'static str,
    usage: wgpu::BufferUsages,
    cap_bytes: u64,
    buffer: Option<wgpu::Buffer>,
    capacity: u64,
    /// Bytes written by the last [`Self::write`].
    pub len: u64,
}

impl GrowBuffer {
    pub fn new(label: &'static str, usage: wgpu::BufferUsages, cap_bytes: u64) -> Self {
        Self {
            label,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            cap_bytes,
            buffer: None,
            capacity: 0,
            len: 0,
        }
    }

    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    pub fn buffer(&self) -> Option<&wgpu::Buffer> {
        self.buffer.as_ref()
    }

    /// Writes `bytes` (growing the buffer when needed). Bytes beyond the cap are dropped and
    /// `false` is returned; the callers truncate to whole elements beforehand.
    pub fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) -> bool {
        let fits = bytes.len() as u64 <= self.cap_bytes;
        let bytes = if fits {
            bytes
        } else {
            &bytes[..self.cap_bytes as usize]
        };
        // `write_buffer` needs a multiple of four bytes.
        let padded = (bytes.len() as u64).next_multiple_of(4);
        if let Some(capacity) = grown_capacity(self.capacity, padded, self.cap_bytes) {
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: capacity,
                usage: self.usage,
                mapped_at_creation: false,
            }));
            self.capacity = capacity;
        }
        self.len = bytes.len() as u64;
        if let Some(buffer) = &self.buffer
            && !bytes.is_empty()
        {
            queue.write_buffer(buffer, 0, bytes);
        }
        fits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_grows_by_powers_of_two_and_stops_at_the_cap() {
        assert_eq!(grown_capacity(0, 100, 1 << 20), Some(MIN_BUFFER_BYTES));
        assert_eq!(grown_capacity(4096, 4096, 1 << 20), None);
        assert_eq!(grown_capacity(4096, 5000, 1 << 20), Some(8192));
        assert_eq!(grown_capacity(8192, 70_000, 1 << 20), Some(131_072));
        assert_eq!(grown_capacity(8192, 70_000, 100_000), Some(100_000));
        assert_eq!(grown_capacity(8192, 200_000, 100_000), None);
    }
}
