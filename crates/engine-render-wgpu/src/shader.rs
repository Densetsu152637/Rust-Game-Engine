//! Shader programs: bind group layouts derived from the reflected interface,
//! and the render pipelines built from them.

use crate::context::GpuContext;
use crate::translate::{
    DEPTH_FORMAT, blend_state, cull_face, depth_state, shader_stages, vertex_format,
};
use engine_render::shader::{BindingKind, ShaderBinding, ShaderInterface};
use engine_render::{PipelineState, RenderError, Result, ShaderResource, VertexLayout};
use std::collections::BTreeMap;

/// Pipeline cache key: `(shader, vertex layout, pipeline state, target format)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PipelineKey {
    pub shader: u64,
    pub layout: VertexLayout,
    pub state: PipelineState,
    pub format: wgpu::TextureFormat,
}

/// Bind group cache key. `texture` is only set for groups that bind a texture.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BindKey {
    pub shader: u64,
    pub group: u32,
    pub texture: Option<u64>,
    pub data_textures: [Option<u64>; 2],
}

pub(crate) struct GroupEntry {
    pub group: u32,
    pub layout: wgpu::BindGroupLayout,
    /// Sorted by binding number.
    pub bindings: Vec<ShaderBinding>,
    pub has_texture: bool,
}

impl GroupEntry {
    /// Uniform bindings in binding order, which is also the dynamic offset order.
    pub fn uniforms(&self) -> impl Iterator<Item = (&ShaderBinding, u32)> {
        self.bindings
            .iter()
            .filter_map(|binding| match binding.kind {
                BindingKind::UniformBuffer { size } => Some((binding, size)),
                _ => None,
            })
    }
}

pub(crate) struct ShaderEntry {
    pub module: wgpu::ShaderModule,
    pub interface: ShaderInterface,
    /// One entry per used bind group, sorted by group index.
    pub groups: Vec<GroupEntry>,
    pub pipeline_layout: wgpu::PipelineLayout,
}

impl ShaderEntry {
    pub fn new(gpu: &GpuContext, resource: &ShaderResource) -> Result<Self> {
        let interface = ShaderInterface::reflect(resource)?;
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&resource.name),
                source: wgpu::ShaderSource::Wgsl(resource.source.code.as_str().into()),
            });
        let mut by_group: BTreeMap<u32, Vec<ShaderBinding>> = BTreeMap::new();
        for binding in &interface.bindings {
            by_group
                .entry(binding.slot.group)
                .or_default()
                .push(binding.clone());
        }
        let groups: Vec<GroupEntry> = by_group
            .into_iter()
            .map(|(group, mut bindings)| {
                bindings.sort_by_key(|binding| binding.slot.binding);
                let entries: Vec<wgpu::BindGroupLayoutEntry> =
                    bindings.iter().map(layout_entry).collect();
                let layout =
                    gpu.device
                        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                            label: Some(&resource.name),
                            entries: &entries,
                        });
                GroupEntry {
                    group,
                    has_texture: bindings
                        .iter()
                        .any(|binding| binding.kind == BindingKind::SampledTexture2d),
                    layout,
                    bindings,
                }
            })
            .collect();
        let highest = groups.last().map_or(0, |group| group.group as usize + 1);
        let mut slots: Vec<Option<&wgpu::BindGroupLayout>> = vec![None; highest];
        for group in &groups {
            slots[group.group as usize] = Some(&group.layout);
        }
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&resource.name),
                bind_group_layouts: &slots,
                immediate_size: 0,
            });
        Ok(Self {
            module,
            interface,
            groups,
            pipeline_layout,
        })
    }

    /// Build the render pipeline for one vertex layout, state and target format.
    pub fn create_pipeline(
        &self,
        device: &wgpu::Device,
        layout: &VertexLayout,
        state: &PipelineState,
        format: wgpu::TextureFormat,
    ) -> Result<wgpu::RenderPipeline> {
        let attributes: Vec<wgpu::VertexAttribute> = layout
            .attributes
            .iter()
            .map(|attribute| wgpu::VertexAttribute {
                format: vertex_format(attribute.format),
                offset: u64::from(attribute.offset),
                shader_location: attribute.location,
            })
            .collect();
        if format == DEPTH_FORMAT {
            return Err(RenderError::InvalidDraw(
                "a depth format cannot be a color target".into(),
            ));
        }
        Ok(
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("engine pipeline"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.module,
                    entry_point: Some(&self.interface.vertex_entry),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: u64::from(layout.stride),
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: cull_face(state.cull),
                    ..Default::default()
                },
                depth_stencil: Some(depth_state(state)),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some(&self.interface.fragment_entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: blend_state(state.blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            }),
        )
    }
}

fn layout_entry(binding: &ShaderBinding) -> wgpu::BindGroupLayoutEntry {
    let ty = match binding.kind {
        BindingKind::UniformBuffer { size } => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: wgpu::BufferSize::new(u64::from(size)),
        },
        BindingKind::SampledTexture2d => wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        BindingKind::Sampler => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        BindingKind::ShadowTexture2d => wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Depth,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
    };
    wgpu::BindGroupLayoutEntry {
        binding: binding.slot.binding,
        visibility: shader_stages(binding),
        ty,
        count: None,
    }
}
