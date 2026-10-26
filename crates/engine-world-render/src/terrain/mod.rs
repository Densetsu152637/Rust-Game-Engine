//! `world.terrain`: GPU heightmap terrain.
//!
//! Each view selects CDLOD nodes on the CPU (`engine_terrain::LodLayout`, the same selection
//! the CPU fallback uses), frustum-culls them and draws every node of a terrain as one
//! instance of a shared `node_resolution` x `node_resolution` patch: one instanced draw call
//! per terrain. The vertex shader reads heights from an `R32Float` texture and geomorphs them
//! exactly like `engine_terrain::lod::morphed_height`; the fragment shader blends up to eight
//! splat layers (tint x optional tiled albedo texture) and lights them with `engine::lighting`.
//! Holes are discarded per fragment.
//!
//! Four features share one GPU cache:
//!
//! - `world.terrain.shadow` (`Shadow` stage) writes terrain depth into every allocated tile of
//!   the engine shadow atlas. Terrain and native meshes receive each other's shadows.
//! - `world.terrain.normals` (`Prepass`) writes the surface into the normals target (and scene
//!   depth) when a feature needs normals.
//! - `world.terrain` (`Opaque`) is the colour pass.
//! - `world.terrain.motion` (`Opaque`, after `world.terrain`) writes current-to-previous motion
//!   (camera, terrain transform and geomorph changes) into the motion target when a feature or
//!   the host (DLSS) needs motion, where terrain is the visible surface.
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
use gpu::{PassKind, Shared};
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
    /// Whether `world.terrain.shadow` ran for allocated atlas tiles and its draw calls.
    pub shadow_pass: bool,
    pub shadow_draw_calls: usize,
    /// Whether the terrain shadow writer ran for the last colour view.
    pub shadowed: bool,
    /// Whether `world.terrain.motion` wrote terrain motion in the last frame (only when a
    /// feature or the host needs motion vectors).
    pub motion_pass: bool,
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
/// Terrain depth into the shared atlas (`Shadow` stage after `engine.shadows`).
pub struct TerrainShadowFeature {
    shared: Arc<Mutex<Shared>>,
    batcher: InstanceBatcher<TerrainInstance>,
    status: TerrainRenderStatusHandle,
}
/// Terrain motion vectors (`world.terrain.motion`, `Opaque` stage after `world.terrain`).
pub struct TerrainMotionFeature {
    shared: Arc<Mutex<Shared>>,
    batcher: InstanceBatcher<TerrainInstance>,
    status: TerrainRenderStatusHandle,
}

/// The four terrain features over one shared GPU cache, in registration order.
pub struct TerrainFeatures {
    pub shadow: TerrainShadowFeature,
    pub normals: TerrainNormalsFeature,
    pub color: TerrainFeature,
    pub motion: TerrainMotionFeature,
}

impl TerrainFeature {
    /// The colour and normals features over one shared GPU cache (no caster or motion pass);
    /// [`TerrainFeature::all`] returns every terrain feature.
    pub fn pair(status: TerrainRenderStatusHandle) -> (TerrainFeature, TerrainNormalsFeature) {
        let all = Self::all(status);
        (all.color, all.normals)
    }

    /// Every terrain feature over one shared GPU cache.
    pub fn all(status: TerrainRenderStatusHandle) -> TerrainFeatures {
        let shared = Arc::new(Mutex::new(Shared::default()));
        TerrainFeatures {
            shadow: TerrainShadowFeature {
                shared: shared.clone(),
                batcher: InstanceBatcher::new("world.terrain.shadow instances"),
                status: status.clone(),
            },
            normals: TerrainNormalsFeature {
                shared: shared.clone(),
                batcher: InstanceBatcher::new("world.terrain.normals instances"),
                status: status.clone(),
            },
            color: TerrainFeature {
                shared: shared.clone(),
                batcher: InstanceBatcher::new("world.terrain instances"),
                status: status.clone(),
            },
            motion: TerrainMotionFeature {
                shared,
                batcher: InstanceBatcher::new("world.terrain.motion instances"),
                status,
            },
        }
    }
}

