//! `world.foliage`: GPU foliage and vegetation.
//!
//! Each view walks the cells of every set in `engine_project::world::foliage::FoliageRenderSet`
//! (the extracted world), culls cells and instances against the view frustum and the types'
//! cull distances, picks a level per instance from the type's `LodBands` (mesh LODs, then a
//! camera-facing impostor, with hysteresis kept per view) and pushes the survivors into an
//! `InstanceBatcher`: **one instanced draw call per (type, level)**. The vertex shaders sway
//! vertices with the same wind model as `engine_foliage::WindParams::sway_offset`; the fragment
//! shader (`#import engine::lighting`) lights two-sided, alpha-cut surfaces with the engine's
//! direct and indirect lighting.
//!
//! [`install`] adds the extraction and the `FoliageGpuRenderer` marker that keeps
//! `render_mode: auto` foliage off the CPU cluster path. The feature draws in the `Opaque`
//! stage; `world.foliage.shadow` adds casters to allocated shared shadow tiles. Foliage writes
//! no normals or motion vectors.
mod extract;
mod gpu;

pub use extract::extract_foliage;
pub use gpu::{BatchInfo, FoliageInstance};

use crate::names;
use anyhow::Result;
use engine_core::EngineApp;
use engine_render_pipeline::{
    FeatureContext, FeatureDescriptor, FeatureSetup, Grouping, InstanceBatcher, PipelineFeature,
    PipelineFeatures, Stage, names as engine_names,
};
use gpu::Shared;
use std::sync::{Arc, Mutex};

/// What the foliage feature did in the last view it ran for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FoliageRenderStatus {
    /// Views encoded since registration.
    pub views: u64,
    /// Foliage sets drawn in the last view.
    pub sets: usize,
    /// Instances drawn (meshes and impostors) and, of those, impostors.
    pub instances: usize,
    pub impostors: usize,
    /// Cells visited and rejected as a whole (distance or frustum).
    pub cells_visited: usize,
    pub cells_culled: usize,
    /// Instances rejected by the cull distance and by the frustum.
    pub culled_distance: usize,
    pub culled_frustum: usize,
    /// Instanced draw calls of the last view (one per batch).
    pub draw_calls: usize,
    /// Batches of the last view: one per type and level with instances.
    pub batches: Vec<BatchInfo>,
    /// GPU data builds since registration (one per set when its assets change).
    pub uploads: u64,
    /// Why something was not drawn.
    pub notes: Vec<String>,
}

/// Shared handle to [`FoliageRenderStatus`].
#[derive(Clone, Debug, Default)]
pub struct FoliageRenderStatusHandle(Arc<Mutex<FoliageRenderStatus>>);
impl FoliageRenderStatusHandle {
    /// A copy of the current status.
    pub fn get(&self) -> FoliageRenderStatus {
        self.0.lock().map(|s| s.clone()).unwrap_or_default()
    }
    fn update(&self, change: impl FnOnce(&mut FoliageRenderStatus)) {
        if let Ok(mut status) = self.0.lock() {
            change(&mut status);
        }
    }
}

/// The colour pass (`world.foliage`, `Opaque` stage after `engine.opaque`).
pub struct FoliageFeature {
    shared: Arc<Mutex<Shared>>,
    batcher: InstanceBatcher<FoliageInstance>,
    status: FoliageRenderStatusHandle,
}

/// Foliage depth into each allocated shared shadow atlas tile.
pub struct FoliageShadowFeature {
    shared: Arc<Mutex<Shared>>,
    /// Separate retained buffer per tile, since queue writes precede encoder submission.
    batchers: Vec<InstanceBatcher<FoliageInstance>>,
    status: FoliageRenderStatusHandle,
}

impl FoliageFeature {
    pub fn new(status: FoliageRenderStatusHandle) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared::default())),
            batcher: InstanceBatcher::new("world.foliage instances"),
            status,
        }
    }
}

fn lock(shared: &Mutex<Shared>) -> Result<std::sync::MutexGuard<'_, Shared>> {
    shared
        .lock()
        .map_err(|_| anyhow::anyhow!("world.foliage state poisoned"))
}

impl PipelineFeature for FoliageFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::FOLIAGE, Stage::Opaque).after(engine_names::OPAQUE)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        lock(&self.shared)?.prepare(setup)
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let mut shared = lock(&self.shared)?;
        let frame = shared.collect(ctx, &mut self.batcher)?;
        self.status.update(|s| {
            s.views += 1;
            s.sets = frame.sets;
            s.instances = frame.instances;
            s.impostors = frame.impostors;
            s.cells_visited = frame.cells_visited;
            s.cells_culled = frame.cells_culled;
            s.culled_distance = frame.culled_distance;
            s.culled_frustum = frame.culled_frustum;
            s.uploads += frame.uploads;
            s.batches = frame.batches.clone();
            s.notes = frame.notes.clone();
            s.draw_calls = 0;
        });
        if frame.instances == 0 {
            return Ok(());
        }
        self.batcher
            .finish(&ctx.gpu.device, &ctx.gpu.queue, Grouping::ByKey)?;
        let view_group = ctx.view_bind_group;
        let mut pass = ctx.scene_pass("world.foliage")?;
        let stats = shared.draw(&mut pass, view_group, &self.batcher, false)?;
        drop(pass);
        ctx.record_draws(stats);
        self.status.update(|s| s.draw_calls = stats.draw_calls);
        Ok(())
    }
}

impl PipelineFeature for FoliageShadowFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::FOLIAGE_SHADOW, Stage::Shadow).after(engine_names::SHADOWS)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        self.batchers.clear();
        lock(&self.shared)?.prepare(setup)
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let views = ctx.shadow_views()?.to_vec();
        self.batchers.truncate(views.len());
        self.batchers.resize_with(views.len(), || {
            InstanceBatcher::new("world.foliage.shadow instances")
        });
        let mut shared = lock(&self.shared)?;
        let mut stats = engine_render_pipeline::DrawStats::default();
        let mut uploads = 0;
        for (tile, view) in views.iter().enumerate() {
            let batcher = &mut self.batchers[tile];
            let frame = shared.collect_shadow(ctx, batcher, view.view.clip_from_world)?;
            uploads += frame.uploads;
            if frame.instances == 0 {
                continue;
            }
            batcher.finish(&ctx.gpu.device, &ctx.gpu.queue, Grouping::ByKey)?;
            let (indices, offset) = shared.shadow_index(view.index)?;
            let mut pass = ctx.shadow_pass("world.foliage.shadow", tile)?;
            pass.set_bind_group(3, &indices, &[offset]);
            stats += shared.draw(&mut pass, ctx.view_bind_group, batcher, true)?;
        }
        ctx.record_draws(stats);
        self.status.update(|s| s.uploads += uploads);
        Ok(())
    }
}

pub(crate) fn register(features: &mut PipelineFeatures) -> Result<FoliageRenderStatusHandle> {
    let status = FoliageRenderStatusHandle::default();
    let color = FoliageFeature::new(status.clone());
    let shadow = FoliageShadowFeature {
        shared: color.shared.clone(),
        batchers: Vec::new(),
        status: status.clone(),
    };
    features.register(shadow)?;
    features.register(color)?;
    Ok(status)
}

pub(crate) fn install(app: &mut EngineApp) {
    extract::install(app);
}
