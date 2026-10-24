//! Draw commands and the per-frame command buffer.

use crate::error::{RenderError, Result};
use crate::resources::{MeshHandle, ShaderHandle, TextureHandle};
use crate::state::DrawState;
use std::collections::BTreeSet;

/// Bind-group index and binding number a uniform buffer occupies in the WGSL
/// (`@group(g) @binding(b) var<uniform>`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UniformSlot {
    pub group: u32,
    pub binding: u32,
}

impl UniformSlot {
    pub const fn new(group: u32, binding: u32) -> Self {
        Self { group, binding }
    }
}

/// Conventional texture binding: `@group(1) @binding(0) var t: texture_2d<f32>`.
/// A draw's optional [`TextureHandle`] is bound here; draws without one bind a
/// 1x1 opaque white texture.
pub const TEXTURE_SLOT: UniformSlot = UniformSlot::new(1, 0);
/// Conventional sampler binding: `@group(1) @binding(1) var s: sampler`. The
/// sampler is always nearest-filtered with clamp-to-edge addressing.
pub const SAMPLER_SLOT: UniformSlot = UniformSlot::new(1, 1);
/// Directional shadow depth map generated before the color pass.
pub const SHADOW_SLOT: UniformSlot = UniformSlot::new(3, 0);

#[derive(Clone, Debug, PartialEq)]
pub struct ShadowCaster {
    pub light_mvp: [[f32; 4]; 4],
    pub resolution: u32,
}

/// Bytes bound to one uniform-buffer slot for a single draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawUniform {
    /// Diagnostic name only; binding is by `slot`.
    pub name: String,
    pub slot: UniformSlot,
    pub bytes: Vec<u8>,
}

impl DrawUniform {
    pub fn from_bytes(
        name: impl Into<String>,
        slot: UniformSlot,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            name: name.into(),
            slot,
            bytes: bytes.into(),
        }
    }

    /// Copy a plain-old-data value (laid out as the WGSL struct expects).
    pub fn from_pod<T: bytemuck::NoUninit>(
        name: impl Into<String>,
        slot: UniformSlot,
        value: &T,
    ) -> Self {
        Self::from_bytes(name, slot, bytemuck::bytes_of(value))
    }
}

/// Stable ordering key: sorts by `producer`, then `draw`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DrawOrderKey {
    pub producer: u64,
    pub draw: u64,
}

impl DrawOrderKey {
    pub const fn new(producer: u64, draw: u64) -> Self {
        Self { producer, draw }
    }
}

/// One mesh drawn with one shader, optional texture, uniforms and state.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawCommand {
    pub shader: ShaderHandle,
    pub mesh: MeshHandle,
    pub texture: Option<TextureHandle>,
    /// Linear-data textures at group 1 bindings 2 (metallic/roughness) and 3 (normal).
    pub data_textures: [Option<TextureHandle>; 2],
    pub uniforms: Vec<DrawUniform>,
    pub state: DrawState,
    /// Assigned by [`RenderCommandBuffer`]: the serial index, or the position
    /// after key sorting. Any value set by the caller is overwritten.
    pub order: u64,
    /// Set by [`RenderCommandBuffer::record_keyed`].
    pub stable_order: Option<DrawOrderKey>,
    /// Opaque triangle geometry participating in the frame's directional shadow map.
    pub shadow: Option<ShadowCaster>,
}

impl DrawCommand {
    pub fn new(shader: ShaderHandle, mesh: MeshHandle) -> Self {
        Self {
            shader,
            mesh,
            texture: None,
            data_textures: [None; 2],
            uniforms: Vec::new(),
            state: DrawState::default(),
            order: 0,
            stable_order: None,
            shadow: None,
        }
    }

    pub fn with_texture(mut self, texture: TextureHandle) -> Self {
        self.texture = Some(texture);
        self
    }

    pub fn with_state(mut self, state: DrawState) -> Self {
        self.state = state;
        self
    }

    pub fn with_uniform(mut self, uniform: DrawUniform) -> Self {
        self.uniforms.push(uniform);
        self
    }

