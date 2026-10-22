//! Shader translation, descriptor/pipeline layouts, render passes and the
//! pipeline cache.
//!
//! WGSL becomes SPIR-V through naga (one module per stage). naga maps
//! `@group(g) @binding(b)` straight to descriptor set `g`, binding `b`. The
//! Y flip is done by the viewport (see `translate.rs`), so naga's own
//! coordinate-space adjustment is switched off.

use crate::error::VkCheck;
use crate::translate;
use ash::vk;
use engine_render::shader::{BindingKind, StageUsage};
use engine_render::{
    PipelineState, RenderError, Result, ShaderInterface, ShaderLanguage, ShaderResource,
    UniformSlot, VertexLayout,
};
use std::collections::HashMap;
use std::ffi::CString;

// ------------------------------------------------------------------- SPIR-V

/// SPIR-V for the two stages of a program.
pub(crate) struct SpirvModules {
    pub vertex: Vec<u32>,
    pub fragment: Vec<u32>,
}

/// Target SPIR-V 1.3: the version Vulkan 1.1 guarantees.
const SPIRV_VERSION: (u8, u8) = (1, 3);

fn spirv_options(flags: naga::back::spv::WriterFlags) -> naga::back::spv::Options<'static> {
    naga::back::spv::Options {
        lang_version: SPIRV_VERSION,
        flags,
        // Without an explicit map naga uses (group, binding) as (set, binding).
        fake_missing_bindings: true,
        ..Default::default()
    }
}

/// Writer flags used for real pipelines: no `ADJUST_COORDINATE_SPACE` (the
/// viewport flips Y) and no debug info.
fn default_writer_flags() -> naga::back::spv::WriterFlags {
    naga::back::spv::WriterFlags::LABEL_VARYINGS | naga::back::spv::WriterFlags::CLAMP_FRAG_DEPTH
}

fn emit(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
    flags: naga::back::spv::WriterFlags,
    stage: naga::ShaderStage,
    entry: &str,
) -> Result<Vec<u32>> {
    naga::back::spv::write_vec(
        module,
        info,
        &spirv_options(flags),
        Some(&naga::back::spv::PipelineOptions {
            shader_stage: stage,
            entry_point: entry.to_string(),
        }),
    )
    .map_err(|error| {
        RenderError::Shader(format!(
            "SPIR-V generation for entry point {entry:?} failed: {error}"
        ))
    })
}

fn translate_with_flags(
    code: &str,
    vertex_entry: &str,
    fragment_entry: &str,
    flags: naga::back::spv::WriterFlags,
) -> Result<SpirvModules> {
    let module = naga::front::wgsl::parse_str(code).map_err(|error| {
        RenderError::Shader(format!(
            "WGSL parse failed:\n{}",
            error.emit_to_string(code)
        ))
    })?;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .map_err(|error| {
        RenderError::Shader(format!(
            "WGSL validation failed:\n{}",
            error.emit_to_string(code)
        ))
    })?;
    Ok(SpirvModules {
        vertex: emit(
            &module,
            &info,
            flags,
            naga::ShaderStage::Vertex,
            vertex_entry,
        )?,
        fragment: emit(
            &module,
            &info,
            flags,
            naga::ShaderStage::Fragment,
            fragment_entry,
        )?,
    })
}

/// Translate a vertex+fragment WGSL program to SPIR-V.
pub(crate) fn compile_wgsl(
    code: &str,
    vertex_entry: &str,
    fragment_entry: &str,
) -> Result<SpirvModules> {
    translate_with_flags(code, vertex_entry, fragment_entry, default_writer_flags())
}

// ------------------------------------------------------------ layout planning

/// One binding of a descriptor set, as plain data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BindingPlan {
    pub binding: u32,
    pub kind: BindingKind,
    pub stages: StageUsage,
}

/// One descriptor set (shader `@group`). Groups without bindings are present
/// but empty so set indices line up with group numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GroupPlan {
    pub bindings: Vec<BindingPlan>,
}

impl GroupPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Uniform buffer bindings in binding order; this is the order of the
    /// dynamic offsets bound with the set.
    pub(crate) fn dynamic_bindings(&self) -> impl Iterator<Item = &BindingPlan> {
        self.bindings
            .iter()
            .filter(|b| matches!(b.kind, BindingKind::UniformBuffer { .. }))
    }

    pub(crate) fn has_texture(&self) -> bool {
        self.bindings
            .iter()
            .any(|b| b.kind == BindingKind::SampledTexture2d)
    }
}

