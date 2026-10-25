//! Dimension-generic debug view: collider, contact and anchor data plus the wireframe
//! builder. `super` is the dimension's debug module (which declares [`DebugShape`] and the
//! shape-specific geometry) and `super::super` the dimension module.
//!
//! The view is plain data: no rendering types, only world-space numbers a renderer or the
//! editor can consume. Every list has an explicit cap (see [`DebugOptions`]) and the view
//! records the uncapped totals plus a `truncated` flag.
use super::super::{Extension, Orientation, Position, World, body_pose};
use super::{DebugShape, local_segments, to_world};
use crate::{Error, Result};
use std::cmp::Ordering;

/// Largest cap accepted for any [`DebugOptions`] item list.
pub const MAX_DEBUG_ITEMS: usize = 65_536;
/// Bounds and toggles for [`World::debug_view`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugOptions {
    /// Most colliders listed (lowest ids first).
    pub max_colliders: usize,
    /// Most contact points listed (sorted by body ids, then position).
    pub max_contacts: usize,
    /// Most joint anchors and, separately, most contributed lines.
    pub max_anchors: usize,
    /// Include each collider's world-space AABB.
    pub aabbs: bool,
    pub contacts: bool,
    pub joints: bool,
}
impl Default for DebugOptions {
    fn default() -> Self {
        Self {
            max_colliders: 4096,
            max_contacts: 4096,
            max_anchors: 4096,
            aabbs: false,
            contacts: true,
            joints: true,
        }
    }
}
impl DebugOptions {
    fn validate(self) -> Result<()> {
        for cap in [self.max_colliders, self.max_contacts, self.max_anchors] {
            if cap > MAX_DEBUG_ITEMS {
                return Err(Error("debug view caps are at most 65536"));
            }
        }
        Ok(())
    }
}
/// Colour key of a collider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugState {
    Fixed,
    Dynamic,
    Kinematic,
    /// A dynamic body Rapier put to sleep.
    Sleeping,
    /// Sensor colliders, whatever their body kind.
    Sensor,
}
impl DebugState {
    pub fn key(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Dynamic => "dynamic",
            Self::Kinematic => "kinematic",
            Self::Sleeping => "sleeping",
            Self::Sensor => "sensor",
        }
    }
    /// Default RGBA colour for this state.
    pub fn color(self) -> [f32; 4] {
        match self {
            Self::Fixed => [0.55, 0.57, 0.62, 1.0],
            Self::Dynamic => [0.25, 0.85, 0.35, 1.0],
            Self::Kinematic => [0.30, 0.55, 1.0, 1.0],
            Self::Sleeping => [0.60, 0.45, 0.85, 1.0],
            Self::Sensor => [1.0, 0.80, 0.20, 0.8],
        }
    }
}
/// Colours of contact normal lines, joint anchor crosses and collider AABBs.
pub const CONTACT_COLOR: [f32; 4] = [1.0, 0.25, 0.25, 1.0];
pub const ANCHOR_COLOR: [f32; 4] = [1.0, 0.55, 0.0, 1.0];
pub const AABB_COLOR: [f32; 4] = [0.9, 0.9, 0.9, 0.35];
/// Length of a drawn contact normal and half-size of an anchor cross, in meters.
const MARKER_SIZE: f32 = 0.15;

#[derive(Clone, Debug, PartialEq)]
pub struct DebugCollider {
    pub id: u64,
    pub shape: DebugShape,
    pub position: Position,
    pub rotation: Orientation,
    pub state: DebugState,
    /// World-space `[min, max]` when [`DebugOptions::aabbs`] is set.
    pub aabb: Option<[Position; 2]>,
}
/// One contact point. `normal` points from the first body toward the second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugContact {
    pub first: u64,
    pub second: u64,
    pub point: Position,
    pub normal: Position,
    pub depth: f32,
}
/// A marked point attached to a body: a joint anchor or a feature-contributed marker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugAnchor {
    pub owner: u64,
    pub kind: &'static str,
    pub position: Position,
}
/// A world-space coloured line segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugLine {
    pub from: Position,
    pub to: Position,
    pub color: [f32; 4],
}
/// Uncapped item counts of a [`DebugView`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebugTotals {
    pub colliders: usize,
    pub contacts: usize,
    pub anchors: usize,
    pub lines: usize,
}
/// Bounded snapshot of a world for visualisation.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugView {
    pub colliders: Vec<DebugCollider>,
    pub contacts: Vec<DebugContact>,
    pub anchors: Vec<DebugAnchor>,
    /// Lines pushed by feature contributors (see [`World::add_debug_contributor`]).
    pub lines: Vec<DebugLine>,
    pub totals: DebugTotals,
    /// True when any list was cut by its cap.
    pub truncated: bool,
}
/// Lines produced by [`DebugView::wireframe`].
#[derive(Clone, Debug, PartialEq)]
pub struct DebugLines {
    pub lines: Vec<DebugLine>,
    pub truncated: bool,
}