    /// Append a plain-old-data uniform value.
    pub fn append_uniform<T: bytemuck::NoUninit>(
        &mut self,
        name: impl Into<String>,
        slot: UniformSlot,
        value: &T,
    ) {
        self.uniforms.push(DrawUniform::from_pod(name, slot, value));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ordering {
    Serial,
    Keyed,
}

/// Collects the draws of one frame.
///
/// Two mutually exclusive modes: serial (`record`; order follows call order)
/// and keyed (`record_keyed`; stable `(producer, draw)` keys sort the frame,
/// so parallel producers merged in any order yield the same draw order).
/// Duplicate keys and mixing modes are rejected without altering the buffer.
#[derive(Debug, Default)]
pub struct RenderCommandBuffer {
    draws: Vec<DrawCommand>,
    keys: BTreeSet<DrawOrderKey>,
    mode: Option<Ordering>,
}

impl RenderCommandBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serial path: returns the assigned order index.
    pub fn record(&mut self, mut draw: DrawCommand) -> Result<u64> {
        self.enter(Ordering::Serial)?;
        let order = self.draws.len() as u64;
        draw.order = order;
        draw.stable_order = None;
        self.draws.push(draw);
        Ok(order)
    }

    /// Keyed path: keys sort lexicographically by producer, then draw.
    pub fn record_keyed(&mut self, mut draw: DrawCommand, key: DrawOrderKey) -> Result<()> {
        self.enter(Ordering::Keyed)?;
        if !self.keys.insert(key) {
            return Err(RenderError::DuplicateDrawKey {
                producer: key.producer,
                draw: key.draw,
            });
        }
        draw.stable_order = Some(key);
        self.draws.push(draw);
        Ok(())
    }

    /// Merge draws recorded elsewhere (for example by a parallel producer).
    /// Both buffers must use the same mode. In keyed mode a key present in both
    /// is rejected and `self` is left unchanged; in serial mode the other
    /// buffer's draws follow this buffer's.
    pub fn merge(&mut self, other: RenderCommandBuffer) -> Result<()> {
        let Some(mode) = other.mode else {
            return Ok(());
        };
        self.enter(mode)?;
        if mode == Ordering::Keyed {
            if let Some(key) = other.keys.iter().find(|key| self.keys.contains(key)) {
                return Err(RenderError::DuplicateDrawKey {
                    producer: key.producer,
                    draw: key.draw,
                });
            }
            self.keys.extend(other.keys);
            self.draws.extend(other.draws);
        } else {
            for draw in other.draws {
                self.record(draw)?;
            }
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.draws.len()
    }

    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }

    /// Draws in submission order, with `order` renumbered to the final position.
    pub fn into_ordered(mut self) -> Vec<DrawCommand> {
        if self.mode == Some(Ordering::Keyed) {
            self.draws.sort_by_key(|draw| draw.stable_order);
        }
        for (index, draw) in self.draws.iter_mut().enumerate() {
            draw.order = index as u64;
        }
        self.draws
    }

    fn enter(&mut self, wanted: Ordering) -> Result<()> {
        match self.mode {
            None => {
                self.mode = Some(wanted);
                Ok(())
            }
            Some(mode) if mode == wanted => Ok(()),
            Some(_) => Err(RenderError::MixedDrawOrdering),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(tag: u64) -> DrawCommand {
        DrawCommand::new(ShaderHandle::from_raw(1), MeshHandle::from_raw(tag))
    }
    fn meshes(draws: &[DrawCommand]) -> Vec<u64> {
        draws.iter().map(|d| d.mesh.id()).collect()
    }

    #[test]
    fn serial_recording_preserves_call_order_and_assigns_indices() {
        let mut buffer = RenderCommandBuffer::new();
        assert!(buffer.is_empty());
        assert_eq!(buffer.record(draw(9)).unwrap(), 0);
        assert_eq!(buffer.record(draw(3)).unwrap(), 1);
        assert_eq!(buffer.len(), 2);
        let draws = buffer.into_ordered();
        assert_eq!(meshes(&draws), [9, 3]);
        assert_eq!((draws[0].order, draws[1].order), (0, 1));
    }

    #[test]
    fn keyed_recording_sorts_by_producer_then_draw_regardless_of_arrival() {
        let mut buffer = RenderCommandBuffer::new();
        for (tag, producer, index) in [(1, 2, 0), (2, 1, 5), (3, 1, 1), (4, 2, 1)] {
            buffer
                .record_keyed(draw(tag), DrawOrderKey::new(producer, index))
                .unwrap();
        }
        let draws = buffer.into_ordered();
        assert_eq!(meshes(&draws), [3, 2, 1, 4]);
        assert_eq!(
            draws.iter().map(|d| d.order).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn duplicate_key_is_rejected_and_leaves_the_buffer_unchanged() {
        let mut buffer = RenderCommandBuffer::new();
        buffer
            .record_keyed(draw(1), DrawOrderKey::new(0, 0))
            .unwrap();
        let error = buffer
            .record_keyed(draw(2), DrawOrderKey::new(0, 0))
            .unwrap_err();
        assert_eq!(
            error,
            RenderError::DuplicateDrawKey {
                producer: 0,
                draw: 0
            }
        );
        assert_eq!(buffer.len(), 1);
        buffer
            .record_keyed(draw(3), DrawOrderKey::new(0, 1))
            .unwrap();
    }

    #[test]
    fn mixing_serial_and_keyed_is_rejected_in_both_directions() {
        let mut serial_first = RenderCommandBuffer::new();
        serial_first.record(draw(1)).unwrap();
        assert_eq!(
            serial_first.record_keyed(draw(2), DrawOrderKey::default()),
            Err(RenderError::MixedDrawOrdering)
        );
        let mut keyed_first = RenderCommandBuffer::new();
        keyed_first
            .record_keyed(draw(1), DrawOrderKey::default())
            .unwrap();
        assert_eq!(
            keyed_first.record(draw(2)),
            Err(RenderError::MixedDrawOrdering)
        );
        assert_eq!(serial_first.len() + keyed_first.len(), 2);
    }

    #[test]
    fn merging_keyed_producers_is_order_independent() {
        let producer = |id: u64, count: u64| {
            let mut buffer = RenderCommandBuffer::new();
            for index in 0..count {
                buffer
                    .record_keyed(draw(id * 10 + index), DrawOrderKey::new(id, index))
                    .unwrap();
            }
            buffer
        };
        let mut forward = RenderCommandBuffer::new();
        forward.merge(producer(1, 2)).unwrap();
        forward.merge(producer(2, 2)).unwrap();
        let mut backward = RenderCommandBuffer::new();
        backward.merge(producer(2, 2)).unwrap();
        backward.merge(producer(1, 2)).unwrap();
        assert_eq!(
            meshes(&forward.into_ordered()),
            meshes(&backward.into_ordered())
        );
    }

    #[test]
    fn merge_rejects_colliding_keys_and_mode_mismatch_without_partial_effects() {
        let mut target = RenderCommandBuffer::new();
        target
            .record_keyed(draw(1), DrawOrderKey::new(0, 0))
            .unwrap();
        let mut colliding = RenderCommandBuffer::new();
        colliding
            .record_keyed(draw(2), DrawOrderKey::new(0, 1))
            .unwrap();
        colliding
            .record_keyed(draw(3), DrawOrderKey::new(0, 0))
            .unwrap();
        assert!(matches!(
            target.merge(colliding),
            Err(RenderError::DuplicateDrawKey { .. })
        ));
        assert_eq!(target.len(), 1);
        let mut serial = RenderCommandBuffer::new();
        serial.record(draw(4)).unwrap();
        assert_eq!(target.merge(serial), Err(RenderError::MixedDrawOrdering));
        assert!(target.merge(RenderCommandBuffer::new()).is_ok());
    }

    #[test]
    fn serial_merge_appends_after_existing_draws() {
        let mut a = RenderCommandBuffer::new();
        a.record(draw(1)).unwrap();
        let mut b = RenderCommandBuffer::new();
        b.record(draw(2)).unwrap();
        a.merge(b).unwrap();
        assert_eq!(meshes(&a.into_ordered()), [1, 2]);
    }

    #[test]
    fn pod_uniform_helper_copies_exact_bytes() {
        let mut command = draw(1);
        command.append_uniform("camera", UniformSlot::new(0, 0), &[1.0f32, 2.0, 3.0, 4.0]);
        assert_eq!(command.uniforms[0].bytes.len(), 16);
        assert_eq!(&command.uniforms[0].bytes[4..8], &2.0f32.to_ne_bytes());
        assert_eq!(command.uniforms[0].slot, UniformSlot::new(0, 0));
    }
}