/// Descriptor layout of a program, derived from its reflected interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LayoutPlan {
    pub groups: Vec<GroupPlan>,
}

impl LayoutPlan {
    pub(crate) fn from_interface(interface: &ShaderInterface) -> Self {
        let group_count = interface
            .bindings
            .iter()
            .map(|b| b.slot.group as usize + 1)
            .max()
            .unwrap_or(0);
        let mut groups = vec![GroupPlan { bindings: vec![] }; group_count];
        for binding in &interface.bindings {
            groups[binding.slot.group as usize]
                .bindings
                .push(BindingPlan {
                    binding: binding.slot.binding,
                    kind: binding.kind,
                    stages: binding.stages,
                });
        }
        for group in &mut groups {
            group.bindings.sort_by_key(|b| b.binding);
        }
        Self { groups }
    }

    pub(crate) fn uniform_count(&self) -> usize {
        self.groups
            .iter()
            .map(|g| g.dynamic_bindings().count())
            .sum()
    }

    /// The `(group, binding)` slot of every dynamic uniform in set `group`, in
    /// dynamic-offset order.
    pub(crate) fn dynamic_slots(&self, group: usize) -> Vec<UniformSlot> {
        self.groups[group]
            .dynamic_bindings()
            .map(|b| UniformSlot::new(group as u32, b.binding))
            .collect()
    }
}

// ------------------------------------------------------------------ programs

/// A created shader program: modules, descriptor set layouts, pipeline layout.
pub(crate) struct Program {
    pub interface: ShaderInterface,
    pub plan: LayoutPlan,
    pub set_layouts: Vec<vk::DescriptorSetLayout>,
    pub pipeline_layout: vk::PipelineLayout,
    vertex_module: vk::ShaderModule,
    fragment_module: vk::ShaderModule,
    vertex_entry: CString,
    fragment_entry: CString,
}

impl Program {
    pub(crate) fn create(
        device: &ash::Device,
        limits: &vk::PhysicalDeviceLimits,
        resource: &ShaderResource,
    ) -> Result<Self> {
        match resource.source.language {
            ShaderLanguage::Wgsl => {}
            #[allow(unreachable_patterns)]
            other => {
                return Err(RenderError::Unsupported {
                    feature: format!("shader language {other:?}"),
                    reason: "the Vulkan backend translates WGSL only".into(),
                });
            }
        }
        let interface = ShaderInterface::reflect(resource)?;
        let plan = LayoutPlan::from_interface(&interface);
        check_limits(&interface, &plan, limits, &resource.name)?;
        let spirv = compile_wgsl(
            &resource.source.code,
            &resource.source.vertex_entry,
            &resource.source.fragment_entry,
        )?;
        let entry = |name: &str| {
            CString::new(name)
                .map_err(|_| RenderError::Shader(format!("entry point {name:?} contains a NUL")))
        };
        let mut program = Self {
            vertex_entry: entry(&resource.source.vertex_entry)?,
            fragment_entry: entry(&resource.source.fragment_entry)?,
            interface,
            plan,
            set_layouts: Vec::new(),
            pipeline_layout: vk::PipelineLayout::null(),
            vertex_module: vk::ShaderModule::null(),
            fragment_module: vk::ShaderModule::null(),
        };
        match program.build(device, &spirv) {
            Ok(()) => Ok(program),
            Err(error) => {
                program.destroy(device);
                Err(error)
            }
        }
    }

    fn build(&mut self, device: &ash::Device, spirv: &SpirvModules) -> Result<()> {
        self.vertex_module = create_module(device, &spirv.vertex)?;
        self.fragment_module = create_module(device, &spirv.fragment)?;
        for group in &self.plan.groups {
            let layout = create_set_layout(device, group)?;
            self.set_layouts.push(layout);
        }
        let info = vk::PipelineLayoutCreateInfo::default().set_layouts(&self.set_layouts);
        // SAFETY: `info` references set layouts created from this device.
        self.pipeline_layout =
            unsafe { device.create_pipeline_layout(&info, None) }.vk("vkCreatePipelineLayout")?;
        Ok(())
    }

