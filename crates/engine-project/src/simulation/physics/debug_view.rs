//! Physics debug view data: bounded, world-space and render-agnostic.
//!
//! `engine.PhysicsSettings.debug` gates [`debug_view`]; [`debug_view_with`] ignores the
//! toggle (tools use it). Nothing here draws: a renderer or the editor consumes
//! [`PhysicsDebugView`] directly, or the flat line list from [`PhysicsDebugView::lines`]
//! (world-space segments with RGBA colours). Body ids in the views are `Entity::to_bits()`;
//! use [`body_entity`](super::body_entity) to map them back.
use super::super::mapped;
use super::{service, service_mut};
use crate::Result;
use bevy_ecs::prelude::*;
use engine_physics::{dim2, dim3};
use glam::Vec3;

/// Caps and toggles of one debug view request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugViewOptions {
    /// Cap on colliders, contacts and anchors listed, each (1..=65536).
    pub max_items: usize,
    pub aabbs: bool,
    pub contacts: bool,
    pub joints: bool,
}
impl Default for DebugViewOptions {
    fn default() -> Self {
        Self {
            max_items: 4096,
            aabbs: false,
            contacts: true,
            joints: true,
        }
    }
}
/// A world-space line segment with an RGBA colour. 2D geometry lies on the Z = 0 plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugSegment {
    pub from: Vec3,
    pub to: Vec3,
    pub color: [f32; 4],
}
/// Segments from [`PhysicsDebugView::lines`].
#[derive(Clone, Debug, PartialEq)]
pub struct DebugSegments {
    pub segments: Vec<DebugSegment>,
    pub truncated: bool,
}
/// Both dimensions' debug views.
#[derive(Clone, Debug, PartialEq)]
pub struct PhysicsDebugView {
    pub two: dim2::DebugView,
    pub three: dim3::DebugView,
}
impl PhysicsDebugView {
    /// True when either dimension cut a list at its cap.
    pub fn truncated(&self) -> bool {
        self.two.truncated || self.three.truncated
    }
    /// Colliders listed in both dimensions.
    pub fn collider_count(&self) -> usize {
        self.two.colliders.len() + self.three.colliders.len()
    }
    /// All outlines, AABBs, contact normals and anchor crosses as one segment list, at most
    /// `max_segments`; `truncated` reports whether more existed.
    pub fn lines(&self, max_segments: usize) -> DebugSegments {
        let two = self.two.wireframe(max_segments);
        let three = self
            .three
            .wireframe(max_segments.saturating_sub(two.lines.len()));
        let mut segments: Vec<DebugSegment> = two
            .lines
            .iter()
            .map(|l| DebugSegment {
                from: Vec3::new(l.from[0], l.from[1], 0.),
                to: Vec3::new(l.to[0], l.to[1], 0.),
                color: l.color,
            })
            .collect();
        segments.extend(three.lines.iter().map(|l| DebugSegment {
            from: Vec3::from(l.from),
            to: Vec3::from(l.to),
            color: l.color,
        }));
        DebugSegments {
            segments,
            truncated: two.truncated || three.truncated,
        }
    }
}
/// The debug view configured by `engine.PhysicsSettings` (`debug`, `debug_aabbs`,
/// `debug_max_items`). Fails when `debug` is off.
pub fn debug_view(world: &World) -> Result<PhysicsDebugView> {
    let settings = &service(world)?.settings;
    if !settings.debug {
        return Err(super::super::invalid(
            "Physics debug view is disabled; set engine.PhysicsSettings.debug",
        ));
    }
    debug_view_with(
        world,
        &DebugViewOptions {
            max_items: settings.debug_max_items,
            aabbs: settings.debug_aabbs,
            ..DebugViewOptions::default()
        },
    )
}
/// A debug view with explicit options, whatever `engine.PhysicsSettings.debug` says.
pub fn debug_view_with(world: &World, options: &DebugViewOptions) -> Result<PhysicsDebugView> {
    let physics = service(world)?;
    let cap = options.max_items;
    Ok(PhysicsDebugView {
        two: physics
            .worlds
            .two
            .debug_view(&dim2::DebugOptions {
                max_colliders: cap,
                max_contacts: cap,
                max_anchors: cap,
                aabbs: options.aabbs,
                contacts: options.contacts,
                joints: options.joints,
            })
            .map_err(mapped)?,
        three: physics
            .worlds
            .three
            .debug_view(&dim3::DebugOptions {
                max_colliders: cap,
                max_contacts: cap,
                max_anchors: cap,
                aabbs: options.aabbs,
                contacts: options.contacts,
                joints: options.joints,
            })
            .map_err(mapped)?,
    })
}
/// Registers a debug contributor (anchors and lines) with the 3D physics world.
pub fn add_debug_contributor_3d(
    world: &mut World,
    name: &'static str,
    contributor: dim3::DebugContributor,
) -> Result<()> {
    service_mut(world)?
        .worlds
        .three
        .add_debug_contributor(name, contributor);
    Ok(())
}
/// Registers a debug contributor (anchors and lines) with the 2D physics world.
pub fn add_debug_contributor_2d(
    world: &mut World,
    name: &'static str,
    contributor: dim2::DebugContributor,
) -> Result<()> {
    service_mut(world)?
        .worlds
        .two
        .add_debug_contributor(name, contributor);
    Ok(())
}
