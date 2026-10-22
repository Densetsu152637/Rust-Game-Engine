//! Pure mappings from `engine-render` types to Vulkan enums and ranges.
//!
//! Nothing here touches a device, so every rule (blend factors, culling,
//! viewport flip, surface selection, readback layout) is unit-tested without a
//! GPU.
//!
//! ## Conventions
//!
//! * Clip space is WebGPU style (y up, depth 0..1). Vulkan's is y down, so the
//!   viewport is flipped with a negative height (`VK_KHR_maintenance1`, core in
//!   1.1) and shaders are left untouched: naga's `ADJUST_COORDINATE_SPACE` is
//!   switched off. Framebuffer row 0 is then the top row, which is what
//!   readback returns.
//! * Front faces are counter-clockwise **as seen on screen** (y up in NDC),
//!   exactly wgpu's default. Because Vulkan measures winding after the viewport
//!   transform, the flipped viewport keeps that meaning, so
//!   [`CullMode::Back`]/[`CullMode::Front`] map straight to Vulkan's
//!   `BACK`/`FRONT` with `COUNTER_CLOCKWISE`.
//! * Vulkan only honors `depthWriteEnable` while `depthTestEnable` is set, but
//!   the engine allows "no test, still write". Depth test is therefore always
//!   enabled, with `ALWAYS` standing in for "no test".

use ash::vk;
use engine_render::shader::{BindingKind, StageUsage};
use engine_render::{BlendMode, CullMode, PipelineState, PixelRect, VertexFormat};

/// Format of every offscreen target and of every sampled texture.
pub(crate) const COLOR_FORMAT: vk::Format = vk::Format::R8G8B8A8_SRGB;
/// Depth format of every target.
pub(crate) const DEPTH_FORMAT: vk::Format = vk::Format::D32_SFLOAT;

pub(crate) fn vertex_format(format: VertexFormat) -> vk::Format {
    match format {
        VertexFormat::Float32 => vk::Format::R32_SFLOAT,
        VertexFormat::Float32x2 => vk::Format::R32G32_SFLOAT,
        VertexFormat::Float32x3 => vk::Format::R32G32B32_SFLOAT,
        VertexFormat::Float32x4 => vk::Format::R32G32B32A32_SFLOAT,
        VertexFormat::Unorm8x4 => vk::Format::R8G8B8A8_UNORM,
    }
}

pub(crate) fn cull_mode(cull: CullMode) -> vk::CullModeFlags {
    match cull {
        CullMode::None => vk::CullModeFlags::NONE,
        CullMode::Front => vk::CullModeFlags::FRONT,
        CullMode::Back => vk::CullModeFlags::BACK,
    }
}

/// Front faces wind counter-clockwise on screen (see the module docs).
pub(crate) const FRONT_FACE: vk::FrontFace = vk::FrontFace::COUNTER_CLOCKWISE;

/// `StraightAlpha` is wgpu's `ALPHA_BLENDING`: color `SRC_ALPHA / ONE_MINUS_SRC_ALPHA`,
/// alpha `ONE / ONE_MINUS_SRC_ALPHA`.
pub(crate) fn color_blend_attachment(mode: BlendMode) -> vk::PipelineColorBlendAttachmentState {
    let base = vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(vk::ColorComponentFlags::RGBA);
    match mode {
        BlendMode::StraightAlpha => base
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD),
        // `Opaque` and any future mode default to replacing the destination.
        _ => base.blend_enable(false),
    }
}

pub(crate) fn depth_compare_op(depth_test: bool) -> vk::CompareOp {
    if depth_test {
        vk::CompareOp::LESS_OR_EQUAL
    } else {
        vk::CompareOp::ALWAYS
    }
}

pub(crate) fn depth_stencil_state(
    state: &PipelineState,
) -> vk::PipelineDepthStencilStateCreateInfo<'static> {
    vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(true)
        .depth_write_enable(state.depth_write)
        .depth_compare_op(depth_compare_op(state.depth_test))
        .min_depth_bounds(0.0)
        .max_depth_bounds(1.0)
}

pub(crate) fn descriptor_type(kind: BindingKind) -> vk::DescriptorType {
    match kind {
        // Uniform data lives in one per-frame arena addressed by dynamic offsets.
        BindingKind::UniformBuffer { .. } => vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC,
        BindingKind::SampledTexture2d => vk::DescriptorType::SAMPLED_IMAGE,
        BindingKind::Sampler => vk::DescriptorType::SAMPLER,
    }
}

