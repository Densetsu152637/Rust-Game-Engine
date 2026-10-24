//! WGSL validation and reflection shared by every backend.
//!
//! WGSL is the authoring language. This module parses and validates a shader
//! once, and reflects the interface the device checks draws against: vertex
//! inputs, resource bindings and uniform sizes. Backends then only translate.

use crate::backend::DeviceLimits;
use crate::command::{DrawUniform, SAMPLER_SLOT, TEXTURE_SLOT, UniformSlot};
use crate::error::{RenderError, Result};
use crate::resources::{ShaderLanguage, ShaderResource, VertexLayout};

macro_rules! bail_shader {
    ($($arg:tt)*) => {
        return Err(RenderError::Shader(format!($($arg)*)))
    };
}

/// Maximum accepted WGSL source size.
pub const MAX_WGSL_BYTES: usize = 16 * 1024 * 1024;

/// Parse and validate a standalone WGSL shader without opening a device.
/// All naga capabilities are admitted here; device feature and limit
/// requirements are checked when the backend creates the pipeline. Diagnostics
/// retain naga source spans. Sources must contain an entry point and fit within
/// 16 MiB.
pub fn validate_wgsl(source: &str) -> Result<()> {
    parse_and_validate(source).map(|_| ())
}

fn parse_and_validate(source: &str) -> Result<(naga::Module, naga::valid::ModuleInfo)> {
    if source.len() > MAX_WGSL_BYTES {
        bail_shader!("WGSL source exceeds 16 MiB");
    }
    let module = naga::front::wgsl::parse_str(source).map_err(|error| {
        RenderError::Shader(format!(
            "WGSL parse failed:\n{}",
            error.emit_to_string(source)
        ))
    })?;
    if module.entry_points.is_empty() {
        bail_shader!("WGSL shader has no entry point");
    }
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|error| {
        RenderError::Shader(format!(
            "WGSL validation failed:\n{}",
            error.emit_to_string(source)
        ))
    })?;
    Ok((module, info))
}

/// Scalar class of a vertex input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarClass {
    Float,
    Sint,
    Uint,
}

/// One `@location(n)` vertex-stage input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexInput {
    pub location: u32,
    pub scalar: ScalarClass,
    pub components: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    /// `var<uniform>`; `size` is the byte size the draw must supply.
    UniformBuffer { size: u32 },
    /// `texture_2d<f32>`.
    SampledTexture2d,
    /// Frame directional shadow map, at group 3 binding 0.
    ShadowTexture2d,
    /// Non-comparison `sampler`.
    Sampler,
}

/// Pipeline stages that reference a binding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StageUsage {
    pub vertex: bool,
    pub fragment: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderBinding {
    pub slot: UniformSlot,
    pub kind: BindingKind,
    pub name: Option<String>,
    pub stages: StageUsage,
}

/// Reflected interface of a vertex+fragment WGSL program. Only resources the
/// two selected entry points actually reference are listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderInterface {
    pub vertex_entry: String,
    pub fragment_entry: String,
    pub vertex_inputs: Vec<VertexInput>,
    /// Number of `@location` color outputs of the fragment stage.
    pub fragment_outputs: u32,
    /// Sorted by `(group, binding)`.
    pub bindings: Vec<ShaderBinding>,
}