impl TerrainFeatures {
    /// Registers the four features (shadow, normals, colour, motion).
    pub fn register(self, features: &mut PipelineFeatures) -> Result<()> {
        features.register(self.shadow)?;
        features.register(self.normals)?;
        features.register(self.color)?;
        features.register(self.motion)
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
        let frame = shared.collect(ctx, &mut self.batcher, PassKind::Color, None)?;
        let shadowed = shared.take_shadowed(ctx.view.id);
        self.status.update(|s| {
            s.views += 1;
            s.terrains = frame.terrains;
            s.nodes = frame.nodes;
            s.culled = frame.culled;
            s.uploads += frame.uploads;
            s.notes = frame.notes.clone();
            s.draw_calls = 0;
            s.shadowed = false;
        });
        if frame.nodes == 0 {
            return Ok(());
        }
        self.batcher
            .finish(&ctx.gpu.device, &ctx.gpu.queue, Grouping::ByKey)?;
        let view_group = ctx.view_bind_group;
        let mut pass = ctx.scene_pass("world.terrain")?;
        let stats = shared.draw(&mut pass, view_group, &self.batcher, PassKind::Color, None)?;
        drop(pass);
        shared.mark_colored(ctx.view.id);
        ctx.record_draws(stats);
        self.status.update(|s| {
            s.draw_calls = stats.draw_calls;
            s.shadowed = shadowed;
        });
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
        let frame = shared.collect(ctx, &mut self.batcher, PassKind::Normals, None)?;
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
                color_attachments: &[Some(color_attachment(&normals))],
                depth_stencil_attachment: Some(depth_attachment(&depth, wgpu::LoadOp::Load)),
                ..Default::default()
            })
            .forget_lifetime();
        let stats: DrawStats = shared.draw(
            &mut pass,
            view_group,
            &self.batcher,
            PassKind::Normals,
            None,
        )?;
        drop(pass);
        ctx.record_draws(stats);
        self.status.update(|s| s.normals_pass = true);
        Ok(())
    }
}

impl PipelineFeature for TerrainShadowFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::TERRAIN_SHADOW, Stage::Shadow).after(engine_names::SHADOWS)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        lock(&self.shared)?.prepare(setup)
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let mut shared = lock(&self.shared)?;
        let views = ctx.shadow_views()?.to_vec();
        shared.set_shadowed(ctx.view.id, !views.is_empty());
        self.status.update(|s| {
            s.shadow_pass = false;
            s.shadow_draw_calls = 0;
        });
        let view_group = ctx.view_bind_group;
        let mut stats = DrawStats::default();
        let mut uploads = 0;
        for (tile, view) in views.iter().enumerate() {
            let frame = shared.collect(
                ctx,
                &mut self.batcher,
                PassKind::Shadow,
                Some(view.view.clip_from_world),
            )?;
            uploads += frame.uploads;
            if frame.nodes == 0 {
                continue;
            }
            self.batcher
                .finish(&ctx.gpu.device, &ctx.gpu.queue, Grouping::ByKey)?;
            let (indices, offset) = shared.shadow_index(view.index)?;
            let mut pass = ctx.shadow_pass("world.terrain.shadow", tile)?;
            stats += shared.draw(
                &mut pass,
                view_group,
                &self.batcher,
                PassKind::Shadow,
                Some((&indices, &[offset])),
            )?;
        }
        ctx.record_draws(stats);
        self.status.update(|s| {
            s.uploads += uploads;
            s.shadow_pass = !views.is_empty();
            s.shadow_draw_calls = stats.draw_calls;
        });
        Ok(())
    }
}

impl PipelineFeature for TerrainMotionFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::TERRAIN_MOTION, Stage::Opaque).after(names::TERRAIN)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        lock(&self.shared)?.prepare(setup)
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let mut shared = lock(&self.shared)?;
        // Only over terrain the colour pass drew this frame (its depth is the test).
        let colored = shared.take_colored(ctx.view.id);
        let Some(motion) = ctx.motion().map(|t| t.view.clone()) else {
            self.status.update(|s| s.motion_pass = false);
            return Ok(());
        };
        if !colored {
            self.status.update(|s| s.motion_pass = false);
            return Ok(());
        }
        let depth = ctx.depth()?.view.clone();
        let frame = shared.collect(ctx, &mut self.batcher, PassKind::Motion, None)?;
        if frame.nodes == 0 {
            self.status.update(|s| s.motion_pass = false);
            return Ok(());
        }
        self.batcher
            .finish(&ctx.gpu.device, &ctx.gpu.queue, Grouping::ByKey)?;
        let view_group = ctx.view_bind_group;
        let mut pass = ctx
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world.terrain.motion"),
                color_attachments: &[Some(color_attachment(&motion))],
                depth_stencil_attachment: Some(depth_attachment(&depth, wgpu::LoadOp::Load)),
                ..Default::default()
            })
            .forget_lifetime();
        let stats = shared.draw(&mut pass, view_group, &self.batcher, PassKind::Motion, None)?;
        drop(pass);
        ctx.record_draws(stats);
        self.status.update(|s| s.motion_pass = true);
        Ok(())
    }
}

fn color_attachment(view: &wgpu::TextureView) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        },
    }
}

fn depth_attachment(
    view: &wgpu::TextureView,
    load: wgpu::LoadOp<f32>,
) -> wgpu::RenderPassDepthStencilAttachment<'_> {
    wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load,
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    }
}

pub(crate) fn register(features: &mut PipelineFeatures) -> Result<TerrainRenderStatusHandle> {
    let status = TerrainRenderStatusHandle::default();
    TerrainFeature::all(status.clone()).register(features)?;
    Ok(status)
}

pub(crate) fn install(app: &mut EngineApp) {
    extract::install(app);
}
