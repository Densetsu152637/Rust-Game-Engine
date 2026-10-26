//! Ordered presentation producers and stage execution. Painter keys are
//! supplied by producers; this module never derives order from geometry.
use crate::context::FrameIo;
use crate::feature::{Activity, FeatureBody, FeatureRegistry, ResolvedFeatures};
use crate::instancing::DrawStats;
use crate::{FeatureContext, HostStages, PipelineLayouts, Stage, ViewData};
use anyhow::{Context, Result, ensure};
use bevy_ecs::world::World;
use engine_render_wgpu::GpuContext;
use std::collections::BTreeSet;

/// Producer-defined painter order. World draws precede UI; ties use layer
/// and a stable authored paint ID (which may differ from a cache identity).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OverlayOrderKey {
    pub ui: bool,
    pub layer: i32,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayItem {
    pub order: OverlayOrderKey,
    /// Opaque producer-local token, valid for this output's retained snapshot.
    pub token: u64,
}

/// One contiguous run of a producer in the merged painter stream. A source
/// is initialized/finished once even if it supplied no visible items.
#[derive(Clone, Copy, Debug)]
pub struct OverlayBatch<'a> {
    pub tokens: &'a [u64],
    pub first: bool,
    pub last: bool,
}

fn painter_order(sources: &[Option<Vec<OverlayItem>>]) -> Result<Vec<(usize, u64)>> {
    let mut items = Vec::new();
    let mut keys = BTreeSet::new();
    for (source, entries) in sources.iter().enumerate() {
        let mut tokens = BTreeSet::new();
        if let Some(entries) = entries {
            for item in entries {
                ensure!(
                    keys.insert(item.order.clone()),
                    "duplicate ordered overlay paint key {:?}",
                    item.order
                );
                ensure!(
                    tokens.insert(item.token),
                    "duplicate ordered overlay token {} in producer {source}",
                    item.token
                );
                items.push((item.order.clone(), source, item.token));
            }
        }
    }
    ensure!(
        items.len() <= 1 << 20,
        "ordered overlay exceeds 1048576 items"
    );
    items.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(items
        .into_iter()
        .map(|(_, source, token)| (source, token))
        .collect())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_presentation(
    stage: Stage,
    gpu: &GpuContext,
    layouts: &PipelineLayouts,
    registry: &mut FeatureRegistry,
    view_bind_group: &wgpu::BindGroup,
    view: &ViewData,
    world: Option<&World>,
    encoder: &mut wgpu::CommandEncoder,
    io: &mut FrameIo,
    resolved: &mut ResolvedFeatures,
    stats: &mut DrawStats,
    host: &mut dyn HostStages,
) -> Result<()> {
    let order: Vec<usize> = registry.ordered().to_vec();
    let mut sources = vec![None; registry.features.len()];
    if stage == Stage::Overlay {
        for &index in &order {
            let registered = &mut registry.features[index];
            let name = &registered.descriptor.name;
            if registered.descriptor.stage != stage || !resolved.is_enabled(name) {
                continue;
            }
            sources[index] = match &mut registered.body {
                FeatureBody::Host => host.overlay_items(name, view)?,
                FeatureBody::Owned(feature) => feature.overlay_items(view)?,
                _ => None,
            };
        }
    }
    let painter = painter_order(&sources)?;
    let anchor = order
        .iter()
        .copied()
        .find(|&i| {
            registry.features[i].descriptor.name == crate::names::SPRITES && sources[i].is_some()
        })
        .or_else(|| order.iter().copied().find(|&i| sources[i].is_some()));
    let mut encoded = vec![0; sources.len()];
    for index in order {
        let descriptor = &registry.features[index].descriptor;
        if descriptor.stage != stage || !resolved.is_enabled(&descriptor.name) {
            continue;
        }
        if sources[index].is_some() {
            if Some(index) != anchor {
                continue;
            }
            flush(gpu, encoder);
            host.begin_overlay(gpu, io.output.as_ref().context("no output target")?)?;
            // Empty producers still receive one complete batch, before items.
            for (source, entries) in sources.iter().enumerate() {
                if entries.as_ref().is_some_and(Vec::is_empty) {
                    encode_source(
                        source,
                        OverlayBatch {
                            tokens: &[],
                            first: true,
                            last: true,
                        },
                        stage,
                        gpu,
                        layouts,
                        registry,
                        view_bind_group,
                        view,
                        world,
                        encoder,
                        io,
                        stats,
                        host,
                    )?;
                }
            }
            let mut start = 0;
            while start < painter.len() {
                let source = painter[start].0;
                let end = (start + 1..painter.len())
                    .find(|&i| painter[i].0 != source)
                    .unwrap_or(painter.len());
                let tokens: Vec<u64> = painter[start..end].iter().map(|p| p.1).collect();
                let first = encoded[source] == 0;
                encoded[source] += tokens.len();
                let last =
                    encoded[source] == sources[source].as_ref().expect("ordered source").len();
                encode_source(
                    source,
                    OverlayBatch {
                        tokens: &tokens,
                        first,
                        last,
                    },
                    stage,
                    gpu,
                    layouts,
                    registry,
                    view_bind_group,
                    view,
                    world,
                    encoder,
                    io,
                    stats,
                    host,
                )?;
                start = end;
            }
            continue;
        }
        let registered = &mut registry.features[index];
        let name = registered.descriptor.name.clone();
        match &mut registered.body {
            FeatureBody::Host => {
                flush(gpu, encoder);
                host.overlay(&name, gpu, io.output.as_ref().context("no output target")?)
                    .with_context(|| format!("host overlay {name}"))?;
            }
            FeatureBody::Owned(feature) => {
                if stage == Stage::Composite && !feature.composite_active(view) {
                    resolved.set_activity(&name, Activity::Idle);
                    continue;
                }
                let mut context = FeatureContext {
                    gpu,
                    encoder,
                    stage,
                    view,
                    view_bind_group,
                    layouts,
                    world,
                    shadows: None,
                    io,
                    stats,
                    name: &name,
                };
                feature
                    .encode(&mut context)
                    .with_context(|| format!("render feature {name}"))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn flush(gpu: &GpuContext, encoder: &mut wgpu::CommandEncoder) {
    let finished = std::mem::replace(
        encoder,
        gpu.device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("engine presentation"),
            }),
    );
    gpu.queue.submit([finished.finish()]);
}

#[allow(clippy::too_many_arguments)]
fn encode_source(
    index: usize,
    batch: OverlayBatch<'_>,
    stage: Stage,
    gpu: &GpuContext,
    layouts: &PipelineLayouts,
    registry: &mut FeatureRegistry,
    view_bind_group: &wgpu::BindGroup,
    view: &ViewData,
    world: Option<&World>,
    encoder: &mut wgpu::CommandEncoder,
    io: &mut FrameIo,
    stats: &mut DrawStats,
    host: &mut dyn HostStages,
) -> Result<()> {
    let registered = &mut registry.features[index];
    let name = registered.descriptor.name.clone();
    match &mut registered.body {
        FeatureBody::Host => {
            flush(gpu, encoder);
            host.overlay_batch(
                &name,
                gpu,
                io.output.as_ref().context("no output target")?,
                batch,
            )
            .with_context(|| format!("host overlay {name}"))?;
        }
        FeatureBody::Owned(feature) => {
            let mut context = FeatureContext {
                gpu,
                encoder,
                stage,
                view,
                view_bind_group,
                layouts,
                world,
                shadows: None,
                io,
                stats,
                name: &name,
            };
            feature
                .encode_overlay(&mut context, batch)
                .with_context(|| format!("ordered overlay {name}"))?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(ui: bool, layer: i32, id: &str, token: u64) -> OverlayItem {
        OverlayItem {
            order: OverlayOrderKey {
                ui,
                layer,
                id: id.into(),
            },
            token,
        }
    }
    #[test]
    fn painter_merges_producers_without_grouping_away_authored_order() {
        let sources = [
            Some(vec![item(true, 2, "a:0", 0), item(true, 2, "a:3", 1)]),
            Some(vec![item(true, 2, "a:2", 9)]),
            Some(vec![item(false, 100, "world", 5)]),
        ];
        assert_eq!(
            painter_order(&sources).unwrap(),
            [(2, 5), (0, 0), (1, 9), (0, 1)]
        );
    }
    #[test]
    fn duplicate_paint_keys_and_tokens_are_rejected() {
        assert!(
            painter_order(&[
                Some(vec![item(true, 0, "same", 0)]),
                Some(vec![item(true, 0, "same", 1)])
            ])
            .is_err()
        );
        assert!(
            painter_order(&[Some(vec![item(true, 0, "a", 7), item(true, 0, "b", 7)])]).is_err()
        );
    }
}
