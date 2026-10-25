//! Small wgpu helpers shared by the features.

use anyhow::Result;
use engine_render_pipeline::{DEPTH_FORMAT, FeatureSetup};

/// Composes `source` (resolving `#import`) and validates it with naga before creating the module.
pub fn shader_module(
    setup: &FeatureSetup<'_>,
    label: &str,
    source: &str,
) -> Result<wgpu::ShaderModule> {
    let composed = setup.composer.compose(source)?;
    engine_render_pipeline::validate_wgsl(&composed)?;
    Ok(setup
        .gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(composed.into()),
        }))
}

/// Depth state against the scene depth: `LessEqual`, or `Always` when the test is off.
pub fn depth_state(write: bool, test: bool) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(write),
        depth_compare: Some(if test {
            wgpu::CompareFunction::LessEqual
        } else {
            wgpu::CompareFunction::Always
        }),
        stencil: Default::default(),
        bias: Default::default(),
    }
}

/// A pipeline layout holding only the `engine::view` group.
pub fn view_layout(setup: &FeatureSetup<'_>, label: &str) -> wgpu::PipelineLayout {
    setup
        .gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(&setup.layouts.view)],
            immediate_size: 0,
        })
}