/// Collector handed to debug contributors. Items beyond the view's caps are counted, not kept.
#[derive(Default)]
pub struct DebugSink {
    anchors: Vec<DebugAnchor>,
    lines: Vec<DebugLine>,
}
impl DebugSink {
    pub fn anchor(&mut self, owner: u64, kind: &'static str, position: Position) {
        self.anchors.push(DebugAnchor {
            owner,
            kind,
            position,
        });
    }
    pub fn line(&mut self, from: Position, to: Position, color: [f32; 4]) {
        self.lines.push(DebugLine { from, to, color });
    }
}
/// A feature's contribution to the debug view: read the world, push anchors and lines.
pub type DebugContributor = fn(&World, &mut DebugSink);
#[derive(Default)]
struct Contributors {
    list: Vec<(&'static str, DebugContributor)>,
}
impl Extension for Contributors {}

/// Edges of the axis-aligned box `[min, max]` (4 in 2D, 12 in 3D).
pub(super) fn box_edges(min: Position, max: Position) -> Vec<[Position; 2]> {
    let n = min.len();
    let corner = |mask: usize| {
        let mut p = min;
        for (axis, value) in p.iter_mut().enumerate() {
            if mask >> axis & 1 == 1 {
                *value = max[axis];
            }
        }
        p
    };
    let mut edges = Vec::new();
    for mask in 0..1usize << n {
        for axis in 0..n {
            if mask >> axis & 1 == 0 {
                edges.push([corner(mask), corner(mask | 1 << axis)]);
            }
        }
    }
    edges
}
/// Segments approximating the arc of `radius` around `center` in the plane of axes `u`, `v`,
/// from angle `start` to `end` radians, in `steps` pieces.
pub(super) fn arc(
    center: Position,
    (u, v): (usize, usize),
    radius: f32,
    (start, end): (f32, f32),
    steps: usize,
) -> Vec<[Position; 2]> {
    let point = |step: usize| {
        let angle = start + (end - start) * step as f32 / steps as f32;
        let mut p = center;
        p[u] += radius * angle.cos();
        p[v] += radius * angle.sin();
        p
    };
    (0..steps).map(|i| [point(i), point(i + 1)]).collect()
}
fn cross(center: Position, color: [f32; 4]) -> impl Iterator<Item = DebugLine> {
    (0..center.len()).map(move |axis| {
        let (mut from, mut to) = (center, center);
        from[axis] -= MARKER_SIZE;
        to[axis] += MARKER_SIZE;
        DebugLine { from, to, color }
    })
}
/// Truncates to `cap` and returns the original length.
fn capped<T>(items: &mut Vec<T>, cap: usize) -> usize {
    let total = items.len();
    items.truncate(cap);
    total
}

impl World {
    /// Registers a debug contributor under a unique `name`; registering a name twice is a
    /// no-op so features may call this on every use. Contributors run in registration order.
    pub fn add_debug_contributor(&mut self, name: &'static str, contributor: DebugContributor) {
        let (_, table) = self.extension_mut::<Contributors>();
        if table.list.iter().all(|(existing, _)| *existing != name) {
            table.list.push((name, contributor));
        }
    }
    /// Bounded debug snapshot. Colliders are sorted by id, contacts by body ids then
    /// position, anchors by owner then position, so identical worlds give identical views.
    pub fn debug_view(&self, options: &DebugOptions) -> Result<DebugView> {
        options.validate()?;
        let core = &self.core;
        let mut colliders = Vec::new();
        for (handle, collider) in core.rapier.colliders.iter() {
            let Some(id) = core.collider_body_id(handle) else {
                continue;
            };
            let Ok(body) = core.body(id) else { continue };
            let pose = body_pose(body);
            let state = if collider.is_sensor() {
                DebugState::Sensor
            } else if body.is_sleeping() {
                DebugState::Sleeping
            } else if body.is_fixed() {
                DebugState::Fixed
            } else if body.is_kinematic() {
                DebugState::Kinematic
            } else {
                DebugState::Dynamic
            };
            let aabb = options.aabbs.then(|| {
                let bounds = collider.shape().compute_aabb(body.position());
                [bounds.mins.to_array(), bounds.maxs.to_array()]
            });
            colliders.push(DebugCollider {
                id,
                shape: super::describe(collider.shape()),
                position: pose.position,
                rotation: pose.rotation,
                state,
                aabb,
            });
        }
        colliders.sort_by_key(|c| c.id);
        let collider_total = capped(&mut colliders, options.max_colliders);

        let mut contacts = Vec::new();
        if options.contacts {
            for pair in core.rapier.narrow_phase.contact_pairs() {
                let (Some(first), Some(second)) = (
                    core.collider_body_id(pair.collider1),
                    core.collider_body_id(pair.collider2),
                ) else {
                    continue;
                };
                let Some(first_collider) = core.rapier.colliders.get(pair.collider1) else {
                    continue;
                };
                let pose = *first_collider.position();
                for manifold in pair.manifolds() {
                    for point in manifold.points.iter().filter(|p| p.dist <= 0.0) {
                        contacts.push(DebugContact {
                            first,
                            second,
                            point: (pose * point.local_p1).to_array(),
                            normal: manifold.data.normal.to_array(),
                            depth: -point.dist,
                        });
                    }
                }
            }
        }
        contacts.sort_by(|a, b| {
            (a.first, a.second)
                .cmp(&(b.first, b.second))
                .then_with(|| a.point.partial_cmp(&b.point).unwrap_or(Ordering::Equal))
        });
        let contact_total = capped(&mut contacts, options.max_contacts);

        let mut sink = DebugSink::default();
        if options.joints {
            for (_, joint) in core.rapier.impulse_joints.iter() {
                for (handle, anchor) in [
                    (joint.body1(), joint.data.local_anchor1()),
                    (joint.body2(), joint.data.local_anchor2()),
                ] {
                    if let (Some(id), Some(body)) =
                        (core.body_id(handle), core.rapier.bodies.get(handle))
                    {
                        sink.anchor(id, "joint", (*body.position() * anchor).to_array());
                    }
                }
            }
        }
        if let Some(table) = self.extension::<Contributors>() {
            for (_, contributor) in &table.list {
                contributor(self, &mut sink);
            }
        }
        let DebugSink {
            mut anchors,
            mut lines,
        } = sink;
        anchors.sort_by(|a, b| {
            a.owner.cmp(&b.owner).then_with(|| {
                a.position
                    .partial_cmp(&b.position)
                    .unwrap_or(Ordering::Equal)
            })
        });
        let anchor_total = capped(&mut anchors, options.max_anchors);
        let line_total = capped(&mut lines, options.max_anchors);
        let truncated = collider_total > colliders.len()
            || contact_total > contacts.len()
            || anchor_total > anchors.len()
            || line_total > lines.len();
        Ok(DebugView {
            colliders,
            contacts,
            anchors,
            lines,
            totals: DebugTotals {
                colliders: collider_total,
                contacts: contact_total,
                anchors: anchor_total,
                lines: line_total,
            },
            truncated,
        })
    }
}

impl DebugView {
    /// World-space line list for the whole view: collider outlines in their state colour,
    /// AABBs, contact normals, joint anchor crosses and contributed lines. At most
    /// `max_lines` are produced; `truncated` reports whether more existed.
    pub fn wireframe(&self, max_lines: usize) -> DebugLines {
        let mut out = DebugLines {
            lines: Vec::new(),
            truncated: false,
        };
        let mut push = |line: DebugLine| {
            if out.lines.len() < max_lines {
                out.lines.push(line);
            } else {
                out.truncated = true;
            }
        };
        for collider in &self.colliders {
            let color = collider.state.color();
            for [a, b] in local_segments(&collider.shape) {
                push(DebugLine {
                    from: to_world(collider.position, collider.rotation, a),
                    to: to_world(collider.position, collider.rotation, b),
                    color,
                });
            }
            if let Some([min, max]) = collider.aabb {
                for [from, to] in box_edges(min, max) {
                    push(DebugLine {
                        from,
                        to,
                        color: AABB_COLOR,
                    });
                }
            }
        }
        for contact in &self.contacts {
            let mut to = contact.point;
            for (axis, value) in to.iter_mut().enumerate() {
                *value += contact.normal[axis] * MARKER_SIZE;
            }
            push(DebugLine {
                from: contact.point,
                to,
                color: CONTACT_COLOR,
            });
        }
        for anchor in &self.anchors {
            cross(anchor.position, ANCHOR_COLOR).for_each(&mut push);
        }
        self.lines.iter().copied().for_each(&mut push);
        out
    }
}
