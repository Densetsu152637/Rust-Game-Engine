//! `world.terrain`: GPU heightmap terrain.
//!
//! Each view selects CDLOD nodes on the CPU (`engine_terrain::LodLayout`, the same selection
//! the CPU fallback uses), frustum-culls them and draws every node of a terrain as one
//! instance of a shared `node_resolution` x `node_resolution` patch: one instanced draw call
//! per terrain. The vertex shader reads heights from an `R32Float` texture and geomorphs them
//! exactly like `engine_terrain::lod::morphed_height`; the fragment shader blends up to eight
//! splat layers (tint x optional tiled albedo texture) and lights them with `engine::lighting`.
//! Holes are discarded per fragment. `world.terrain.normals` writes the same surface into the
//! normals target (and scene depth) in the `Prepass` stage when a feature needs normals.
//!
//! The features read `engine_project::world::terrain::TerrainRenderSet` from the extracted
//! world; [`install`] adds the extraction and the `TerrainGpuRenderer` marker that keeps
//! `render_mode: auto` terrains off the CPU fallback path.
mod extract;
mod gpu;

pub use extract::extract_terrain;
pub use gpu::TerrainInstance;

use crate::names;
use anyhow::Result;
use engine_core::EngineApp;
use engine_render_pipeline::{
    DrawStats, FeatureContext, FeatureDescriptor, FeatureSetup, Grouping, InstanceBatcher,
    PipelineFeature, PipelineFeatures, Stage, names as engine_names,
};
use gpu::Shared;
use std::sync::{Arc, Mutex};

/// What the terrain features did in the last view they ran for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerrainRenderStatus {
    /// Views encoded since registration (colour pass).
    pub views: u64,
    /// Terrains drawn in the last view.
    pub terrains: usize,
    /// Nodes (instances) drawn and nodes culled by the frustum.
    pub nodes: usize,
    pub culled: usize,
    /// Instanced draw calls of the colour pass (one per terrain).
    pub draw_calls: usize,
    /// Texture uploads since registration (one per terrain revision or layer change).
    pub uploads: u64,
    /// Whether the normals prepass ran in the last frame.
    pub normals_pass: bool,
    /// Why something was not drawn.
    pub notes: Vec<String>,
}

/// Shared handle to [`TerrainRenderStatus`].
#[derive(Clone, Debug, Default)]
pub struct TerrainRenderStatusHandle(Arc<Mutex<TerrainRenderStatus>>);
impl TerrainRenderStatusHandle {
    /// A copy of the current status.
    pub fn get(&self) -> TerrainRenderStatus {
        self.0.lock().map(|s| s.clone()).unwrap_or_default()
    }
    fn update(&self, change: impl FnOnce(&mut TerrainRenderStatus)) {
        if let Ok(mut status) = self.0.lock() {
            change(&mut status);
        }
    }
}

/// The colour pass (`world.terrain`, `Opaque` stage after `engine.opaque`).
pub struct TerrainFeature {
    shared: Arc<Mutex<Shared>>,
    batcher: InstanceBatcher<TerrainInstance>,
    status: TerrainRenderStatusHandle,
}
/// The normals pass (`world.terrain.normals`, `Prepass` stage after `engine.prepass.normals`).
pub struct TerrainNormalsFeature {
    shared: Arc<Mutex<Shared>>,
    batcher: InstanceBatcher<TerrainInstance>,
    status: TerrainRenderStatusHandle,
}

impl TerrainFeature {
    /// The pair of features over one shared GPU cache.
    pub fn pair(status: TerrainRenderStatusHandle) -> (TerrainFeature, TerrainNormalsFeature) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        (
            TerrainFeature {
                shared: shared.clone(),
                batcher: InstanceBatcher::new("world.terrain instances"),
                status: status.clone(),
            },
            TerrainNormalsFeature {
                shared,
                batcher: InstanceBatcher::new("world.terrain.normals instances"),
                status,
            },
        )
    }
}

fn lock(shared: &Mutex<Shared>) -> Result<std::sync::MutexGuard<'_, Shared>> {
    shared
        .lock()
        .map_err(|_| anyhow::anyhow!("world.terrain state poisoned"))
}

impl PipelineFeature for TerrainFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::TERRAIN, Stage::Opaque).after(engine_names::OPAQUE)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        lock(&self.shared)?.prepare(setup)
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let mut shared = lock(&self.shared)?;
        let frame = shared.collect(ctx, &mut self.batcher)?;
        self.status.update(|s| {
            s.views += 1;
            s.terrains = frame.terrains;
            s.nodes = frame.nodes;
            s.culled = frame.culled;
            s.uploads += frame.uploads;
            s.notes = frame.notes.clone();
            s.draw_calls = 0;
        });
        if frame.nodes == 0 {
            return Ok(());
        }
        self.batcher
            .finish(&ctx.gpu.device, &ctx.gpu.queue, Grouping::ByKey)?;
        let view_group = ctx.view_bind_group;
        let mut pass = ctx.scene_pass("world.terrain")?;
        let stats = shared.draw(&mut pass, view_group, &self.batcher, false)?;
        drop(pass);
        ctx.record_draws(stats);
        self.status.update(|s| s.draw_calls = stats.draw_calls);
        Ok(())
    }
}

impl PipelineFeature for TerrainNormalsFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::TERRAIN_NORMALS, Stage::Prepass).after(engine_names::NORMALS)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        lock(&self.shared)?.prepare(setup)
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let Some(normals) = ctx.normals().map(|t| t.view.clone()) else {
            self.status.update(|s| s.normals_pass = false);
            return Ok(());
        };
        let depth = ctx.depth()?.view.clone();
        let mut shared = lock(&self.shared)?;
        let frame = shared.collect(ctx, &mut self.batcher)?;
        self.status.update(|s| s.uploads += frame.uploads);
        if frame.nodes == 0 {
            return Ok(());
        }
        self.batcher
            .finish(&ctx.gpu.device, &ctx.gpu.queue, Grouping::ByKey)?;
        let view_group = ctx.view_bind_group;
        let mut pass = ctx
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world.terrain.normals"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &normals,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            })
            .forget_lifetime();
        let stats: DrawStats = shared.draw(&mut pass, view_group, &self.batcher, true)?;
        drop(pass);
        ctx.record_draws(stats);
        self.status.update(|s| s.normals_pass = true);
        Ok(())
    }
}

pub(crate) fn register(features: &mut PipelineFeatures) -> Result<TerrainRenderStatusHandle> {
    let status = TerrainRenderStatusHandle::default();
    let (color, normals) = TerrainFeature::pair(status.clone());
    features.register(normals)?;
    features.register(color)?;
    Ok(status)
}

pub(crate) fn install(app: &mut EngineApp) {
    extract::install(app);
}
