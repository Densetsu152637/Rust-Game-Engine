//! The reusable WGSL ray-query and shading libraries and shader composition.
//!
//! Later passes (RT shadows, AO, reflections, diffuse GI) compose their compute
//! or fragment shader with [`compose`]:
//!
//! ```
//! let source = engine_render_rt::wgsl::compose(
//!     0,
//!     "@compute @workgroup_size(1) fn main() { let hidden = rt_occluded(vec3(0.0), vec3(0.0, 1.0, 0.0), 0.0, 10.0, 0xffu); }",
//! );
//! assert!(source.starts_with("enable wgpu_ray_query;"));
//! ```
//!
//! and bind an [`crate::RtScene`]'s [`crate::RtScene::bind_group`] at that group.
//! The ray-query library defines `rt_occluded`, `rt_closest_hit` (returning
//! `RtHit`) and `rt_surface` (returning `RtSurface`); see
//! `src/shaders/ray_query.wgsl`. [`compose_kernel`] also appends the shading
//! library ([`SHADING_LIBRARY`], `src/shaders/shading.wgsl`): `rt_seed` /
//! `rt_rand`, `rt_cosine_sample`, `rt_cone_sample`, `rt_ggx_vndf`, `rt_sky`,
//! `rt_sun_diffuse` and the `RtMaterialRecord` / `RtLightingRecord` layouts.
//! docs/rendering-raytracing.md has the exact contract.

/// The library source with its scene group left as the `RT_GROUP` placeholder.
pub const LIBRARY_TEMPLATE: &str = include_str!("shaders/ray_query.wgsl");

/// Sampling and shading helpers shared by the kernels (no bindings; calls
/// `rt_occluded`, so it follows the ray-query library).
pub const SHADING_LIBRARY: &str = include_str!("shaders/shading.wgsl");

/// The directive every module using the library must start with.
pub const ENABLE_RAY_QUERY: &str = "enable wgpu_ray_query;";

/// The library with its scene bindings at `@group(group)`.
pub fn library(group: u32) -> String {
    LIBRARY_TEMPLATE.replace("@group(RT_GROUP)", &format!("@group({group})"))
}

/// `enable wgpu_ray_query;`, the library at `group`, then `body`.
pub fn compose(group: u32, body: &str) -> String {
    format!("{ENABLE_RAY_QUERY}\n\n{}\n{body}", library(group))
}

/// [`compose`] with the shading library between the ray-query library and
/// `body`.
pub fn compose_kernel(group: u32, body: &str) -> String {
    compose(group, &format!("{SHADING_LIBRARY}\n{body}"))
}

pub(crate) const PROBE_BODY: &str = include_str!("shaders/probe.wgsl");
pub(crate) const SHADOW_MASK_BODY: &str = include_str!("shaders/shadow_mask.wgsl");
pub(crate) const PATH_TRACE_BODY: &str = include_str!("shaders/path_trace.wgsl");
pub(crate) const AMBIENT_OCCLUSION_BODY: &str = include_str!("shaders/ambient_occlusion.wgsl");
pub(crate) const REFLECTIONS_BODY: &str = include_str!("shaders/reflections.wgsl");
pub(crate) const DIFFUSE_GI_BODY: &str = include_str!("shaders/diffuse_gi.wgsl");
const DENOISE_COMMON: &str = include_str!("shaders/denoise/common.wgsl");
const DENOISE_TEMPORAL: &str = include_str!("shaders/denoise/temporal.wgsl");
const DENOISE_VARIANCE: &str = include_str!("shaders/denoise/variance.wgsl");
const DENOISE_ATROUS: &str = include_str!("shaders/denoise/atrous.wgsl");
const DENOISE_RESOLVE: &str = include_str!("shaders/denoise/resolve.wgsl");

pub(crate) fn probe_source() -> String {
    compose(0, PROBE_BODY)
}

pub(crate) fn shadow_mask_source(storage_format: &str) -> String {
    compose_kernel(
        0,
        &SHADOW_MASK_BODY.replace("STORAGE_FORMAT", storage_format),
    )
}

pub(crate) fn path_trace_source() -> String {
    compose_kernel(0, PATH_TRACE_BODY)
}

pub(crate) fn ambient_occlusion_source(storage_format: &str) -> String {
    compose_kernel(
        0,
        &AMBIENT_OCCLUSION_BODY.replace("STORAGE_FORMAT", storage_format),
    )
}

pub(crate) fn reflections_source() -> String {
    compose_kernel(0, REFLECTIONS_BODY)
}

pub(crate) fn diffuse_gi_source() -> String {
    compose_kernel(0, DIFFUSE_GI_BODY)
}

/// Passes of the denoiser; their sources need no ray query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DenoisePass {
    Temporal,
    Variance,
    Atrous,
    Resolve,
}

impl DenoisePass {
    #[cfg(test)]
    pub const ALL: [Self; 4] = [Self::Temporal, Self::Variance, Self::Atrous, Self::Resolve];

    pub fn entry_point(self) -> &'static str {
        match self {
            Self::Temporal => "temporal",
            Self::Variance => "variance",
            Self::Atrous => "atrous",
            Self::Resolve => "resolve",
        }
    }

    pub fn source(self) -> String {
        let body = match self {
            Self::Temporal => DENOISE_TEMPORAL,
            Self::Variance => DENOISE_VARIANCE,
            Self::Atrous => DENOISE_ATROUS,
            Self::Resolve => DENOISE_RESOLVE,
        };
        format!("{DENOISE_COMMON}\n{body}")
    }
}

pub(crate) fn module(device: &wgpu::Device, label: &str, source: String) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(label: &str, source: &str) {
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|e| panic!("{label}: {}", e.emit_to_string(source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{label}: {e:?}"));
    }

    #[test]
    fn every_kernel_validates_with_naga() {
        // A shader error fails the ordinary `cargo test` on any machine.
        validate("probe", &probe_source());
        for format in ["r8unorm", "r16float", "r32float"] {
            validate(format, &shadow_mask_source(format));
            validate(format, &ambient_occlusion_source(format));
        }
        validate("path trace", &path_trace_source());
        validate("reflections", &reflections_source());
        validate("diffuse gi", &diffuse_gi_source());
        for pass in DenoisePass::ALL {
            let source = pass.source();
            assert!(!source.contains("ray_query"), "{pass:?} needs no ray query");
            validate(pass.entry_point(), &source);
        }
    }

    #[test]
    fn library_is_relocatable_to_any_group() {
        let source = library(3);
        assert!(!source.contains("RT_GROUP)"));
        assert_eq!(source.matches("@group(3)").count(), 6);
        validate(
            "group 3",
            &compose_kernel(
                3,
                "@compute @workgroup_size(1) fn main() {
                    let h = rt_closest_hit(vec3(0.0), vec3(0.0, 0.0, 1.0), 0.0, 1.0, 0xffu);
                    if h.hit { let s = rt_surface(h, vec3(0.0), vec3(0.0, 0.0, 1.0)); }
                    let o = rt_occluded(vec3(0.0), vec3(0.0, 1.0, 0.0), 0.0, 10.0, 1u);
                    rt_seed(vec2(0u), 1u, 0u, 0u);
                    let d = rt_cosine_sample(vec3(0.0, 1.0, 0.0));
                }",
            ),
        );
        assert!(!SHADING_LIBRARY.contains("@group"));
    }
}