    /// Destroy every Vulkan object. The GPU must no longer use the program.
    pub(crate) fn destroy(&mut self, device: &ash::Device) {
        // SAFETY: the caller guarantees no pending work references these objects;
        // destroying null handles is a no-op.
        unsafe {
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            for layout in self.set_layouts.drain(..) {
                device.destroy_descriptor_set_layout(layout, None);
            }
            device.destroy_shader_module(self.vertex_module, None);
            device.destroy_shader_module(self.fragment_module, None);
        }
        self.pipeline_layout = vk::PipelineLayout::null();
        self.vertex_module = vk::ShaderModule::null();
        self.fragment_module = vk::ShaderModule::null();
    }
}

fn check_limits(
    interface: &ShaderInterface,
    plan: &LayoutPlan,
    limits: &vk::PhysicalDeviceLimits,
    name: &str,
) -> Result<()> {
    let unsupported = |reason: String| RenderError::Unsupported {
        feature: format!("shader {name:?}"),
        reason,
    };
    if plan.groups.len() > limits.max_bound_descriptor_sets as usize {
        return Err(unsupported(format!(
            "uses descriptor set {} but the device binds at most {} sets",
            plan.groups.len() - 1,
            limits.max_bound_descriptor_sets
        )));
    }
    if plan.uniform_count() > limits.max_descriptor_set_uniform_buffers_dynamic as usize {
        return Err(unsupported(format!(
            "declares {} uniform buffers but the device allows {}",
            plan.uniform_count(),
            limits.max_descriptor_set_uniform_buffers_dynamic
        )));
    }
    for (binding, size) in interface.uniform_bindings() {
        if size > limits.max_uniform_buffer_range {
            return Err(unsupported(format!(
                "uniform block at (group {}, binding {}) is {size} bytes; the device allows {}",
                binding.slot.group, binding.slot.binding, limits.max_uniform_buffer_range
            )));
        }
    }
    Ok(())
}

fn create_module(device: &ash::Device, words: &[u32]) -> Result<vk::ShaderModule> {
    let info = vk::ShaderModuleCreateInfo::default().code(words);
    // SAFETY: `words` is a complete SPIR-V module produced by naga.
    unsafe { device.create_shader_module(&info, None) }.vk("vkCreateShaderModule")
}

fn create_set_layout(device: &ash::Device, group: &GroupPlan) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding<'_>> = group
        .bindings
        .iter()
        .map(|b| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b.binding)
                .descriptor_type(translate::descriptor_type(b.kind))
                .descriptor_count(1)
                .stage_flags(translate::stage_flags(b.stages))
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: `info` references `bindings`, alive for the call.
    unsafe { device.create_descriptor_set_layout(&info, None) }.vk("vkCreateDescriptorSetLayout")
}

// -------------------------------------------------------------- render passes

/// What the color attachment is used for after the pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PassKind {
    /// Offscreen target, read back by a transfer afterwards.
    Offscreen,
    /// Swapchain image, presented afterwards.
    Surface,
}

/// One subpass: clear color + clear depth, store color, discard depth.
pub(crate) fn create_render_pass(
    device: &ash::Device,
    color_format: vk::Format,
    kind: PassKind,
) -> Result<vk::RenderPass> {
    let final_layout = match kind {
        PassKind::Offscreen => vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        PassKind::Surface => vk::ImageLayout::PRESENT_SRC_KHR,
    };
    let attachments = [
        vk::AttachmentDescription::default()
            .format(color_format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(final_layout),
        vk::AttachmentDescription::default()
            .format(translate::DEPTH_FORMAT)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL),
    ];
    let color_ref = [vk::AttachmentReference::default()
        .attachment(0)
        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let depth_ref = vk::AttachmentReference::default()
        .attachment(1)
        .layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
    let subpasses = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color_ref)
        .depth_stencil_attachment(&depth_ref)];
    let attachment_stages = vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
        | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
        | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS;
    let attachment_writes =
        vk::AccessFlags::COLOR_ATTACHMENT_WRITE | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE;
    let dependencies = [
        // Orders this pass after the previous use of the same attachments (the
        // depth image is reused frame to frame; the swapchain image arrives via
        // the acquire semaphore waited at COLOR_ATTACHMENT_OUTPUT).
        vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(attachment_stages)
            .src_access_mask(attachment_writes)
            .dst_stage_mask(attachment_stages)
            .dst_access_mask(attachment_writes),
        vk::SubpassDependency::default()
            .src_subpass(0)
            .dst_subpass(vk::SUBPASS_EXTERNAL)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            // Identical for both pass kinds so their compatibility holds; later
            // consumers (present, readback) synchronize with a semaphore/fence.
            .dst_stage_mask(vk::PipelineStageFlags::BOTTOM_OF_PIPE)
            .dst_access_mask(vk::AccessFlags::empty()),
    ];
    let info = vk::RenderPassCreateInfo::default()
        .attachments(&attachments)
        .subpasses(&subpasses)
        .dependencies(&dependencies);
    // SAFETY: `info` references locals alive for the call.
    unsafe { device.create_render_pass(&info, None) }.vk("vkCreateRenderPass")
}

