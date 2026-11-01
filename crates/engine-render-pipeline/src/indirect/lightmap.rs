//! Per-instance bindings of supplied lightmaps to built-in lit mesh draws.
//!
//! A binding selects one of the view's prepared lightmaps, an atlas region
//! (`uv = uv2 * scale + offset`) and whether the baked diffuse replaces or adds
//! to the probe/ambient diffuse term. It is carried in the reserved `params`
//! words of the 208-byte `EngineInstance`, so the instance layout, the Hi-Z
//! compaction shader and custom materials are unchanged. This consumes supplied
//! data only; nothing here bakes, unwraps or denoises.
use super::MAX_LIGHTMAPS;
use anyhow::{Result, ensure};

/// How the baked diffuse combines with the probe/ambient diffuse of the draw.
/// Specular image-based lighting is never affected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LightmapMode {
    /// The lightmap supplies the diffuse indirect term; the probe or ambient
    /// diffuse is not added.
    #[default]
    Replace,
    /// The lightmap diffuse is added to the probe or ambient diffuse.
    Add,
}

/// `params.y` bit set when the instance samples a lightmap.
pub const LIGHTMAP_FLAG_ENABLED: u32 = 1;
/// `params.y` bit selecting [`LightmapMode::Add`].
pub const LIGHTMAP_FLAG_ADD: u32 = 2;

/// Smallest atlas extent representable by the 16-bit instance encoding.
const MIN_EXTENT: f32 = 1.0 / 65535.0;
const REGION_EPSILON: f32 = 1e-6;

/// A validated lightmap region for one mesh instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightmapBinding {
    lightmap: usize,
    scale: [f32; 2],
    offset: [f32; 2],
    mode: LightmapMode,
}

impl LightmapBinding {
    /// `lightmap` indexes the view's prepared lightmaps. The region
    /// `[offset, offset + scale]` must lie inside the atlas: scale in
    /// `(0, 1]`, offset in `[0, 1]`, finite. The instance stores both as
    /// unsigned 16-bit fractions (step 1/65535 of the atlas).
    pub fn new(
        lightmap: usize,
        scale: [f32; 2],
        offset: [f32; 2],
        mode: LightmapMode,
    ) -> Result<Self> {
        ensure!(
            lightmap < MAX_LIGHTMAPS,
            "lightmap index {lightmap} is not below the limit of {MAX_LIGHTMAPS}"
        );
        ensure!(
            scale.iter().chain(&offset).all(|v| v.is_finite()),
            "lightmap atlas scale and offset must be finite"
        );
        ensure!(
            scale.iter().all(|&s| (MIN_EXTENT..=1.0).contains(&s)),
            "lightmap atlas scale must be within 1/65535..=1, got {scale:?}"
        );
        ensure!(
            offset.iter().all(|&o| (0.0..=1.0).contains(&o)),
            "lightmap atlas offset must be within 0..=1, got {offset:?}"
        );
        ensure!(
            (0..2).all(|i| offset[i] + scale[i] <= 1.0 + REGION_EPSILON),
            "lightmap atlas region (offset {offset:?} + scale {scale:?}) must lie inside the atlas"
        );
        Ok(Self {
            lightmap,
            scale,
            offset,
            mode,
        })
    }

    /// Index into the view's prepared lightmaps.
    pub fn lightmap(&self) -> usize {
        self.lightmap
    }
    pub fn scale(&self) -> [f32; 2] {
        self.scale
    }
    pub fn offset(&self) -> [f32; 2] {
        self.offset
    }
    pub fn mode(&self) -> LightmapMode {
        self.mode
    }

    /// `EngineInstance.params.yzw`: flags, packed scale (x low), packed offset.
    pub(crate) fn instance_words(&self) -> [u32; 3] {
        let flags = LIGHTMAP_FLAG_ENABLED
            | match self.mode {
                LightmapMode::Replace => 0,
                LightmapMode::Add => LIGHTMAP_FLAG_ADD,
            };
        [
            flags,
            pack_unorm2x16(self.scale),
            pack_unorm2x16(self.offset),
        ]
    }
}

/// WGSL `pack2x16unorm`: x in the low half, y in the high half, round to nearest.
pub fn pack_unorm2x16(value: [f32; 2]) -> u32 {
    let one = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u32;
    one(value[0]) | (one(value[1]) << 16)
}

/// Inverse of [`pack_unorm2x16`], as WGSL `unpack2x16unorm` evaluates it.
pub fn unpack_unorm2x16(word: u32) -> [f32; 2] {
    [
        (word & 0xffff) as f32 / 65535.0,
        (word >> 16) as f32 / 65535.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(scale: [f32; 2], offset: [f32; 2]) -> Result<LightmapBinding> {
        LightmapBinding::new(0, scale, offset, LightmapMode::Replace)
    }

    #[test]
    fn valid_regions_round_trip_through_the_instance_words() {
        let binding = LightmapBinding::new(3, [0.5, 0.25], [0.5, 0.75], LightmapMode::Add).unwrap();
        let [flags, scale, offset] = binding.instance_words();
        assert_eq!(flags, LIGHTMAP_FLAG_ENABLED | LIGHTMAP_FLAG_ADD);
        for (got, want) in unpack_unorm2x16(scale).into_iter().zip([0.5, 0.25]) {
            assert!((got - want).abs() <= 1.0 / 65535.0, "{got} vs {want}");
        }
        for (got, want) in unpack_unorm2x16(offset).into_iter().zip([0.5, 0.75]) {
            assert!((got - want).abs() <= 1.0 / 65535.0, "{got} vs {want}");
        }
        let replace = region([1.0, 1.0], [0.0, 0.0]).unwrap().instance_words();
        assert_eq!(replace[0], LIGHTMAP_FLAG_ENABLED);
        assert_eq!(replace[1], 0xffff_ffff);
        assert_eq!(replace[2], 0);
    }

    #[test]
    fn invalid_bindings_are_rejected_with_clear_errors() {
        let message = |binding: Result<LightmapBinding>| format!("{:#}", binding.unwrap_err());
        assert!(
            message(LightmapBinding::new(
                8,
                [1.0; 2],
                [0.0; 2],
                LightmapMode::Add
            ))
            .contains("lightmap index 8")
        );
        assert!(message(region([f32::NAN, 1.0], [0.0; 2])).contains("finite"));
        assert!(message(region([1.0, 1.0], [f32::INFINITY, 0.0])).contains("finite"));
        assert!(message(region([0.0, 1.0], [0.0; 2])).contains("scale"));
        assert!(message(region([-0.5, 1.0], [0.0; 2])).contains("scale"));
        assert!(message(region([1.5, 1.0], [0.0; 2])).contains("scale"));
        assert!(message(region([0.5, 0.5], [-0.1, 0.0])).contains("offset"));
        assert!(message(region([0.5, 0.5], [0.0, 1.5])).contains("offset"));
        assert!(message(region([0.5, 0.5], [0.75, 0.0])).contains("inside the atlas"));
        assert!(
            region([0.5, 0.5], [0.5, 0.5]).is_ok(),
            "touching the edge is inside"
        );
    }

    #[test]
    fn packing_matches_wgsl_pack2x16unorm_ordering() {
        assert_eq!(pack_unorm2x16([1.0, 0.0]), 0x0000_ffff);
        assert_eq!(pack_unorm2x16([0.0, 1.0]), 0xffff_0000);
        assert_eq!(unpack_unorm2x16(0x8000_0000)[1], 32768.0 / 65535.0);
    }
}
