//! Conversion and bounded device-local caching of authored indirect scenes.
use anyhow::{Context, Result, ensure};
use engine_project::{indirect::IndirectAsset, rendering3d::SceneRenderFrame};
use engine_render_pipeline::{
    GpuContext, ViewId,
    indirect::{
        BrdfLut, DiffusePolynomial, EnvironmentData, EnvironmentLighting, GpuIndirectResources,
        IndirectProbe, IndirectScene, ProbeVolume, RadianceImage,
    },
};
use glam::{Mat4, Quat, Vec3};
use std::{collections::HashMap, sync::Arc};

const MAX_RESIDENT: u64 = 128 * 1024 * 1024;
const LUT_BYTES: u64 = 64 * 64 * 8;

/// A named budget failure callers can report without mistaking fallback for IBL.
#[derive(Debug)]
pub(crate) struct ActiveIndirectBudgetExceeded {
    pub used: u64,
    pub requested: u64,
}
impl std::fmt::Display for ActiveIndirectBudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "authored indirect active-view GPU budget exceeds 128 MiB (used {}, requested {} bytes)",
            self.used, self.requested
        )
    }
}
impl std::error::Error for ActiveIndirectBudgetExceeded {}

struct Prepared {
    _source: Arc<IndirectAsset>,
    resources: Arc<GpuIndirectResources>,
    resident_bytes: u64,
}
struct ViewScene {
    source: usize,
    origin: [f64; 3],
    scene: Arc<IndirectScene>,
}

/// One renderer/device owns this cache. Resolve is transactional with respect
/// to per-view state: call commit only after ScenePipeline::set_indirect succeeds.
#[derive(Default)]
pub(crate) struct AuthoredIndirectCache {
    sources: HashMap<usize, Prepared>,
    views: HashMap<ViewId, ViewScene>,
    lut: Option<BrdfLut>,
}

impl AuthoredIndirectCache {
    pub(crate) fn resolve(
        &mut self,
        gpu: &GpuContext,
        view: ViewId,
        frame: &SceneRenderFrame,
    ) -> Result<Option<Arc<IndirectScene>>> {
        let Some(source) = frame.indirect.as_ref() else {
            return Ok(None);
        };
        let key = Arc::as_ptr(source) as usize;
        if let Some(old) = self.views.get(&view) {
            if old.source == key && old.origin == frame.applied_origin {
                return Ok(Some(old.scene.clone()));
            }
        }
        // Only unreferenced resources may be removed. A replacement view's old
        // scene remains alive until set_indirect and commit have both succeeded.
        self.sources
            .retain(|id, _| self.views.values().any(|v| v.source == *id));
        if !self.sources.contains_key(&key) {
            let resident_bytes = source
                .texture_bytes
                .checked_add(LUT_BYTES)
                .context("indirect resident size overflow")?;
            let used: u64 = self.sources.values().map(|item| item.resident_bytes).sum();
            if used
                .checked_add(resident_bytes)
                .is_none_or(|n| n > MAX_RESIDENT)
            {
                return Err(ActiveIndirectBudgetExceeded {
                    used,
                    requested: resident_bytes,
                }
                .into());
            }
            let data = source
                .environments
                .iter()
                .map(|environment| -> Result<EnvironmentData> {
                    let diffuse = DiffusePolynomial::new(environment.diffuse)?;
                    let mips = environment
                        .mips
                        .iter()
                        .map(|faces| -> Result<[RadianceImage; 6]> {
                            faces
                                .iter()
                                .map(|face| RadianceImage::new([face.size; 2], face.pixels.clone()))
                                .collect::<Result<Vec<_>>>()?
                                .try_into()
                                .map_err(|_| anyhow::anyhow!("six cube faces required"))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    EnvironmentData::new(diffuse, mips)
                })
                .collect::<Result<Vec<_>>>()?;
            if self.lut.is_none() {
                self.lut = Some(BrdfLut::prepare(64, 256)?);
            }
            let resources = Arc::new(GpuIndirectResources::prepare(
                gpu,
                &data,
                &[],
                self.lut.as_ref().expect("prepared above"),
            )?);
            self.sources.insert(
                key,
                Prepared {
                    _source: source.clone(),
                    resources,
                    resident_bytes,
                },
            );
        }
        let prepared = &self.sources[&key];
        let global = source.global.as_ref().map(lighting).transpose()?;
        let probes = source
            .probes
            .iter()
            .map(|probe| -> Result<IndirectProbe> {
                let local = probe_local(probe.position_absolute, frame.applied_origin)?;
                let rotation = quat(probe.lighting.rotation);
                let world_from_probe = Mat4::from_scale_rotation_translation(
                    Vec3::from_array(probe.scale),
                    rotation,
                    Vec3::from_array(local),
                );
                Ok(IndirectProbe {
                    volume: ProbeVolume::new(
                        probe.id.clone(),
                        world_from_probe,
                        Vec3::from_array(probe.half_extents),
                        probe.blend_distance,
                        probe.priority,
                    )?,
                    lighting: lighting(&probe.lighting)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Arc::new(IndirectScene::new(
            prepared.resources.clone(),
            global,
            probes,
        )?)))
    }

    pub(crate) fn commit(
        &mut self,
        view: ViewId,
        frame: &SceneRenderFrame,
        scene: Option<Arc<IndirectScene>>,
    ) {
        if let (Some(source), Some(scene)) = (&frame.indirect, scene) {
            self.views.insert(
                view,
                ViewScene {
                    source: Arc::as_ptr(source) as usize,
                    origin: frame.applied_origin,
                    scene,
                },
            );
        } else {
            self.views.remove(&view);
        }
        self.sources
            .retain(|id, _| self.views.values().any(|v| v.source == *id));
    }

    pub(crate) fn forget_view(&mut self, view: ViewId) {
        self.views.remove(&view);
        self.sources
            .retain(|id, _| self.views.values().any(|v| v.source == *id));
    }
}

fn quat(q: [f32; 4]) -> Quat {
    Quat::from_xyzw(q[0], q[1], q[2], q[3])
}
fn probe_local(absolute: [f64; 3], applied_origin: [f64; 3]) -> Result<[f32; 3]> {
    let local = std::array::from_fn(|i| (absolute[i] - applied_origin[i]) as f32);
    ensure!(
        local.iter().all(|v| v.is_finite()),
        "indirect probe position cannot be represented in render coordinates"
    );
    Ok(local)
}

#[cfg(test)]
mod tests {
    use super::probe_local;

    #[test]
    fn absolute_probe_is_subtracted_in_f64_before_local_rounding() {
        let absolute = [999_999_999_000.25, 0.0, 0.0];
        assert_eq!(
            probe_local(absolute, [999_999_998_000.0, 0.0, 0.0]).unwrap()[0],
            1000.25
        );
        assert_eq!(
            probe_local(absolute, [999_999_999_000.0, 0.0, 0.0]).unwrap()[0],
            0.25
        );
    }
}
fn lighting(
    value: &engine_project::indirect::EnvironmentLightingAsset,
) -> Result<EnvironmentLighting> {
    EnvironmentLighting::new(value.environment, quat(value.rotation), value.intensity)
}