impl ShaderInterface {
    /// Parse, validate and reflect a [`ShaderResource`].
    pub fn reflect(resource: &ShaderResource) -> Result<Self> {
        let ShaderLanguage::Wgsl = resource.source.language;
        let source = &resource.source;
        let (module, info) = parse_and_validate(&source.code)?;
        let find = |name: &str, stage: naga::ShaderStage| {
            module
                .entry_points
                .iter()
                .position(|e| e.name == name && e.stage == stage)
        };
        let Some(vs) = find(&source.vertex_entry, naga::ShaderStage::Vertex) else {
            bail_shader!(
                "shader {:?} has no vertex entry point {:?}",
                resource.name,
                source.vertex_entry
            );
        };
        let Some(fs) = find(&source.fragment_entry, naga::ShaderStage::Fragment) else {
            bail_shader!(
                "shader {:?} has no fragment entry point {:?}",
                resource.name,
                source.fragment_entry
            );
        };

        let mut vertex_inputs = Vec::new();
        for argument in &module.entry_points[vs].function.arguments {
            collect_inputs(
                &module,
                argument.binding.as_ref(),
                argument.ty,
                &mut vertex_inputs,
            )?;
        }
        vertex_inputs.sort_by_key(|input| input.location);

        let mut fragment_outputs = 0;
        if let Some(result) = &module.entry_points[fs].function.result {
            let mut outputs = Vec::new();
            collect_inputs(&module, result.binding.as_ref(), result.ty, &mut outputs)?;
            fragment_outputs = outputs.len() as u32;
        }

        let mut bindings = Vec::new();
        let ctx = module.to_ctx();
        for (handle, variable) in module.global_variables.iter() {
            let Some(binding) = variable.binding else {
                continue;
            };
            let used = |entry: usize| !info.get_entry_point(entry)[handle].is_empty();
            let stages = StageUsage {
                vertex: used(vs),
                fragment: used(fs),
            };
            if !stages.vertex && !stages.fragment {
                continue;
            }
            let slot = UniformSlot::new(binding.group, binding.binding);
            let kind = match (&variable.space, &module.types[variable.ty].inner) {
                (naga::AddressSpace::Uniform, inner) => BindingKind::UniformBuffer {
                    size: inner.size(ctx),
                },
                (
                    naga::AddressSpace::Handle,
                    naga::TypeInner::Image {
                        dim: naga::ImageDimension::D2,
                        arrayed: false,
                        class:
                            naga::ImageClass::Sampled {
                                kind: naga::ScalarKind::Float,
                                multi: false,
                            },
                    },
                ) => BindingKind::SampledTexture2d,
                (
                    naga::AddressSpace::Handle,
                    naga::TypeInner::Image {
                        dim: naga::ImageDimension::D2,
                        arrayed: false,
                        class: naga::ImageClass::Depth { multi: false },
                    },
                ) => BindingKind::ShadowTexture2d,
                (naga::AddressSpace::Handle, naga::TypeInner::Sampler { comparison: false }) => {
                    BindingKind::Sampler
                }
                (space, _) => {
                    return Err(RenderError::Unsupported {
                        feature: format!(
                            "binding @group({}) @binding({})",
                            binding.group, binding.binding
                        ),
                        reason: format!(
                            "shader {:?} uses a resource ({space:?}) other than a uniform \
                             buffer, texture_2d<f32> or filtering sampler",
                            resource.name
                        ),
                    });
                }
            };
            bindings.push(ShaderBinding {
                slot,
                kind,
                name: variable.name.clone(),
                stages,
            });
        }
        bindings.sort_by_key(|b| b.slot);
        let interface = Self {
            vertex_entry: source.vertex_entry.clone(),
            fragment_entry: source.fragment_entry.clone(),
            vertex_inputs,
            fragment_outputs,
            bindings,
        };
        interface.check_conventions(&resource.name)?;
        Ok(interface)
    }

    fn check_conventions(&self, name: &str) -> Result<()> {
        for binding in &self.bindings {
            let by_convention = match binding.kind {
                BindingKind::SampledTexture2d => {
                    binding.slot == TEXTURE_SLOT
                        || (binding.slot.group == 1 && matches!(binding.slot.binding, 2 | 3))
                }
                BindingKind::Sampler => binding.slot == SAMPLER_SLOT,
                BindingKind::ShadowTexture2d => binding.slot == crate::SHADOW_SLOT,
                BindingKind::UniformBuffer { .. } => {
                    binding.slot != TEXTURE_SLOT && binding.slot != SAMPLER_SLOT
                }
            };
            if !by_convention {
                bail_shader!(
                    "shader {name:?}: @group({}) @binding({}) is reserved for the conventional \
                     texture (1,0), sampler (1,1), data textures (1,2)/(1,3) or shadow (3,0) bindings",
                    binding.slot.group,
                    binding.slot.binding
                );
            }
        }
        Ok(())
    }

