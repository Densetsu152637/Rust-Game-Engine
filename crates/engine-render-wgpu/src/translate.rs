//! Pure mappings from the engine's neutral render types onto wgpu.

use engine_render::shader::ShaderBinding;
use engine_render::{BlendMode, CullMode, PipelineState, PixelRect, TargetFormat, VertexFormat};

/// Color format of every offscreen target and texture.
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Depth format of the depth attachment the backend owns per target.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

pub fn target_format(format: TargetFormat) -> wgpu::TextureFormat {
    match format {
        TargetFormat::Rgba8Srgb => COLOR_FORMAT,
        // `TargetFormat` is non-exhaustive; RGBA8 sRGB is the only format today.
        #[allow(unreachable_patterns)]
        _ => COLOR_FORMAT,
    }
}

pub fn blend_state(blend: BlendMode) -> Option<wgpu::BlendState> {
    match blend {
        BlendMode::Opaque => None,
        BlendMode::StraightAlpha => Some(wgpu::BlendState::ALPHA_BLENDING),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

pub fn cull_face(cull: CullMode) -> Option<wgpu::Face> {
    match cull {
        CullMode::None => None,
        CullMode::Front => Some(wgpu::Face::Front),
        CullMode::Back => Some(wgpu::Face::Back),
    }
}

pub fn depth_compare(depth_test: bool) -> wgpu::CompareFunction {
    if depth_test {
        wgpu::CompareFunction::LessEqual
    } else {
        wgpu::CompareFunction::Always
    }
}

pub fn depth_state(state: &PipelineState) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(state.depth_write),
        depth_compare: Some(depth_compare(state.depth_test)),
        stencil: Default::default(),
        bias: Default::default(),
    }
}

pub fn vertex_format(format: VertexFormat) -> wgpu::VertexFormat {
    match format {
        VertexFormat::Float32 => wgpu::VertexFormat::Float32,
        VertexFormat::Float32x2 => wgpu::VertexFormat::Float32x2,
        VertexFormat::Float32x3 => wgpu::VertexFormat::Float32x3,
        VertexFormat::Float32x4 => wgpu::VertexFormat::Float32x4,
        VertexFormat::Unorm8x4 => wgpu::VertexFormat::Unorm8x4,
    }
}

/// Visibility of a binding from the stages that reference it. A binding no
/// entry point references (not reflected) is visible to both stages.
pub fn shader_stages(binding: &ShaderBinding) -> wgpu::ShaderStages {
    let mut stages = wgpu::ShaderStages::NONE;
    if binding.stages.vertex {
        stages |= wgpu::ShaderStages::VERTEX;
    }
    if binding.stages.fragment {
        stages |= wgpu::ShaderStages::FRAGMENT;
    }
    if stages.is_empty() {
        wgpu::ShaderStages::VERTEX_FRAGMENT
    } else {
        stages
    }
}

/// Round `value` up to a multiple of `alignment` (a nonzero power of two or any
/// positive integer).
pub fn align_up(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment.max(1)) * alignment.max(1)
}

/// The rectangle covered by an optional viewport/scissor on a target.
pub fn effective_rect(rect: Option<PixelRect>, width: u32, height: u32) -> PixelRect {
    rect.unwrap_or(PixelRect::new(0, 0, width, height))
        .clamped_to(width, height)
}

/// Surface format choice: the first sRGB format, else the first offered.
pub fn choose_surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    formats
        .iter()
        .copied()
        .find(|format| format.is_srgb())
        .or_else(|| formats.first().copied())
}