pub(crate) fn stage_flags(stages: StageUsage) -> vk::ShaderStageFlags {
    let mut flags = vk::ShaderStageFlags::empty();
    if stages.vertex {
        flags |= vk::ShaderStageFlags::VERTEX;
    }
    if stages.fragment {
        flags |= vk::ShaderStageFlags::FRAGMENT;
    }
    flags
}

/// Viewport rectangle in target pixels: `None` covers the whole target, anything
/// else is clamped to it. May be empty.
pub(crate) fn effective_viewport(
    requested: Option<PixelRect>,
    width: u32,
    height: u32,
) -> PixelRect {
    requested
        .unwrap_or(PixelRect::new(0, 0, width, height))
        .clamped_to(width, height)
}

/// Scissor in target pixels (top-left origin): `None` covers the whole target,
/// anything else is clamped to it. May be empty.
pub(crate) fn effective_scissor(scissor: Option<PixelRect>, width: u32, height: u32) -> PixelRect {
    effective_viewport(scissor, width, height)
}

pub(crate) fn rect_to_vk(rect: PixelRect) -> vk::Rect2D {
    vk::Rect2D {
        // Clamped rectangles are bounded by the target size, far below i32::MAX.
        offset: vk::Offset2D {
            x: rect.x as i32,
            y: rect.y as i32,
        },
        extent: vk::Extent2D {
            width: rect.width,
            height: rect.height,
        },
    }
}

/// Negative-height viewport that flips Vulkan's y-down clip space to y up.
/// `rect` must be non-empty and inside the target.
pub(crate) fn flipped_viewport(rect: PixelRect) -> vk::Viewport {
    vk::Viewport {
        x: rect.x as f32,
        y: (rect.y + rect.height) as f32,
        width: rect.width as f32,
        height: -(rect.height as f32),
        min_depth: 0.0,
        max_depth: 1.0,
    }
}

pub(crate) fn clear_values(color: [f32; 4], depth: f32) -> [vk::ClearValue; 2] {
    [
        vk::ClearValue {
            color: vk::ClearColorValue { float32: color },
        },
        vk::ClearValue {
            depth_stencil: vk::ClearDepthStencilValue {
                depth: depth.clamp(0.0, 1.0),
                stencil: 0,
            },
        },
    ]
}

pub(crate) fn align_up(value: u64, alignment: u64) -> u64 {
    let alignment = alignment.max(1);
    value.div_ceil(alignment) * alignment
}

// ------------------------------------------------------------------ surface

/// Prefer BGRA8 sRGB, then RGBA8 sRGB, in the sRGB-nonlinear color space.
/// Textures and targets are sRGB, so a UNORM swapchain would present different
/// colors; such surfaces are rejected rather than silently darkened.
pub(crate) fn choose_surface_format(
    available: &[vk::SurfaceFormatKHR],
) -> Option<vk::SurfaceFormatKHR> {
    [vk::Format::B8G8R8A8_SRGB, vk::Format::R8G8B8A8_SRGB]
        .into_iter()
        .find_map(|wanted| {
            available.iter().copied().find(|candidate| {
                candidate.format == wanted
                    && candidate.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
            })
        })
}

fn is_bgra(format: vk::Format) -> bool {
    matches!(
        format,
        vk::Format::B8G8R8A8_SRGB | vk::Format::B8G8R8A8_UNORM
    )
}

/// Swapchain extent: the surface's fixed extent when it has one, otherwise the
/// requested size clamped to the surface limits. Zero means "suspended".
pub(crate) fn choose_extent(
    caps: &vk::SurfaceCapabilitiesKHR,
    requested: (u32, u32),
) -> vk::Extent2D {
    if caps.current_extent.width != u32::MAX {
        return caps.current_extent;
    }
    vk::Extent2D {
        width: requested
            .0
            .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
        height: requested
            .1
            .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
    }
}

/// One more than the minimum so the CPU never waits on the presentation engine,
/// bounded by the surface maximum (0 means unbounded).
pub(crate) fn choose_image_count(caps: &vk::SurfaceCapabilitiesKHR) -> u32 {
    let wanted = caps.min_image_count + 1;
    if caps.max_image_count == 0 {
        wanted
    } else {
        wanted.min(caps.max_image_count)
    }
}