    /// Check the reflected bindings against device limits: bind group indices,
    /// binding numbers, bindings per group and uniform sizes.
    pub fn check_limits(&self, limits: &DeviceLimits) -> Result<()> {
        let exceeded = |limit: &'static str, requested: u64, max: u32| {
            Err(RenderError::LimitExceeded {
                limit,
                requested,
                max: u64::from(max),
            })
        };
        for binding in &self.bindings {
            let slot = binding.slot;
            if slot.group >= limits.max_bind_groups {
                return exceeded(
                    "max_bind_groups (group index + 1)",
                    u64::from(slot.group) + 1,
                    limits.max_bind_groups,
                );
            }
            if slot.binding >= limits.max_bindings_per_bind_group {
                return exceeded(
                    "max_bindings_per_bind_group (binding index + 1)",
                    u64::from(slot.binding) + 1,
                    limits.max_bindings_per_bind_group,
                );
            }
            if let BindingKind::UniformBuffer { size } = binding.kind
                && size > limits.max_uniform_buffer_binding_size
            {
                return exceeded(
                    "max_uniform_buffer_binding_size",
                    u64::from(size),
                    limits.max_uniform_buffer_binding_size,
                );
            }
        }
        Ok(())
    }

    /// Whether the program samples a draw's optional texture.
    pub fn uses_texture(&self) -> bool {
        self.bindings
            .iter()
            .any(|b| b.kind == BindingKind::SampledTexture2d)
    }

    pub fn uses_sampler(&self) -> bool {
        self.bindings.iter().any(|b| b.kind == BindingKind::Sampler)
    }

    /// Uniform buffer bindings a draw must supply, in slot order.
    pub fn uniform_bindings(&self) -> impl Iterator<Item = (&ShaderBinding, u32)> {
        self.bindings.iter().filter_map(|b| match b.kind {
            BindingKind::UniformBuffer { size } => Some((b, size)),
            _ => None,
        })
    }

    /// A mesh layout satisfies the shader when every vertex input location has an
    /// attribute with a float format of at least as many components.
    pub fn check_vertex_layout(&self, layout: &VertexLayout) -> Result<()> {
        for input in &self.vertex_inputs {
            let Some(attribute) = layout.attribute(input.location) else {
                return Err(RenderError::InvalidDraw(format!(
                    "mesh has no vertex attribute at location {} required by the shader",
                    input.location
                )));
            };
            if input.scalar != ScalarClass::Float {
                return Err(RenderError::InvalidDraw(format!(
                    "shader input at location {} is {:?}; vertex formats supply floats",
                    input.location, input.scalar
                )));
            }
            if attribute.format.components() < input.components {
                return Err(RenderError::InvalidDraw(format!(
                    "vertex attribute at location {} supplies {} components but the shader reads {}",
                    input.location,
                    attribute.format.components(),
                    input.components
                )));
            }
        }
        Ok(())
    }

    /// Every declared uniform buffer must be supplied exactly once with exactly
    /// the declared byte size, and nothing else may be supplied.
    pub fn check_uniforms(&self, uniforms: &[DrawUniform]) -> Result<()> {
        for (index, uniform) in uniforms.iter().enumerate() {
            if uniforms[..index].iter().any(|u| u.slot == uniform.slot) {
                return Err(RenderError::InvalidDraw(format!(
                    "uniform slot (group {}, binding {}) supplied twice",
                    uniform.slot.group, uniform.slot.binding
                )));
            }
            if !self
                .uniform_bindings()
                .any(|(binding, _)| binding.slot == uniform.slot)
            {
                return Err(RenderError::InvalidDraw(format!(
                    "uniform {:?} targets (group {}, binding {}), which the shader does not declare",
                    uniform.name, uniform.slot.group, uniform.slot.binding
                )));
            }
        }
        for (binding, size) in self.uniform_bindings() {
            match uniforms.iter().find(|u| u.slot == binding.slot) {
                None => {
                    return Err(RenderError::InvalidDraw(format!(
                        "missing uniform {:?} at (group {}, binding {})",
                        binding.name.as_deref().unwrap_or("?"),
                        binding.slot.group,
                        binding.slot.binding
                    )));
                }
                Some(uniform) if uniform.bytes.len() != size as usize => {
                    return Err(RenderError::InvalidDraw(format!(
                        "uniform {:?} is {} bytes but the shader declares {size}",
                        uniform.name,
                        uniform.bytes.len()
                    )));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

fn collect_inputs(
    module: &naga::Module,
    binding: Option<&naga::Binding>,
    ty: naga::Handle<naga::Type>,
    out: &mut Vec<VertexInput>,
) -> Result<()> {
    match binding {
        Some(naga::Binding::Location { location, .. }) => {
            let (scalar, components) = match module.types[ty].inner {
                naga::TypeInner::Scalar(s) => (s, 1),
                naga::TypeInner::Vector { size, scalar } => (scalar, size as u32),
                ref other => {
                    bail_shader!("unsupported @location type {other:?}")
                }
            };
            let class = match (scalar.kind, scalar.width) {
                (naga::ScalarKind::Float, 4) => ScalarClass::Float,
                (naga::ScalarKind::Sint, 4) => ScalarClass::Sint,
                (naga::ScalarKind::Uint, 4) => ScalarClass::Uint,
                _ => bail_shader!("@location(s) must be 32-bit float or integer scalars/vectors"),
            };
            out.push(VertexInput {
                location: *location,
                scalar: class,
                components,
            });
        }
        Some(naga::Binding::BuiltIn(_)) => {}
        None => {
            if let naga::TypeInner::Struct { members, .. } = &module.types[ty].inner {
                for member in members {
                    collect_inputs(module, member.binding.as_ref(), member.ty, out)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod reflection_tests {
    use super::*;
    use crate::resources::{ShaderSource, VertexAttribute, VertexFormat};

    pub(crate) const TEXTURED: &str = "
struct Camera { view_projection: mat4x4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}
@vertex fn vs_main(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>,
                  @location(2) color: vec4<f32>) -> VertexOutput {
    var output: VertexOutput;
    output.position = camera.view_projection * vec4<f32>(position, 1.0);
    output.uv = uv;
    output.color = color;
    return output;
}
@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(image, image_sampler, input.uv) * input.color;
}
";

    fn reflect(code: &str) -> Result<ShaderInterface> {
        ShaderInterface::reflect(&ShaderResource::new("test", ShaderSource::wgsl(code)))
    }

    fn attr(location: u32, format: VertexFormat, offset: u32) -> VertexAttribute {
        VertexAttribute {
            location,
            format,
            offset,
        }
    }

    #[test]
    fn textured_shader_reflects_inputs_bindings_and_uniform_size() {
        let interface = reflect(TEXTURED).unwrap();
        assert_eq!(
            interface.vertex_inputs,
            [
                VertexInput {
                    location: 0,
                    scalar: ScalarClass::Float,
                    components: 3
                },
                VertexInput {
                    location: 1,
                    scalar: ScalarClass::Float,
                    components: 2
                },
                VertexInput {
                    location: 2,
                    scalar: ScalarClass::Float,
                    components: 4
                },
            ]
        );
        assert_eq!(interface.fragment_outputs, 1);
        let kinds: Vec<_> = interface
            .bindings
            .iter()
            .map(|b| (b.slot, b.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                (
                    UniformSlot::new(0, 0),
                    BindingKind::UniformBuffer { size: 64 }
                ),
                (TEXTURE_SLOT, BindingKind::SampledTexture2d),
                (SAMPLER_SLOT, BindingKind::Sampler),
            ]
        );
        assert!(interface.bindings[0].stages.vertex && !interface.bindings[0].stages.fragment);
        assert!(interface.bindings[1].stages.fragment && !interface.bindings[1].stages.vertex);
        assert!(interface.uses_texture() && interface.uses_sampler());
    }

    #[test]
    fn unused_bindings_are_not_reported() {
        let interface = reflect(
            "@group(0) @binding(0) var<uniform> unused: vec4<f32>;
             @vertex fn vs_main() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }
             @fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }",
        )
        .unwrap();
        assert!(interface.bindings.is_empty());
    }

    #[test]
    fn missing_or_wrong_stage_entry_points_are_named_in_the_error() {
        let error = reflect(
            "@vertex fn vs_main() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }",
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("no fragment entry point \"fs_main\""),
            "{error}"
        );
        let swapped = ShaderResource::new(
            "swap",
            ShaderSource::wgsl(TEXTURED).with_entry_points("fs_main", "vs_main"),
        );
        assert!(ShaderInterface::reflect(&swapped).is_err());
    }

    #[test]
    fn compute_storage_and_comparison_resources_are_unsupported() {
        let storage = reflect(
            "@group(0) @binding(0) var<storage, read> data: array<f32>;
             @vertex fn vs_main() -> @builtin(position) vec4<f32> { return vec4<f32>(data[0]); }
             @fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }",
        );
        assert!(
            matches!(storage, Err(RenderError::Unsupported { .. })),
            "{storage:?}"
        );
    }

    #[test]
    fn texture_and_sampler_must_use_the_conventional_slots() {
        let error = reflect(
            "@group(2) @binding(0) var t: texture_2d<f32>;
             @group(2) @binding(1) var s: sampler;
             @vertex fn vs_main() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }
             @fragment fn fs_main() -> @location(0) vec4<f32> { return textureSample(t, s, vec2<f32>(0.0)); }",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("reserved"), "{error}");
    }

    #[test]
    fn vertex_layout_must_cover_every_shader_input_with_enough_components() {
        let interface = reflect(TEXTURED).unwrap();
        let good = VertexLayout::new(
            36,
            [
                attr(0, VertexFormat::Float32x3, 0),
                attr(1, VertexFormat::Float32x2, 12),
                attr(2, VertexFormat::Float32x4, 20),
            ],
        );
        interface.check_vertex_layout(&good).unwrap();
        let wider = VertexLayout::new(
            40,
            [
                attr(0, VertexFormat::Float32x4, 0),
                attr(1, VertexFormat::Float32x2, 16),
                attr(2, VertexFormat::Unorm8x4, 24),
                attr(9, VertexFormat::Float32, 28),
            ],
        );
        interface.check_vertex_layout(&wider).unwrap();
        let missing = VertexLayout::new(
            20,
            [
                attr(0, VertexFormat::Float32x3, 0),
                attr(1, VertexFormat::Float32x2, 12),
            ],
        );
        assert!(
            interface
                .check_vertex_layout(&missing)
                .unwrap_err()
                .to_string()
                .contains("location 2")
        );
        let narrow = VertexLayout::new(
            28,
            [
                attr(0, VertexFormat::Float32x2, 0),
                attr(1, VertexFormat::Float32x2, 8),
                attr(2, VertexFormat::Float32x4, 16),
            ],
        );
        assert!(
            interface
                .check_vertex_layout(&narrow)
                .unwrap_err()
                .to_string()
                .contains("supplies 2")
        );
    }

    #[test]
    fn integer_vertex_inputs_are_rejected_against_float_formats() {
        let interface = reflect(
            "@vertex fn vs_main(@location(0) id: u32) -> @builtin(position) vec4<f32> { return vec4<f32>(f32(id)); }
             @fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }",
        )
        .unwrap();
        let layout = VertexLayout::new(4, [attr(0, VertexFormat::Float32, 0)]);
        assert!(
            interface
                .check_vertex_layout(&layout)
                .unwrap_err()
                .to_string()
                .contains("Uint")
        );
    }

    #[test]
    fn uniforms_must_match_declared_slots_and_exact_sizes() {
        let interface = reflect(TEXTURED).unwrap();
        let camera = |bytes: usize| {
            DrawUniform::from_bytes("camera", UniformSlot::new(0, 0), vec![0; bytes])
        };
        interface.check_uniforms(&[camera(64)]).unwrap();
        assert!(
            interface
                .check_uniforms(&[])
                .unwrap_err()
                .to_string()
                .contains("missing uniform")
        );
        assert!(
            interface
                .check_uniforms(&[camera(60)])
                .unwrap_err()
                .to_string()
                .contains("64")
        );
        assert!(
            interface
                .check_uniforms(&[camera(64), camera(64)])
                .unwrap_err()
                .to_string()
                .contains("twice")
        );
        let stray = DrawUniform::from_bytes("stray", UniformSlot::new(3, 3), vec![0; 4]);
        assert!(
            interface
                .check_uniforms(&[camera(64), stray])
                .unwrap_err()
                .to_string()
                .contains("does not declare")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_real_shader_and_reports_parse_and_semantic_errors_without_gpu() {
        validate_wgsl(super::reflection_tests::TEXTURED).unwrap();
        assert!(
            validate_wgsl("@vertex fn broken(")
                .unwrap_err()
                .to_string()
                .contains("WGSL parse failed")
        );
        assert!(
            validate_wgsl("@vertex fn main() -> @builtin(position) f32 { return 1.0; }")
                .unwrap_err()
                .to_string()
                .contains("WGSL validation failed")
        );
        assert!(validate_wgsl("").is_err());
    }
}

#[cfg(test)]
mod wgsl_contract_tests {
    use super::*;

    const VERTEX_ONLY: &str =
        "@vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }";

    macro_rules! accepts {
        ($($name:ident: $source:expr;)*) => {$(
            #[test]
            fn $name() {
                validate_wgsl($source).unwrap();
            }
        )*};
    }

    accepts! {
        accepts_minimal_vertex_shader: VERTEX_ONLY;
        accepts_fragment_only_shader: "@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }";
        accepts_compute_shader_with_workgroup_size: "@compute @workgroup_size(8, 8, 1) fn main() {}";
        accepts_compute_shader_with_storage_buffer: "@group(0) @binding(0) var<storage, read_write> data: array<u32>;\n@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) { data[id.x] = id.x * 2u; }";
        accepts_uniform_buffer_and_texture_bindings: "struct U { m: mat4x4<f32> }\n@group(0) @binding(0) var<uniform> u: U;\n@group(1) @binding(0) var t: texture_2d<f32>;\n@group(1) @binding(1) var s: sampler;\n@fragment fn fs(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> { return textureSample(t, s, uv) * u.m[0]; }";
        accepts_multiple_entry_points_in_one_module: "@vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }\n@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }";
        accepts_comments_and_unicode_in_comments: "// shader \u{3b1}\u{3b2}\u{3b3}\n/* block */ @vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }";
        accepts_helper_functions_and_constants: "const SCALE: f32 = 2.0;\nfn twice(x: f32) -> f32 { return x * SCALE; }\n@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(twice(0.25)); }";
        accepts_loops_and_branches: "@fragment fn fs() -> @location(0) vec4<f32> { var acc = 0.0; for (var i = 0; i < 4; i++) { if (i == 2) { continue; } acc += 0.25; } return vec4<f32>(acc); }";
    }

    macro_rules! rejects {
        ($($name:ident: $source:expr => $message:expr;)*) => {$(
            #[test]
            fn $name() {
                let error = validate_wgsl($source).unwrap_err().to_string();
                assert!(error.contains($message), "expected {:?} in:\n{error}", $message);
            }
        )*};
    }

    rejects! {
        rejects_empty_source: "" => "no entry point";
        rejects_whitespace_only_source: "  \n\t  \n" => "no entry point";
        rejects_comment_only_source: "// nothing here" => "no entry point";
        rejects_module_without_entry_point: "fn helper() -> f32 { return 1.0; }" => "no entry point";
        rejects_unterminated_parameter_list: "@vertex fn broken(" => "WGSL parse failed";
        rejects_missing_semicolon: "@fragment fn fs() -> @location(0) vec4<f32> { let x = 1.0 return vec4<f32>(x); }" => "WGSL parse failed";
        rejects_unbalanced_braces: "@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0);" => "WGSL parse failed";
        rejects_unknown_type: "@fragment fn fs() -> @location(0) vec4<nope> { return vec4<f32>(1.0); }" => "WGSL parse failed";
        rejects_undefined_identifier: "@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(missing); }" => "missing";
        rejects_unknown_attribute: "@bogus fn f() {}\n@vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }" => "WGSL parse failed";
        rejects_duplicate_function_names: "fn f() {}\nfn f() {}\n@compute @workgroup_size(1) fn main() { f(); }" => "WGSL parse failed";
        rejects_compute_entry_without_workgroup_size: "@compute fn main() {}" => "WGSL parse failed";
        rejects_float_integer_mismatch: "@fragment fn fs() -> @location(0) vec4<f32> { let x: i32 = 1.5; return vec4<f32>(0.0); }" => "WGSL parse failed";
        rejects_vertex_entry_returning_non_position: "@vertex fn main() -> @builtin(position) f32 { return 1.0; }" => "WGSL validation failed";
        rejects_fragment_output_without_binding: "@fragment fn fs() -> vec4<f32> { return vec4<f32>(1.0); }" => "WGSL validation failed";
        rejects_vertex_without_position_output: "@vertex fn vs() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }" => "WGSL validation failed";
        rejects_two_bindings_sharing_a_slot: "@group(0) @binding(0) var<uniform> a: vec4<f32>;\n@group(0) @binding(0) var<uniform> b: vec4<f32>;\n@fragment fn fs() -> @location(0) vec4<f32> { return a + b; }" => "WGSL validation failed";
        rejects_unsized_array_in_uniform_space: "@group(0) @binding(0) var<uniform> a: array<f32>;\n@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(a[0]); }" => "WGSL";
    }

    #[test]
    fn parse_errors_carry_line_and_column_spans() {
        let source = "@vertex\nfn broken() -> @builtin(position) vec4<f32> {\n    return vec4<f32>(oops);\n}\n";
        let error = validate_wgsl(source).unwrap_err().to_string();
        assert!(error.contains("oops"), "{error}");
        assert!(error.contains(":3:"), "line number missing:\n{error}");
    }

    #[test]
    fn validation_errors_name_the_offending_entry_point() {
        let source = "@vertex
fn main() -> @builtin(position) f32 {
    return 1.0;
}
";
        let error = validate_wgsl(source).unwrap_err().to_string();
        assert!(error.contains("WGSL validation failed"), "{error}");
        assert!(error.contains("Entry point main"), "{error}");
    }

    #[test]
    fn size_limit_is_sixteen_mebibytes_inclusive() {
        let limit = 16 * 1024 * 1024;
        let at_limit = " ".repeat(limit);
        let error = validate_wgsl(&at_limit).unwrap_err().to_string();
        assert!(
            !error.contains("16 MiB"),
            "limit itself must not trip the size check"
        );
        let over = " ".repeat(limit + 1);
        let error = validate_wgsl(&over).unwrap_err().to_string();
        assert!(error.contains("exceeds 16 MiB"), "{error}");
    }

    #[test]
    fn oversized_source_is_rejected_before_parsing() {
        let mut source = String::from(VERTEX_ONLY);
        source.push_str(&" ".repeat(16 * 1024 * 1024));
        assert!(
            validate_wgsl(&source)
                .unwrap_err()
                .to_string()
                .contains("exceeds 16 MiB")
        );
    }

    #[test]
    fn validation_is_deterministic() {
        let first = validate_wgsl("@vertex fn x(").unwrap_err().to_string();
        let second = validate_wgsl("@vertex fn x(").unwrap_err().to_string();
        assert_eq!(first, second);
    }
}