// ----------------------------------------------------------------- pipelines

/// Cache key: `(shader, vertex layout, pipeline state, target format)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PipelineKey {
    pub shader: u64,
    pub layout: VertexLayout,
    pub state: PipelineState,
    pub format: vk::Format,
}

/// All graphics pipelines plus the render passes they were built against.
pub(crate) struct PipelineStore {
    cache: vk::PipelineCache,
    pipelines: HashMap<PipelineKey, vk::Pipeline>,
    pub offscreen_pass: vk::RenderPass,
    /// Present only while a swapchain exists.
    pub surface_pass: Option<(vk::Format, vk::RenderPass)>,
}

impl PipelineStore {
    /// A store with no Vulkan objects yet; call [`Self::init`].
    pub(crate) fn empty() -> Self {
        Self {
            cache: vk::PipelineCache::null(),
            pipelines: HashMap::new(),
            offscreen_pass: vk::RenderPass::null(),
            surface_pass: None,
        }
    }

    pub(crate) fn init(&mut self, device: &ash::Device) -> Result<()> {
        self.offscreen_pass =
            create_render_pass(device, translate::COLOR_FORMAT, PassKind::Offscreen)?;
        let info = vk::PipelineCacheCreateInfo::default();
        // SAFETY: valid, empty create-info.
        self.cache =
            unsafe { device.create_pipeline_cache(&info, None) }.vk("vkCreatePipelineCache")?;
        Ok(())
    }

    /// Render pass pipelines of `format` are built against.
    fn pass_for(&self, format: vk::Format) -> Result<vk::RenderPass> {
        if format == translate::COLOR_FORMAT {
            return Ok(self.offscreen_pass);
        }
        match self.surface_pass {
            Some((surface_format, pass)) if surface_format == format => Ok(pass),
            _ => Err(RenderError::Backend(format!(
                "no render pass for target format {format:?}"
            ))),
        }
    }

    pub(crate) fn get_or_create(
        &mut self,
        device: &ash::Device,
        program: &Program,
        key: &PipelineKey,
    ) -> Result<vk::Pipeline> {
        if let Some(pipeline) = self.pipelines.get(key) {
            return Ok(*pipeline);
        }
        let pass = self.pass_for(key.format)?;
        let pipeline = create_pipeline(device, self.cache, pass, program, key)?;
        self.pipelines.insert(key.clone(), pipeline);
        Ok(pipeline)
    }

    /// Remove (without destroying) every pipeline built from `shader`.
    pub(crate) fn take_shader(&mut self, shader: u64) -> Vec<vk::Pipeline> {
        self.take_where(|key| key.shader == shader)
    }

    /// Remove (without destroying) every pipeline for `format`.
    pub(crate) fn take_format(&mut self, format: vk::Format) -> Vec<vk::Pipeline> {
        self.take_where(|key| key.format == format)
    }

    fn take_where(&mut self, matches: impl Fn(&PipelineKey) -> bool) -> Vec<vk::Pipeline> {
        let keys: Vec<PipelineKey> = self
            .pipelines
            .keys()
            .filter(|k| matches(k))
            .cloned()
            .collect();
        keys.iter()
            .filter_map(|key| self.pipelines.remove(key))
            .collect()
    }

    pub(crate) fn len(&self) -> usize {
        self.pipelines.len()
    }

