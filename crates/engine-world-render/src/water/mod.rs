//! `world.water` and `world.water.underwater`: GPU water surfaces and underwater fog.
//!
//! `world.water` (`Transparent` stage, before `engine.transparent`) copies the HDR colour and
//! depth of the opaque scene, then draws every body of
//! `engine_project::world::water::WaterRenderSet` as one grid (oceans and lakes) or ribbon
//! (rivers) draw: the vertex shader displaces the grid analytically (Gerstner) or from the
//! uploaded FFT ocean frame (`Rgba32Float` displacement and normal textures, periodic
//! bilinear lookups), scaled by the per-vertex shoreline attenuation; the fragment shader
//! refracts the copied scene colour, absorbs it with Beer-Lambert extinction over the water
//! thickness from the copied depth, adds in-scattered shallow/deep colour, a Schlick Fresnel
//! reflection of the ambient sky and the sun highlight, and crest plus shoreline foam (scene
//! depth closer than `foam_depth` below the surface). The surface writes depth.
//!
//! `world.water.underwater` (`Transparent` stage, after `engine.transparent` and
//! `world.water`) checks the view camera against the CPU surfaces and, when it is below one,
//! fogs every pixel by its distance: `mix(fog, scene exp(-absorption d), exp(-density d))`.
//! It is a Transparent-stage pass instead of a PostProcess one because it needs render-size
//! depth and the pre-upscale HDR colour (temporal upscalers then see fogged frames, and the
//! fog works at any upscale ratio).
mod extract;
mod gpu;

pub use extract::extract_water;
pub use gpu::{GPU_WAVES, camera_underwater};

use crate::names;
use anyhow::Result;
use engine_core::EngineApp;
use engine_render_pipeline::{
    FeatureContext, FeatureDescriptor, FeatureSetup, PipelineFeature, PipelineFeatures, Stage,
    names as engine_names,
};
use gpu::Shared;
use std::sync::{Arc, Mutex};

/// What the water features did in the last view they ran for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WaterRenderStatus {
    /// Views encoded since registration (surface pass).
    pub views: u64,
    /// Water bodies drawn in the last view (one draw call each).
    pub bodies: usize,
    pub draw_calls: usize,
    /// Ocean frame and shore grid texture uploads since registration.
    pub uploads: u64,
    /// Views fogged by the underwater pass since registration, and the body the last view's
    /// camera was in (`None` above water).
    pub underwater_views: u64,
    pub underwater: Option<String>,
    pub notes: Vec<String>,
}

/// Shared handle to [`WaterRenderStatus`].
#[derive(Clone, Debug, Default)]
pub struct WaterRenderStatusHandle(Arc<Mutex<WaterRenderStatus>>);
impl WaterRenderStatusHandle {
    pub fn get(&self) -> WaterRenderStatus {
        self.0.lock().map(|s| s.clone()).unwrap_or_default()
    }
    fn update(&self, change: impl FnOnce(&mut WaterRenderStatus)) {
        if let Ok(mut status) = self.0.lock() {
            change(&mut status);
        }
    }
}

fn lock(shared: &Mutex<Shared>) -> Result<std::sync::MutexGuard<'_, Shared>> {
    shared
        .lock()
        .map_err(|_| anyhow::anyhow!("world.water state poisoned"))
}

/// The surface pass (`world.water`).
pub struct WaterFeature {
    shared: Arc<Mutex<Shared>>,
    status: WaterRenderStatusHandle,
}
/// Opt-in opaque-depth water caster for the shared engine shadow atlas.
pub struct WaterShadowFeature {
    shared: Arc<Mutex<Shared>>,
    status: WaterRenderStatusHandle,
}
/// The underwater fog pass (`world.water.underwater`).
pub struct UnderwaterFeature {
    shared: Arc<Mutex<Shared>>,
    status: WaterRenderStatusHandle,
}

impl WaterFeature {
    /// The surface and underwater features over one shared GPU cache.
    pub fn pair(status: WaterRenderStatusHandle) -> (WaterFeature, UnderwaterFeature) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        (
            WaterFeature {
                shared: shared.clone(),
                status: status.clone(),
            },
            UnderwaterFeature { shared, status },
        )
    }
}

impl PipelineFeature for WaterFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::WATER, Stage::Transparent).before(engine_names::TRANSPARENT)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        lock(&self.shared)?.prepare(setup)
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let frame = lock(&self.shared)?.draw_water(ctx)?;
        ctx.record_draws(engine_render_pipeline::DrawStats {
            draw_calls: frame.bodies,
            instances: frame.bodies,
        });
        self.status.update(|s| {
            s.views += 1;
            s.bodies = frame.bodies;
            s.draw_calls = frame.bodies;
            s.uploads += frame.uploads;
            s.notes = frame.notes;
        });
        Ok(())
    }
}

impl PipelineFeature for WaterShadowFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::WATER_SHADOW, Stage::Shadow).after(engine_names::SHADOWS)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        lock(&self.shared)?.prepare(setup)
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let (frame, stats) = lock(&self.shared)?.draw_shadow(ctx)?;
        ctx.record_draws(stats);
        self.status.update(|s| s.uploads += frame.uploads);
        Ok(())
    }
}

impl PipelineFeature for UnderwaterFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(names::WATER_UNDERWATER, Stage::Transparent)
            .after(engine_names::TRANSPARENT)
            .after(names::WATER)
    }
    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        // The surface feature prepares the shared state; prepare again only if it has not.
        let mut shared = lock(&self.shared)?;
        if !shared.is_prepared() {
            shared.prepare(setup)?;
        }
        Ok(())
    }
    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let fogged = lock(&self.shared)?.draw_underwater(ctx)?;
        self.status.update(|s| {
            s.underwater_views += u64::from(fogged.is_some());
            s.underwater = fogged;
        });
        Ok(())
    }
}

/// Registers `world.water` and `world.water.underwater`; the handle reports what they drew.
pub fn register(features: &mut PipelineFeatures) -> Result<WaterRenderStatusHandle> {
    let status = WaterRenderStatusHandle::default();
    let (surface, underwater) = WaterFeature::pair(status.clone());
    features.register(WaterShadowFeature {
        shared: surface.shared.clone(),
        status: status.clone(),
    })?;
    features.register(surface)?;
    features.register(underwater)?;
    Ok(status)
}

pub(crate) fn install(app: &mut EngineApp) {
    extract::install(app);
}
