//! An external game can own heap data, native schedules and unrestricted GPU
//! passes (render features) without changes to the engine's fixed authored
//! component catalog.
use engine_core::{bevy_ecs, prelude::*};
use engine_desktop::render_pipeline::{
    FeatureContext, FeatureDescriptor, PipelineFeature, Stage as RenderStage,
};
use engine_desktop::{PipelineFeatures, wgpu};

#[derive(Component, Clone, Debug)]
pub struct CustomActor {
    pub name: String,
    pub payload: Vec<u8>,
    pub ticks: u64,
}
#[derive(Component, Clone, Copy)]
pub struct CustomColor(pub [f32; 4]);
#[derive(Bundle)]
pub struct CustomBundle {
    pub actor: CustomActor,
    pub color: CustomColor,
}
#[derive(Resource, Default)]
pub struct CustomState {
    pub messages: Vec<String>,
}
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum GameSystems {
    Advance,
    Record,
}

pub fn configure(app: &mut EngineApp) -> Result<()> {
    app.world_mut().insert_resource(CustomState::default());
    app.world_mut().spawn(CustomBundle {
        actor: CustomActor {
            name: "external heap-owned actor".into(),
            payload: vec![3, 1, 4],
            ticks: 0,
        },
        color: CustomColor([0.0, 1.0, 0.0, 1.0]),
    });
    app.schedule_mut(Stage::Update)
        .configure_sets((GameSystems::Advance, GameSystems::Record).chain())
        .add_systems(advance.in_set(GameSystems::Advance))
        .add_systems(record.in_set(GameSystems::Record));
    app.extract_component::<CustomActor>()
        .extract_component::<CustomColor>();
    let mut features = PipelineFeatures::default();
    features.register(CustomTriangle::default())?;
    app.world_mut().insert_resource(features);
    Ok(())
}
fn advance(mut query: Query<&mut CustomActor>) {
    for mut actor in &mut query {
        actor.ticks += 1;
        let tick = actor.ticks as u8;
        actor.payload.push(tick);
    }
}
fn record(query: Query<&CustomActor>, mut state: ResMut<CustomState>) {
    for actor in &query {
        state.messages.push(format!(
            "{}:{}:{}",
            actor.name,
            actor.ticks,
            actor.payload.len()
        ));
    }
}

/// An overlay-stage render feature: drawn into the output target after the
/// engine's 3D output and 2D sprites, coloured by a game component read from
/// the extracted world.
#[derive(Default)]
pub struct CustomTriangle {
    /// Pipeline for the current output format (built on first use).
    pipeline: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    colors: Option<wgpu::Buffer>,
    bind: Option<wgpu::BindGroup>,
}
impl CustomTriangle {
    fn build(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("external color layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let colors = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("external component color"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("external color binding"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: colors.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("external pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("external game shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("triangle.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("external triangle pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.pipeline = Some((format, pipeline));
        self.colors = Some(colors);
        self.bind = Some(bind);
    }
}
impl PipelineFeature for CustomTriangle {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new("example.triangle", RenderStage::Overlay)
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let world = ctx
            .world
            .ok_or_else(|| engine_core::anyhow::anyhow!("custom pass requires extracted world"))?;
        let color = world
            .iter_entities()
            .find_map(|entity| entity.get::<CustomColor>().copied())
            .ok_or_else(|| engine_core::anyhow::anyhow!("custom component was not extracted"))?;
        let target = ctx.output()?.clone();
        if self
            .pipeline
            .as_ref()
            .is_none_or(|(f, _)| *f != target.format)
        {
            self.build(&ctx.gpu.device, target.format);
        }
        let bytes: Vec<u8> = color.0.into_iter().flat_map(f32::to_ne_bytes).collect();
        ctx.gpu
            .queue
            .write_buffer(self.colors.as_ref().unwrap(), 0, &bytes);
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("external actual render pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline.as_ref().unwrap().1);
        pass.set_bind_group(0, self.bind.as_ref().unwrap(), &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}
