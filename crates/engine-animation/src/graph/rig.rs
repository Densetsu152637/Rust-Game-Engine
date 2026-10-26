//! Explicit named rig bindings and deterministic asset dependencies for retarget sources.
use super::{Graph, compile::Node};
use crate::{
    Clip, Skeleton,
    retarget::{RetargetDefinition, RetargetMap},
};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Debug)]
pub struct RetargetClip {
    pub clip: String,
    pub source_rig: String,
    pub target_rig: String,
    pub mapping: RetargetDefinition,
    pub speed: f32,
    pub looping: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphAssetKind {
    Animation,
    Model,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct GraphDependency<'a> {
    pub id: &'a str,
    pub kind: GraphAssetKind,
}
#[derive(Clone, Debug)]
pub struct GraphRigs {
    pub(super) target_id: String,
    pub(super) target: Arc<Skeleton>,
    pub(super) sources: BTreeMap<String, Arc<Skeleton>>,
}
impl GraphRigs {
    pub fn new(
        target_id: String,
        target: Arc<Skeleton>,
        sources: BTreeMap<String, Arc<Skeleton>>,
    ) -> Result<Self> {
        let valid =
            |id: &str| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control);
        ensure!(
            valid(&target_id)
                && sources.len() <= 16
                && sources.keys().all(|id| valid(id))
                && sources.values().map(|rig| rig.joint_count()).sum::<usize>() <= 65536,
            "retarget rig binding limit or invalid rig identifier"
        );
        Ok(Self {
            target_id,
            target,
            sources,
        })
    }
}
impl Graph {
    pub fn retargets(&self) -> impl Iterator<Item = &RetargetClip> {
        self.nodes.iter().filter_map(|node| {
            if let Node::RetargetClip(source) = node {
                Some(source)
            } else {
                None
            }
        })
    }
    pub fn source_rig_ids(&self) -> impl Iterator<Item = &str> {
        self.retargets()
            .map(|node| node.source_rig.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
    }
    pub fn rig_ids(&self) -> impl Iterator<Item = &str> {
        self.retargets()
            .flat_map(|node| [node.source_rig.as_str(), node.target_rig.as_str()])
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
    }
    /// Sorted by asset kind then ID, with no duplicates; at most 273 entries.
    pub fn asset_dependencies(&self) -> Vec<GraphDependency<'_>> {
        self.clip_ids()
            .map(|id| GraphDependency {
                id,
                kind: GraphAssetKind::Animation,
            })
            .chain(self.rig_ids().map(|id| GraphDependency {
                id,
                kind: GraphAssetKind::Model,
            }))
            .collect()
    }
    /// Validates named rigs, map topology and clip counts without evaluating parameter defaults.
    pub fn validate_retarget_bindings(
        &self,
        rigs: &GraphRigs,
        clips: &BTreeMap<String, Arc<Clip>>,
    ) -> Result<()> {
        self.bind_retargets(rigs, clips).map(|_| ())
    }
    pub(super) fn bind_retargets(
        &self,
        rigs: &GraphRigs,
        clips: &BTreeMap<String, Arc<Clip>>,
    ) -> Result<Vec<Option<RetargetMap>>> {
        self.nodes
            .iter()
            .map(|node| match node {
                Node::RetargetClip(spec) => {
                    ensure!(
                        spec.target_rig == rigs.target_id,
                        "retarget target rig differs from bound graph rig"
                    );
                    let source = rigs.sources.get(&spec.source_rig).ok_or_else(|| {
                        anyhow::anyhow!("missing retarget source rig {}", spec.source_rig)
                    })?;
                    ensure!(
                        clips
                            .get(&spec.clip)
                            .is_some_and(|clip| clip.joint_count() == source.joint_count()),
                        "retarget clip/source rig joint count differs"
                    );
                    Ok(Some(RetargetMap::new(
                        source.clone(),
                        rigs.target.clone(),
                        &spec.mapping,
                    )?))
                }
                Node::Clip { id, .. } => {
                    ensure!(
                        clips
                            .get(id)
                            .is_some_and(|clip| clip.joint_count() == rigs.target.joint_count()),
                        "target clip/rig joint count differs"
                    );
                    Ok(None)
                }
                _ => Ok(None),
            })
            .collect()
    }
}