pub(crate) fn choose_composite_alpha(
    caps: &vk::SurfaceCapabilitiesKHR,
) -> vk::CompositeAlphaFlagsKHR {
    [
        vk::CompositeAlphaFlagsKHR::OPAQUE,
        vk::CompositeAlphaFlagsKHR::INHERIT,
        vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
        vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
    ]
    .into_iter()
    .find(|flag| caps.supported_composite_alpha.contains(*flag))
    .unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE)
}

// ----------------------------------------------------------------- readback

/// Byte order of the texels in a mapped readback buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadbackOrder {
    Rgba,
    Bgra,
}

impl ReadbackOrder {
    /// Byte order the texels of an image in `format` are stored in.
    pub(crate) fn for_format(format: vk::Format) -> Self {
        if is_bgra(format) {
            Self::Bgra
        } else {
            Self::Rgba
        }
    }
}

/// Repack mapped texels into tightly packed RGBA rows, top row first.
/// `row_pitch` is the byte distance between source rows (at least `width * 4`).
/// Returns `None` when `src` is too short or the sizes are inconsistent.
pub(crate) fn repack_readback(
    src: &[u8],
    width: u32,
    height: u32,
    row_pitch: usize,
    order: ReadbackOrder,
) -> Option<Vec<u8>> {
    let row_bytes = (width as usize).checked_mul(4)?;
    if row_pitch < row_bytes || width == 0 || height == 0 {
        return None;
    }
    let needed = row_pitch
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if src.len() < needed {
        return None;
    }
    let mut out = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let line = &src[row * row_pitch..row * row_pitch + row_bytes];
        match order {
            ReadbackOrder::Rgba => out.extend_from_slice(line),
            ReadbackOrder::Bgra => {
                for texel in line.as_chunks::<4>().0 {
                    out.extend_from_slice(&[texel[2], texel[1], texel[0], texel[3]]);
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_formats_map_to_matching_vulkan_formats() {
        assert_eq!(
            vertex_format(VertexFormat::Float32x3),
            vk::Format::R32G32B32_SFLOAT
        );
        assert_eq!(
            vertex_format(VertexFormat::Unorm8x4),
            vk::Format::R8G8B8A8_UNORM
        );
        for format in [
            VertexFormat::Float32,
            VertexFormat::Float32x2,
            VertexFormat::Float32x3,
            VertexFormat::Float32x4,
            VertexFormat::Unorm8x4,
        ] {
            // Every engine format is exactly as wide as its Vulkan counterpart.
            let bits = match vertex_format(format) {
                vk::Format::R32_SFLOAT | vk::Format::R8G8B8A8_UNORM => 32,
                vk::Format::R32G32_SFLOAT => 64,
                vk::Format::R32G32B32_SFLOAT => 96,
                vk::Format::R32G32B32A32_SFLOAT => 128,
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(bits, format.byte_size() * 8);
        }
    }

    #[test]
    fn cull_modes_map_directly_with_counter_clockwise_fronts() {
        assert_eq!(cull_mode(CullMode::None), vk::CullModeFlags::NONE);
        assert_eq!(cull_mode(CullMode::Back), vk::CullModeFlags::BACK);
        assert_eq!(cull_mode(CullMode::Front), vk::CullModeFlags::FRONT);
        assert_eq!(FRONT_FACE, vk::FrontFace::COUNTER_CLOCKWISE);
    }

    #[test]
    fn straight_alpha_matches_wgpu_alpha_blending() {
        let blend = color_blend_attachment(BlendMode::StraightAlpha);
        assert_eq!(blend.blend_enable, vk::TRUE);
        assert_eq!(blend.src_color_blend_factor, vk::BlendFactor::SRC_ALPHA);
        assert_eq!(
            blend.dst_color_blend_factor,
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA
        );
        assert_eq!(blend.src_alpha_blend_factor, vk::BlendFactor::ONE);
        assert_eq!(
            blend.dst_alpha_blend_factor,
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA
        );
        assert_eq!(blend.color_blend_op, vk::BlendOp::ADD);
        assert_eq!(blend.alpha_blend_op, vk::BlendOp::ADD);
        assert_eq!(blend.color_write_mask, vk::ColorComponentFlags::RGBA);
    }

    #[test]
    fn opaque_disables_blending_but_writes_all_channels() {
        let blend = color_blend_attachment(BlendMode::Opaque);
        assert_eq!(blend.blend_enable, vk::FALSE);
        assert_eq!(blend.color_write_mask, vk::ColorComponentFlags::RGBA);
    }

    #[test]
    fn depth_test_off_still_enables_the_stage_so_writes_stay_possible() {
        let mut state = PipelineState {
            blend: BlendMode::Opaque,
            depth_test: true,
            depth_write: true,
            cull: CullMode::None,
        };
        let on = depth_stencil_state(&state);
        assert_eq!(on.depth_compare_op, vk::CompareOp::LESS_OR_EQUAL);
        assert_eq!(on.depth_write_enable, vk::TRUE);
        state.depth_test = false;
        let off = depth_stencil_state(&state);
        assert_eq!(off.depth_test_enable, vk::TRUE);
        assert_eq!(off.depth_compare_op, vk::CompareOp::ALWAYS);
        state.depth_write = false;
        assert_eq!(depth_stencil_state(&state).depth_write_enable, vk::FALSE);
    }

    #[test]
    fn descriptor_types_and_stages() {
        assert_eq!(
            descriptor_type(BindingKind::UniformBuffer { size: 64 }),
            vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC
        );
        assert_eq!(
            descriptor_type(BindingKind::SampledTexture2d),
            vk::DescriptorType::SAMPLED_IMAGE
        );
        assert_eq!(
            descriptor_type(BindingKind::Sampler),
            vk::DescriptorType::SAMPLER
        );
        assert_eq!(
            stage_flags(StageUsage {
                vertex: true,
                fragment: true
            }),
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT
        );
        assert_eq!(
            stage_flags(StageUsage {
                vertex: false,
                fragment: true
            }),
            vk::ShaderStageFlags::FRAGMENT
        );
    }

    #[test]
    fn viewport_is_flipped_with_negative_height_anchored_at_the_bottom_edge() {
        let full = flipped_viewport(PixelRect::new(0, 0, 64, 32));
        assert_eq!(
            (full.x, full.y, full.width, full.height),
            (0.0, 32.0, 64.0, -32.0)
        );
        let sub = flipped_viewport(PixelRect::new(8, 4, 16, 10));
        assert_eq!(
            (sub.x, sub.y, sub.width, sub.height),
            (8.0, 14.0, 16.0, -10.0)
        );
        assert_eq!((sub.min_depth, sub.max_depth), (0.0, 1.0));
    }

    #[test]
    fn viewport_and_scissor_default_to_the_target_and_clamp() {
        assert_eq!(
            effective_viewport(None, 64, 32),
            PixelRect::new(0, 0, 64, 32)
        );
        assert_eq!(
            effective_viewport(Some(PixelRect::new(60, 10, 20, 100)), 64, 32),
            PixelRect::new(60, 10, 4, 22)
        );
        assert!(effective_scissor(Some(PixelRect::new(100, 100, 5, 5)), 64, 64).is_empty());
        let rect = rect_to_vk(PixelRect::new(3, 5, 7, 9));
        assert_eq!((rect.offset.x, rect.offset.y), (3, 5));
        assert_eq!((rect.extent.width, rect.extent.height), (7, 9));
    }

    #[test]
    fn clear_values_carry_linear_color_and_clamped_depth() {
        let values = clear_values([0.25, 0.5, 0.75, 1.0], 2.0);
        // SAFETY: both union fields were initialized by `clear_values`.
        let (color, depth) = unsafe { (values[0].color.float32, values[1].depth_stencil.depth) };
        assert_eq!(color, [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(depth, 1.0);
    }

    #[test]
    fn align_up_rounds_to_the_alignment() {
        assert_eq!(align_up(0, 256), 0);
        assert_eq!(align_up(1, 256), 256);
        assert_eq!(align_up(256, 256), 256);
        assert_eq!(align_up(257, 64), 320);
        assert_eq!(align_up(5, 0), 5);
    }

    fn format(format: vk::Format, space: vk::ColorSpaceKHR) -> vk::SurfaceFormatKHR {
        vk::SurfaceFormatKHR {
            format,
            color_space: space,
        }
    }

    #[test]
    fn surface_format_prefers_bgra_srgb_then_rgba_srgb_and_rejects_unorm_only() {
        let nl = vk::ColorSpaceKHR::SRGB_NONLINEAR;
        let both = [
            format(vk::Format::R8G8B8A8_SRGB, nl),
            format(vk::Format::B8G8R8A8_SRGB, nl),
        ];
        assert_eq!(
            choose_surface_format(&both).unwrap().format,
            vk::Format::B8G8R8A8_SRGB
        );
        assert_eq!(
            choose_surface_format(&both[..1]).unwrap().format,
            vk::Format::R8G8B8A8_SRGB
        );
        let unorm = [format(vk::Format::B8G8R8A8_UNORM, nl)];
        assert!(choose_surface_format(&unorm).is_none());
        let hdr = [format(
            vk::Format::B8G8R8A8_SRGB,
            vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT,
        )];
        assert!(choose_surface_format(&hdr).is_none());
        assert_eq!(
            ReadbackOrder::for_format(vk::Format::B8G8R8A8_SRGB),
            ReadbackOrder::Bgra
        );
        assert_eq!(
            ReadbackOrder::for_format(vk::Format::R8G8B8A8_SRGB),
            ReadbackOrder::Rgba
        );
    }

    #[test]
    fn extent_uses_the_fixed_surface_extent_or_clamps_the_request() {
        let mut caps = vk::SurfaceCapabilitiesKHR {
            current_extent: vk::Extent2D {
                width: 800,
                height: 600,
            },
            ..Default::default()
        };
        let fixed = choose_extent(&caps, (1, 1));
        assert_eq!((fixed.width, fixed.height), (800, 600));
        caps.current_extent = vk::Extent2D {
            width: u32::MAX,
            height: u32::MAX,
        };
        caps.min_image_extent = vk::Extent2D {
            width: 16,
            height: 16,
        };
        caps.max_image_extent = vk::Extent2D {
            width: 1024,
            height: 512,
        };
        let clamped = choose_extent(&caps, (4096, 4));
        assert_eq!((clamped.width, clamped.height), (1024, 16));
    }

    #[test]
    fn image_count_is_min_plus_one_bounded_by_the_maximum() {
        let mut caps = vk::SurfaceCapabilitiesKHR {
            min_image_count: 2,
            max_image_count: 0,
            ..Default::default()
        };
        assert_eq!(choose_image_count(&caps), 3);
        caps.max_image_count = 2;
        assert_eq!(choose_image_count(&caps), 2);
    }

    #[test]
    fn composite_alpha_prefers_opaque() {
        let caps = vk::SurfaceCapabilitiesKHR {
            supported_composite_alpha: vk::CompositeAlphaFlagsKHR::INHERIT
                | vk::CompositeAlphaFlagsKHR::OPAQUE,
            ..Default::default()
        };
        assert_eq!(
            choose_composite_alpha(&caps),
            vk::CompositeAlphaFlagsKHR::OPAQUE
        );
        let only = vk::SurfaceCapabilitiesKHR {
            supported_composite_alpha: vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            ..Default::default()
        };
        assert_eq!(
            choose_composite_alpha(&only),
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED
        );
    }

    #[test]
    fn readback_keeps_rgba_rows_top_first_and_drops_row_padding() {
        // 2x2 with an 12-byte row pitch (4 bytes of padding per row).
        let src: Vec<u8> = vec![
            1, 2, 3, 4, 5, 6, 7, 8, 0xAA, 0xAA, 0xAA, 0xAA, //
            9, 10, 11, 12, 13, 14, 15, 16, 0xBB, 0xBB, 0xBB, 0xBB,
        ];
        let out = repack_readback(&src, 2, 2, 12, ReadbackOrder::Rgba).unwrap();
        assert_eq!(out, (1..=16).collect::<Vec<u8>>());
    }

    #[test]
    fn readback_swizzles_bgra_to_rgba() {
        let src = [3u8, 2, 1, 255, 30, 20, 10, 128];
        let out = repack_readback(&src, 2, 1, 8, ReadbackOrder::Bgra).unwrap();
        assert_eq!(out, [1, 2, 3, 255, 10, 20, 30, 128]);
    }

    #[test]
    fn readback_rejects_short_or_inconsistent_input() {
        assert!(repack_readback(&[0; 7], 2, 1, 8, ReadbackOrder::Rgba).is_none());
        assert!(repack_readback(&[0; 16], 2, 1, 4, ReadbackOrder::Rgba).is_none());
        assert!(repack_readback(&[0; 16], 0, 1, 8, ReadbackOrder::Rgba).is_none());
        // The last row needs no trailing padding.
        assert!(repack_readback(&[0; 20], 2, 2, 12, ReadbackOrder::Rgba).is_some());
    }
}
