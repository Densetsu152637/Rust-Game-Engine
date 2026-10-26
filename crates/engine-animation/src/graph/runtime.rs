use super::compile::{Node, valid_parameter};
use super::{Graph, GraphRigs};
use crate::{
    Clip, ClipEvent, LocalPose, Skeleton, Vec3, blend_poses, ik::TwoBone, retarget::RetargetMap,
};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// One event from a contributing clip. Shared clip nodes emit once per update.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphEvent {
    pub clip: String,
    pub event: ClipEvent,
}
#[derive(Clone)]
struct Fade {
    from: Vec<LocalPose>,
    elapsed: f32,
    duration: f32,
}
#[derive(Clone, Default)]
struct State {
    time: f32,
    active: usize,
    fade: Option<Fade>,
    displayed: Vec<LocalPose>,
}
/// Stateful evaluator. Graph topology and assets stay immutable; parameters are checked atomically.
pub struct GraphPlayer {
    graph: Arc<Graph>,
    rest: Vec<LocalPose>,
    skeleton: Option<Arc<Skeleton>>,
    solvers: Vec<Option<TwoBone>>,
    retargeters: Vec<Option<RetargetMap>>,
    clips: BTreeMap<String, Arc<Clip>>,
    parameters: BTreeMap<String, Value>,
    states: Vec<State>,
    pose: Vec<LocalPose>,
}
impl GraphPlayer {
    pub fn new(
        graph: Arc<Graph>,
        rest: Vec<LocalPose>,
        clips: BTreeMap<String, Arc<Clip>>,
    ) -> Result<Self> {
        Self::with_parameters(graph, rest, clips, &BTreeMap::new())
    }
    /// Applies and validates initial overrides before the first pose evaluation.
    pub fn with_parameters(
        graph: Arc<Graph>,
        rest: Vec<LocalPose>,
        clips: BTreeMap<String, Arc<Clip>>,
        parameters: &BTreeMap<String, Value>,
    ) -> Result<Self> {
        ensure!(
            !graph.requires_skeleton(),
            "Graph requires an explicit Skeleton"
        );
        Self::bound(graph, rest, clips, None, parameters, None)
    }
    /// Binds the complete validated hierarchy and its local rest pose for IK nodes.
    pub fn with_skeleton(
        graph: Arc<Graph>,
        skeleton: Arc<Skeleton>,
        clips: BTreeMap<String, Arc<Clip>>,
        parameters: &BTreeMap<String, Value>,
    ) -> Result<Self> {
        Self::bound(
            graph,
            skeleton.rest_pose().to_vec(),
            clips,
            Some(skeleton),
            parameters,
            None,
        )
    }
    /// Binds explicit target/source rig IDs without changing the established IK constructor.
    pub fn with_rigs(
        graph: Arc<Graph>,
        rigs: GraphRigs,
        clips: BTreeMap<String, Arc<Clip>>,
        parameters: &BTreeMap<String, Value>,
    ) -> Result<Self> {
        let target = rigs.target.clone();
        Self::bound(
            graph,
            target.rest_pose().to_vec(),
            clips,
            Some(target),
            parameters,
            Some(rigs),
        )
    }
    fn bound(
        graph: Arc<Graph>,
        rest: Vec<LocalPose>,
        clips: BTreeMap<String, Arc<Clip>>,
        skeleton: Option<Arc<Skeleton>>,
        parameters: &BTreeMap<String, Value>,
        rigs: Option<GraphRigs>,
    ) -> Result<Self> {
        ensure!(
            !rest.is_empty() && rest.len() <= 4096 && graph.nodes.len() * rest.len() <= 262144,
            "animation graph pose budget exceeded"
        );
        for pose in &rest {
            pose.validate()?;
        }
        for id in graph.clip_ids() {
            ensure!(clips.contains_key(id), "missing animation clip {id}");
        }
        for node in &graph.nodes {
            if let Node::Clip { id, .. } = node {
                ensure!(
                    clips[id].joint_count() == rest.len(),
                    "target clip joint count differs: {id}"
                );
            }
        }
        let retargeters = if let Some(rigs) = rigs {
            graph.bind_retargets(&rigs, &clips)?
        } else {
            ensure!(
                graph.retargets().next().is_none(),
                "Retarget graph requires named rig bindings"
            );
            vec![None; graph.nodes.len()]
        };
        let solvers = graph
            .nodes
            .iter()
            .map(|node| match node {
                Node::Ik { chain, limits, .. } => Ok(Some(TwoBone::new(
                    skeleton
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("IK requires a Skeleton"))?,
                    *chain,
                    *limits,
                )?)),
                _ => Ok(None),
            })
            .collect::<Result<Vec<_>>>()?;
        let states = graph
            .nodes
            .iter()
            .map(|node| State {
                active: if let Node::Machine { initial, .. } = node {
                    *initial
                } else {
                    0
                },
                ..Default::default()
            })
            .collect();
        let mut player = Self {
            parameters: graph.parameters.clone(),
            pose: rest.clone(),
            graph,
            rest,
            skeleton,
            solvers,
            retargeters,
            clips,
            states,
        };
        player.set_parameters(parameters)?;
        player.evaluate(0., false)?;
        Ok(player)
    }
    pub fn pose(&self) -> &[LocalPose] {
        &self.pose
    }
    pub fn parameters(&self) -> &BTreeMap<String, Value> {
        &self.parameters
    }
    /// Replaces overrides, resetting omitted variables to their authored defaults.
    pub fn set_parameters(&mut self, overrides: &BTreeMap<String, Value>) -> Result<()> {
        let mut values = self.graph.parameters.clone();
        for (name, value) in overrides {
            ensure!(
                self.graph
                    .parameters
                    .get(name)
                    .is_some_and(|default| valid_parameter(default, value)),
                "unknown or mistyped animation parameter {name}"
            );
            values.insert(name.clone(), value.clone());
        }
        for node in &self.graph.nodes {
            if let Node::Ik { weight, .. } = node {
                ensure!(
                    values[weight]
                        .as_f64()
                        .is_some_and(|v| (0.0..=1.).contains(&v)),
                    "IK weight must be in 0..1"
                );
            }
        }
        self.parameters = values;
        Ok(())
    }
    /// Transactional pose/time update; invalid output or event overflow retains the previous state.
    pub fn update(&mut self, dt: f32) -> Result<Vec<GraphEvent>> {
        ensure!(
            dt.is_finite() && (0.0..=60.).contains(&dt),
            "invalid animation timestep"
        );
        self.evaluate(dt, true)
    }
    fn evaluate(&mut self, dt: f32, transition: bool) -> Result<Vec<GraphEvent>> {
        let mut states = self.states.clone();
        let mut poses: Vec<Vec<LocalPose>> = vec![Vec::new(); self.graph.nodes.len()];
        let mut contributing: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); self.graph.nodes.len()];
        let number = |name: &str| {
            self.parameters[name]
                .as_f64()
                .expect("compiled float parameter") as f32
        };
        for &i in &self.graph.order {
            let mut inputs = Vec::new();
            let pose = match &self.graph.nodes[i] {
                Node::Output(input) => {
                    inputs.push((*input, 1.));
                    poses[*input].clone()
                }
                Node::Clip { id, speed, looping } => {
                    let pose = sample(
                        &self.clips[id],
                        &mut states[i],
                        dt,
                        *speed,
                        *looping,
                        &self.rest,
                    )?;
                    contributing[i].insert(i);
                    pose
                }
                Node::RetargetClip(spec) => {
                    let map = self.retargeters[i]
                        .as_ref()
                        .expect("validated retarget binding");
                    let source = sample(
                        &self.clips[&spec.clip],
                        &mut states[i],
                        dt,
                        spec.speed,
                        spec.looping,
                        map.source().rest_pose(),
                    )?;
                    let pose = map.apply(&source)?;
                    contributing[i].insert(i);
                    pose
                }
                Node::Blend { a, b, parameter } => {
                    let weight = number(parameter).clamp(0., 1.);
                    inputs.extend([(*a, 1. - weight), (*b, weight)]);
                    blend_poses(&poses[*a], &poses[*b], weight)?
                }
                Node::One {
                    inputs: samples,
                    parameter,
                    space,
                } => {
                    inputs.extend(
                        samples
                            .iter()
                            .copied()
                            .zip(space.weights(number(parameter))?),
                    );
                    weighted_pose(&poses, &inputs)?
                }
                Node::Two {
                    inputs: samples,
                    x,
                    y,
                    space,
                } => {
                    inputs.extend(
                        samples
                            .iter()
                            .copied()
                            .zip(space.weights([number(x), number(y)])?),
                    );
                    weighted_pose(&poses, &inputs)?
                }
                Node::Ik {
                    input,
                    target,
                    pole,
                    weight,
                    ..
                } => {
                    inputs.push((*input, 1.));
                    let vector = |name: &str| {
                        Vec3::from_array(std::array::from_fn(|i| {
                            self.parameters[name][i]
                                .as_f64()
                                .expect("validated vec3 parameter")
                                as f32
                        }))
                    };
                    self.solvers[i].as_ref().expect("bound IK solver").solve(
                        self.skeleton.as_deref().expect("bound IK skeleton"),
                        &poses[*input],
                        vector(target),
                        vector(pole),
                        number(weight),
                    )?
                }
                Node::Machine {
                    inputs: samples,
                    transitions,
                    ..
                } => {
                    let state = &mut states[i];
                    if transition
                        && let Some(t) = transitions
                            .iter()
                            .find(|t| t.from == state.active && t.matches(&self.parameters))
                    {
                        state.active = t.to;
                        state.fade = if t.duration > 0. {
                            Some(Fade {
                                from: state.displayed.clone(),
                                elapsed: 0.,
                                duration: t.duration,
                            })
                        } else {
                            None
                        };
                    }
                    let target = samples[state.active];
                    inputs.push((target, 1.));
                    if let Some(fade) = &mut state.fade {
                        fade.elapsed = (fade.elapsed + dt).min(fade.duration);
                        let pose =
                            blend_poses(&fade.from, &poses[target], fade.elapsed / fade.duration)?;
                        if fade.elapsed >= fade.duration {
                            state.fade = None;
                        }
                        pose
                    } else {
                        poses[target].clone()
                    }
                }
            };
            for (input, weight) in inputs {
                if weight > 0. {
                    let sources = contributing[input].clone();
                    contributing[i].extend(sources);
                }
            }
            if matches!(self.graph.nodes[i], Node::Machine { .. }) {
                states[i].displayed = pose.clone();
            }
            poses[i] = pose;
        }
        let mut events = Vec::new();
        for &i in &contributing[self.graph.output] {
            if let Some((id, speed, looping)) = self.graph.nodes[i].playback() {
                for event in self.clips[id].events_between(
                    self.states[i].time,
                    self.states[i].time + dt * speed,
                    looping,
                )? {
                    ensure!(
                        events.len() < 4096,
                        "animation graph event capacity exceeded"
                    );
                    events.push(GraphEvent {
                        clip: id.to_owned(),
                        event,
                    });
                }
            }
        }
        self.states = states;
        self.pose = std::mem::take(&mut poses[self.graph.output]);
        Ok(events)
    }
}
fn weighted_pose(poses: &[Vec<LocalPose>], inputs: &[(usize, f32)]) -> Result<Vec<LocalPose>> {
    let mut total = 0.;
    let mut result: Option<Vec<LocalPose>> = None;
    for &(input, weight) in inputs {
        if weight > 0. {
            total += weight;
            result = Some(if let Some(current) = result {
                blend_poses(&current, &poses[input], weight / total)?
            } else {
                poses[input].clone()
            });
        }
    }
    result.ok_or_else(|| anyhow::anyhow!("blend has no positive sample weight"))
}

fn sample(
    clip: &Clip,
    state: &mut State,
    dt: f32,
    speed: f32,
    looping: bool,
    rest: &[LocalPose],
) -> Result<Vec<LocalPose>> {
    let next = state.time + dt * speed;
    ensure!(next.is_finite(), "animation clip clock overflow");
    let pose = clip.sample(next, looping, rest)?;
    state.time = if looping {
        next.rem_euclid(clip.duration())
    } else {
        next.min(clip.duration())
    };
    Ok(pose)
}
