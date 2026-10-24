use crate::{MaterialId, MeshId, Mobility};
use std::convert::Infallible;

#[derive(Clone, Debug, PartialEq)]
pub enum Footprint2D {
    Rect {
        center: [f32; 2],
        half_extents: [f32; 2],
        rotation: f32,
    },
    Circle {
        center: [f32; 2],
        radius: f32,
    },
    Capsule {
        center: [f32; 2],
        half_height: f32,
        radius: f32,
        rotation: f32,
    },
    Polygon {
        points: Vec<[f32; 2]>,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub struct Footprint {
    pub shape: Footprint2D,
    pub material: MaterialId,
    pub mobility: Mobility,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyPlane {
    SideView,
    TopDown,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Enclosure {
    None,
    Auto { margin_m: f32 },
    Bounds { min: [f32; 2], max: [f32; 2] },
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Proxy2DSettings {
    pub plane: ProxyPlane,
    pub units_per_meter: f32,
    pub depth_m: f32,
    pub enclose_depth: bool,
    pub wall_height_m: f32,
    pub ear_height_m: f32,
    pub floor: bool,
    pub ceiling: bool,
    pub open_sky: bool,
    pub enclosure: Enclosure,
    pub material: MaterialId,
    pub floor_material: MaterialId,
    pub ceiling_material: MaterialId,
    pub side_material: MaterialId,
}

/// Listener pose in acoustic space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListenerPose {
    pub position: [f32; 3],
    pub forward: [f32; 3],
    pub up: [f32; 3],
}

/// Mapping from game space to proxy acoustic space. It can only come from a
/// successful `AcousticWorldBuilder::add_proxy_2d`, which is not implemented yet,
/// so no value exists and the methods are unreachable.
#[derive(Clone, Debug)]
pub struct ProxyMapping {
    never: Infallible,
}
impl ProxyMapping {
    pub fn to_acoustic(&self, _world: [f32; 3]) -> [f32; 3] {
        match self.never {}
    }
    pub fn pose_to_acoustic(&self, _pose: ListenerPose) -> ListenerPose {
        match self.never {}
    }
    pub fn metres_per_unit(&self) -> f32 {
        match self.never {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VolumeShape {
    Box { half_extents: [f32; 3] },
    Mesh(MeshId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeKind {
    Solid,
    /// `open_faces` has one bit per face: -X, +X, -Y, +Y, -Z, +Z.
    Room {
        open_faces: u8,
    },
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AcousticVolume {
    pub shape: VolumeShape,
    pub kind: VolumeKind,
    pub material: MaterialId,
    pub mobility: Mobility,
}