/// Human readable backend label, for example `wgpu/vulkan`.
pub fn backend_label(backend: wgpu::Backend) -> String {
    let name = match backend {
        wgpu::Backend::Vulkan => "vulkan",
        wgpu::Backend::Dx12 => "dx12",
        wgpu::Backend::Metal => "metal",
        wgpu::Backend::Gl => "gl",
        other => return format!("wgpu/{}", format!("{other:?}").to_ascii_lowercase()),
    };
    format!("wgpu/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_render::DrawState;
    use engine_render::shader::StageUsage;

    #[test]
    fn blend_modes_map_to_replace_or_straight_alpha() {
        assert_eq!(blend_state(BlendMode::Opaque), None);
        assert_eq!(
            blend_state(BlendMode::StraightAlpha),
            Some(wgpu::BlendState::ALPHA_BLENDING)
        );
    }

    #[test]
    fn cull_modes_map_to_faces() {
        assert_eq!(cull_face(CullMode::None), None);
        assert_eq!(cull_face(CullMode::Front), Some(wgpu::Face::Front));
        assert_eq!(cull_face(CullMode::Back), Some(wgpu::Face::Back));
    }

    #[test]
    fn depth_flags_map_to_compare_and_write() {
        let state = DrawState {
            depth_test: true,
            depth_write: false,
            ..Default::default()
        }
        .pipeline_state();
        let depth = depth_state(&state);
        assert_eq!(depth.depth_compare, Some(wgpu::CompareFunction::LessEqual));
        assert_eq!(depth.depth_write_enabled, Some(false));
        let state = DrawState {
            depth_test: false,
            depth_write: true,
            ..Default::default()
        }
        .pipeline_state();
        let depth = depth_state(&state);
        assert_eq!(depth.depth_compare, Some(wgpu::CompareFunction::Always));
        assert_eq!(depth.depth_write_enabled, Some(true));
        assert_eq!(depth.format, wgpu::TextureFormat::Depth32Float);
    }

    #[test]
    fn vertex_formats_keep_their_sizes() {
        for (format, expected) in [
            (VertexFormat::Float32, wgpu::VertexFormat::Float32),
            (VertexFormat::Float32x2, wgpu::VertexFormat::Float32x2),
            (VertexFormat::Float32x3, wgpu::VertexFormat::Float32x3),
            (VertexFormat::Float32x4, wgpu::VertexFormat::Float32x4),
            (VertexFormat::Unorm8x4, wgpu::VertexFormat::Unorm8x4),
        ] {
            let mapped = vertex_format(format);
            assert_eq!(mapped, expected);
            assert_eq!(mapped.size(), u64::from(format.byte_size()));
        }
    }

    #[test]
    fn targets_and_textures_are_rgba8_srgb() {
        assert_eq!(
            target_format(TargetFormat::Rgba8Srgb),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
        assert!(COLOR_FORMAT.is_srgb());
    }

    #[test]
    fn align_up_rounds_to_the_alignment() {
        assert_eq!(align_up(0, 256), 0);
        assert_eq!(align_up(1, 256), 256);
        assert_eq!(align_up(256, 256), 256);
        assert_eq!(align_up(257, 256), 512);
        assert_eq!(align_up(5, 0), 5);
    }

    #[test]
    fn rectangles_default_to_the_target_and_clamp() {
        assert_eq!(effective_rect(None, 64, 32), PixelRect::new(0, 0, 64, 32));
        assert_eq!(
            effective_rect(Some(PixelRect::new(60, 10, 20, 100)), 64, 32),
            PixelRect::new(60, 10, 4, 22)
        );
        assert!(effective_rect(Some(PixelRect::new(100, 100, 5, 5)), 64, 64).is_empty());
    }

    #[test]
    fn surface_format_prefers_srgb() {
        use wgpu::TextureFormat::*;
        assert_eq!(
            choose_surface_format(&[Bgra8Unorm, Bgra8UnormSrgb]),
            Some(Bgra8UnormSrgb)
        );
        assert_eq!(choose_surface_format(&[Bgra8Unorm]), Some(Bgra8Unorm));
        assert_eq!(choose_surface_format(&[]), None);
    }

    #[test]
    fn stages_follow_reflection_and_default_to_both() {
        use engine_render::UniformSlot;
        use engine_render::shader::BindingKind;
        let binding = |vertex, fragment| ShaderBinding {
            slot: UniformSlot::new(0, 0),
            kind: BindingKind::Sampler,
            name: None,
            stages: StageUsage { vertex, fragment },
        };
        assert_eq!(
            shader_stages(&binding(true, false)),
            wgpu::ShaderStages::VERTEX
        );
        assert_eq!(
            shader_stages(&binding(false, true)),
            wgpu::ShaderStages::FRAGMENT
        );
        assert_eq!(
            shader_stages(&binding(true, true)),
            wgpu::ShaderStages::VERTEX_FRAGMENT
        );
        assert_eq!(
            shader_stages(&binding(false, false)),
            wgpu::ShaderStages::VERTEX_FRAGMENT
        );
    }

    #[test]
    fn backend_labels_name_the_wgpu_api() {
        assert_eq!(backend_label(wgpu::Backend::Vulkan), "wgpu/vulkan");
        assert_eq!(backend_label(wgpu::Backend::Dx12), "wgpu/dx12");
        assert_eq!(backend_label(wgpu::Backend::Metal), "wgpu/metal");
        assert_eq!(backend_label(wgpu::Backend::Gl), "wgpu/gl");
    }
}