    /// Destroy everything. The GPU must be idle.
    pub(crate) fn destroy(&mut self, device: &ash::Device) {
        // SAFETY: the caller guarantees the device is idle.
        unsafe {
            for (_, pipeline) in self.pipelines.drain() {
                device.destroy_pipeline(pipeline, None);
            }
            device.destroy_pipeline_cache(self.cache, None);
            device.destroy_render_pass(self.offscreen_pass, None);
            if let Some((_, pass)) = self.surface_pass.take() {
                device.destroy_render_pass(pass, None);
            }
        }
        self.cache = vk::PipelineCache::null();
        self.offscreen_pass = vk::RenderPass::null();
    }
}

fn create_pipeline(
    device: &ash::Device,
    cache: vk::PipelineCache,
    pass: vk::RenderPass,
    program: &Program,
    key: &PipelineKey,
) -> Result<vk::Pipeline> {
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(program.vertex_module)
            .name(&program.vertex_entry),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(program.fragment_module)
            .name(&program.fragment_entry),
    ];
    let bindings = [vk::VertexInputBindingDescription::default()
        .binding(0)
        .stride(key.layout.stride)
        .input_rate(vk::VertexInputRate::VERTEX)];
    // Only attributes the vertex shader reads are declared.
    let attributes: Vec<vk::VertexInputAttributeDescription> = program
        .interface
        .vertex_inputs
        .iter()
        .filter_map(|input| key.layout.attribute(input.location))
        .map(|attribute| {
            vk::VertexInputAttributeDescription::default()
                .location(attribute.location)
                .binding(0)
                .format(translate::vertex_format(attribute.format))
                .offset(attribute.offset)
        })
        .collect();
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&bindings)
        .vertex_attribute_descriptions(&attributes);
    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(translate::cull_mode(key.state.cull))
        .front_face(translate::FRONT_FACE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let depth = translate::depth_stencil_state(&key.state);
    let blend_attachments = [translate::color_blend_attachment(key.state.blend)];
    let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .depth_stencil_state(&depth)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(program.pipeline_layout)
        .render_pass(pass)
        .subpass(0);
    // SAFETY: every pointer in `info` references a local alive for the call, and
    // the layout/modules/pass belong to this device.
    let result = unsafe { device.create_graphics_pipelines(cache, &[info], None) };
    match result {
        Ok(pipelines) => Ok(pipelines[0]),
        Err((pipelines, result)) => {
            for pipeline in pipelines {
                // SAFETY: partially created pipelines are unused.
                unsafe { device.destroy_pipeline(pipeline, None) };
            }
            Err(crate::error::from_vk("vkCreateGraphicsPipelines", result))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_render::ShaderSource;

    /// The textured sprite shader the desktop renderer uses (a verbatim copy of
    /// `crates/engine-desktop/src/textured.wgsl`).
    pub(crate) const TEXTURED: &str = r#"
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
"#;

    fn interface(code: &str) -> ShaderInterface {
        ShaderInterface::reflect(&ShaderResource::new("test", ShaderSource::wgsl(code))).unwrap()
    }

    fn contains_name(words: &[u32], name: &str) -> bool {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        bytes.windows(name.len()).any(|w| w == name.as_bytes())
    }

    #[test]
    fn textured_shader_translates_to_spirv_1_3_modules() {
        let modules = compile_wgsl(TEXTURED, "vs_main", "fs_main").unwrap();
        for words in [&modules.vertex, &modules.fragment] {
            assert_eq!(words[0], 0x0723_0203, "SPIR-V magic");
            assert_eq!(words[1], 0x0001_0300, "version 1.3");
        }
        assert!(contains_name(&modules.vertex, "vs_main"));
        assert!(!contains_name(&modules.vertex, "fs_main"));
        assert!(contains_name(&modules.fragment, "fs_main"));
        assert!(!contains_name(&modules.fragment, "vs_main"));
    }

    #[test]
    fn y_flip_is_left_to_the_viewport_not_the_shader() {
        let ours = compile_wgsl(TEXTURED, "vs_main", "fs_main").unwrap();
        let flipped = translate_with_flags(
            TEXTURED,
            "vs_main",
            "fs_main",
            default_writer_flags() | naga::back::spv::WriterFlags::ADJUST_COORDINATE_SPACE,
        )
        .unwrap();
        assert_ne!(ours.vertex, flipped.vertex, "naga must not flip Y itself");
        assert_eq!(ours.fragment, flipped.fragment);
    }

    #[test]
    fn spirv_output_parses_back_and_validates() {
        // Reads the SPIR-V we generate through naga's SPIR-V frontend and runs
        // the full validator over it: a structural sanity check of the words.
        let modules = compile_wgsl(TEXTURED, "vs_main", "fs_main").unwrap();
        for (words, entry, stage) in [
            (&modules.vertex, "vs_main", naga::ShaderStage::Vertex),
            (&modules.fragment, "fs_main", naga::ShaderStage::Fragment),
        ] {
            let module = naga::front::spv::parse_u8_slice(
                &words
                    .iter()
                    .flat_map(|w| w.to_le_bytes())
                    .collect::<Vec<u8>>(),
                &naga::front::spv::Options::default(),
            )
            .unwrap();
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
            assert!(
                module
                    .entry_points
                    .iter()
                    .any(|e| e.name == entry && e.stage == stage)
            );
        }
    }

    #[test]
    fn bindings_keep_their_group_and_binding_as_descriptor_set_and_binding() {
        let modules = compile_wgsl(TEXTURED, "vs_main", "fs_main").unwrap();
        let module = naga::front::spv::parse_u8_slice(
            &modules
                .fragment
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect::<Vec<u8>>(),
            &naga::front::spv::Options::default(),
        )
        .unwrap();
        let mut slots: Vec<(u32, u32)> = module
            .global_variables
            .iter()
            .filter_map(|(_, v)| v.binding.map(|b| (b.group, b.binding)))
            .collect();
        slots.sort_unstable();
        assert_eq!(slots, vec![(1, 0), (1, 1)]);
    }

    #[test]
    fn invalid_wgsl_is_reported_as_a_shader_error() {
        let parse = compile_wgsl("fn main( {", "vs_main", "fs_main");
        assert!(matches!(parse, Err(RenderError::Shader(m)) if m.contains("WGSL parse failed")));
        let invalid = compile_wgsl(
            // A vertex stage without a position output parses but cannot validate.
            "@vertex fn vs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }\n\
             @fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }",
            "vs_main",
            "fs_main",
        );
        assert!(
            matches!(invalid, Err(RenderError::Shader(m)) if m.contains("WGSL validation failed"))
        );
    }

    #[test]
    fn missing_entry_point_is_reported_by_name() {
        let result = compile_wgsl(TEXTURED, "vs_main", "no_such_fragment");
        let Err(RenderError::Shader(message)) = result else {
            panic!("expected a shader error");
        };
        assert!(message.contains("no_such_fragment"), "{message}");
    }

    #[test]
    fn textured_shader_plan_has_a_uniform_set_and_a_texture_sampler_set() {
        let plan = LayoutPlan::from_interface(&interface(TEXTURED));
        assert_eq!(plan.groups.len(), 2);
        let g0 = &plan.groups[0];
        assert_eq!(g0.bindings.len(), 1);
        assert_eq!(g0.bindings[0].binding, 0);
        assert!(matches!(
            g0.bindings[0].kind,
            BindingKind::UniformBuffer { size: 64 }
        ));
        assert_eq!(
            g0.bindings[0].stages,
            StageUsage {
                vertex: true,
                fragment: false
            }
        );
        let g1 = &plan.groups[1];
        assert!(g1.has_texture());
        assert_eq!(g1.bindings.len(), 2);
        assert_eq!(
            g1.bindings[1],
            BindingPlan {
                binding: 1,
                kind: BindingKind::Sampler,
                stages: StageUsage {
                    vertex: false,
                    fragment: true
                }
            }
        );
        assert_eq!(plan.uniform_count(), 1);
        assert_eq!(plan.dynamic_slots(0), vec![UniformSlot::new(0, 0)]);
        assert!(plan.dynamic_slots(1).is_empty());
    }

    #[test]
    fn groups_below_the_highest_used_one_exist_but_are_empty() {
        let plan = LayoutPlan::from_interface(&interface(
            "@group(1) @binding(0) var image: texture_2d<f32>;\n\
             @group(1) @binding(1) var smp: sampler;\n\
             @vertex fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {\n\
                 return vec4<f32>(f32(i), 0.0, 0.0, 1.0);\n\
             }\n\
             @fragment fn fs_main() -> @location(0) vec4<f32> {\n\
                 return textureSample(image, smp, vec2<f32>(0.5, 0.5));\n\
             }",
        ));
        assert_eq!(plan.groups.len(), 2);
        assert!(plan.groups[0].is_empty());
        assert!(!plan.groups[1].is_empty());
    }

    #[test]
    fn dynamic_offsets_follow_binding_order_within_a_set() {
        let plan = LayoutPlan::from_interface(&interface(
            "struct A { v: vec4<f32> }\n\
             struct B { v: vec4<f32> }\n\
             @group(2) @binding(5) var<uniform> b: B;\n\
             @group(2) @binding(1) var<uniform> a: A;\n\
             @vertex fn vs_main() -> @builtin(position) vec4<f32> { return a.v + b.v; }\n\
             @fragment fn fs_main() -> @location(0) vec4<f32> { return b.v; }",
        ));
        assert_eq!(plan.groups.len(), 3);
        assert!(plan.groups[0].is_empty() && plan.groups[1].is_empty());
        assert_eq!(
            plan.dynamic_slots(2),
            vec![UniformSlot::new(2, 1), UniformSlot::new(2, 5)]
        );
        // `a` is vertex-only, `b` is used by both stages.
        assert_eq!(
            plan.groups[2].bindings[0].stages,
            StageUsage {
                vertex: true,
                fragment: false
            }
        );
        assert_eq!(
            plan.groups[2].bindings[1].stages,
            StageUsage {
                vertex: true,
                fragment: true
            }
        );
    }

    #[test]
    fn shaders_without_resources_have_no_groups() {
        let plan = LayoutPlan::from_interface(&interface(
            "@vertex fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {\n\
                 return vec4<f32>(f32(i), 0.0, 0.0, 1.0);\n\
             }\n\
             @fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }",
        ));
        assert!(plan.groups.is_empty());
        assert_eq!(plan.uniform_count(), 0);
    }

    #[test]
    fn limits_reject_oversized_uniform_blocks_and_too_many_sets() {
        let interface = interface(TEXTURED);
        let plan = LayoutPlan::from_interface(&interface);
        let mut limits = vk::PhysicalDeviceLimits {
            max_bound_descriptor_sets: 4,
            max_descriptor_set_uniform_buffers_dynamic: 8,
            max_uniform_buffer_range: 65536,
            ..Default::default()
        };
        check_limits(&interface, &plan, &limits, "t").unwrap();
        limits.max_uniform_buffer_range = 32;
        assert!(matches!(
            check_limits(&interface, &plan, &limits, "t"),
            Err(RenderError::Unsupported { .. })
        ));
        limits.max_uniform_buffer_range = 65536;
        limits.max_bound_descriptor_sets = 1;
        assert!(check_limits(&interface, &plan, &limits, "t").is_err());
        limits.max_bound_descriptor_sets = 4;
        limits.max_descriptor_set_uniform_buffers_dynamic = 0;
        assert!(check_limits(&interface, &plan, &limits, "t").is_err());
    }

    #[test]
    fn pipeline_key_distinguishes_every_component() {
        use engine_render::{BlendMode, CullMode, VertexAttribute, VertexFormat};
        let layout = VertexLayout::new(
            12,
            [VertexAttribute {
                location: 0,
                format: VertexFormat::Float32x3,
                offset: 0,
            }],
        );
        let key = PipelineKey {
            shader: 1,
            layout: layout.clone(),
            state: PipelineState {
                blend: BlendMode::Opaque,
                depth_test: true,
                depth_write: true,
                cull: CullMode::None,
            },
            format: vk::Format::R8G8B8A8_SRGB,
        };
        let mut set = std::collections::HashSet::new();
        assert!(set.insert(key.clone()));
        assert!(!set.insert(key.clone()));
        let mut other = key.clone();
        other.shader = 2;
        assert!(set.insert(other));
        let mut other = key.clone();
        other.state.cull = CullMode::Back;
        assert!(set.insert(other));
        let mut other = key.clone();
        other.format = vk::Format::B8G8R8A8_SRGB;
        assert!(set.insert(other));
        let mut other = key;
        other.layout.stride = 16;
        assert!(set.insert(other));
    }
}
