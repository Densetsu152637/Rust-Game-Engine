//! Explicit acoustic volumes (section 5.2 c): solid boxes, inward room shells with
//! open faces, and mesh volumes. Box rooms also provide analytic RT60 estimates.

use crate::math::{self, Vec3};
use crate::world::{AcousticWorld, AcousticWorldBuilder, invalid, limit};
use crate::{
    AcousticMesh, AcousticVolume, Affine3, BandGains, InstanceId, MaterialId, Result, VolumeKind,
    VolumeShape,
};

/// Face order of box volumes and of `VolumeKind::Room::open_faces` bits.
pub const BOX_FACES: [&str; 6] = ["-x", "+x", "-y", "+y", "-z", "+z"];

#[derive(Clone, Copy, Debug)]
pub(crate) struct RoomSeed {
    pub instance: InstanceId,
    pub half_extents: Vec3,
    pub material: MaterialId,
    pub open_faces: u8,
}

/// Analytic description of a box room volume, for Sabine/Eyring estimates.
#[derive(Clone, Debug, PartialEq)]
pub struct RoomInfo {
    pub instance: InstanceId,
    pub half_extents: [f32; 3],
    pub volume_m3: f32,
    /// `(area, absorption)` per face; open faces absorb fully (`[1; 4]`).
    pub surfaces: Vec<(f32, BandGains)>,
    pub open_area_m2: f32,
}
impl RoomInfo {
    pub(crate) fn from_seed(seed: &RoomSeed, world: &AcousticWorld) -> Self {
        let m = world.instances[seed.instance.0 as usize].world_from_object;
        let h = seed.half_extents;
        let absorption = world
            .material(seed.material)
            .map_or([1.0; 4], |m| m.absorption);
        let mut surfaces = Vec::with_capacity(6);
        let mut open = 0.0;
        for (face, (u, v)) in face_axes().iter().enumerate() {
            let eu = math::scale(math::transform_vector(&m, axis(*u)), 2.0 * h[*u]);
            let ev = math::scale(math::transform_vector(&m, axis(*v)), 2.0 * h[*v]);
            let area = math::length(math::cross(eu, ev));
            if seed.open_faces & (1 << face) != 0 {
                open += area;
                surfaces.push((area, [1.0; 4]));
            } else {
                surfaces.push((area, absorption));
            }
        }
        let volume = (8.0
            * f64::from(h[0])
            * f64::from(h[1])
            * f64::from(h[2])
            * math::determinant(&m).abs()) as f32;
        Self {
            instance: seed.instance,
            half_extents: h,
            volume_m3: volume,
            surfaces,
            open_area_m2: open,
        }
    }
    /// True when `p` (acoustic space) is inside the box at the instance's current
    /// transform.
    pub fn contains(&self, world: &AcousticWorld, p: [f32; 3]) -> bool {
        let inv = world.instances[self.instance.0 as usize].object_from_world;
        let local = math::transform_point(&inv, p);
        (0..3).all(|k| local[k].abs() <= self.half_extents[k])
    }
    pub fn surface_m2(&self) -> f32 {
        self.surfaces.iter().map(|s| s.0).sum()
    }
    pub fn sabine_rt60(&self, air_np_per_m: BandGains) -> Result<BandGains> {
        crate::sabine_rt60(self.volume_m3, &self.surfaces, air_np_per_m)
    }
    pub fn eyring_rt60(&self, air_np_per_m: BandGains) -> Result<BandGains> {
        crate::eyring_rt60(self.volume_m3, &self.surfaces, air_np_per_m)
    }
}

fn axis(k: usize) -> Vec3 {
    let mut a = [0.0; 3];
    a[k] = 1.0;
    a
}
/// The two in-plane axes of each face in `BOX_FACES` order.
fn face_axes() -> [(usize, usize); 6] {
    [(1, 2), (1, 2), (0, 2), (0, 2), (0, 1), (0, 1)]
}

/// Axis-aligned box mesh centred at the origin, two triangles per face in
/// `BOX_FACES` order, skipping faces whose bit is set in `skip`.
pub(crate) fn box_mesh(h: Vec3, skip: u8, material: MaterialId) -> AcousticMesh {
    let corner = |i: usize| -> Vec3 {
        [
            if i & 1 == 0 { -h[0] } else { h[0] },
            if i & 2 == 0 { -h[1] } else { h[1] },
            if i & 4 == 0 { -h[2] } else { h[2] },
        ]
    };
    // Corner quads per face (bit 0 = x, bit 1 = y, bit 2 = z).
    const QUADS: [[u32; 4]; 6] = [
        [0, 2, 6, 4],
        [1, 5, 7, 3],
        [0, 4, 5, 1],
        [2, 3, 7, 6],
        [0, 1, 3, 2],
        [4, 6, 7, 5],
    ];
    let mut triangles = Vec::with_capacity(12);
    for (face, q) in QUADS.iter().enumerate() {
        if skip & (1 << face) != 0 {
            continue;
        }
        triangles.push([q[0], q[1], q[2]]);
        triangles.push([q[0], q[2], q[3]]);
    }
    AcousticMesh {
        positions: (0..8).map(corner).collect(),
        triangles,
        materials: vec![material],
    }
}

impl AcousticWorldBuilder {
    /// Adds an explicit volume as one instance placed by `transform`. `Room` boxes
    /// are inward shells (meshes are double-sided) and are recorded for analytic
    /// estimates; `Mesh` volumes take a copy of the mesh with the volume material.
    pub fn add_volume(&mut self, v: AcousticVolume, transform: Affine3) -> Result<InstanceId> {
        if self.volumes >= self.limits().max_volumes {
            return Err(limit(format!(
                "a world holds at most {} volumes",
                self.limits().max_volumes
            )));
        }
        self.check_material(v.material)?;
        let open = match v.kind {
            VolumeKind::Solid => 0,
            VolumeKind::Room { open_faces } => {
                if open_faces >= 1 << 6 {
                    return Err(invalid("room open_faces uses only the low 6 bits"));
                }
                open_faces
            }
        };
        let (mesh, room) = match v.shape {
            VolumeShape::Box { half_extents } => {
                if !half_extents.iter().all(|h| h.is_finite() && *h > 0.0) {
                    return Err(invalid("box half_extents must be finite and positive"));
                }
                let room = matches!(v.kind, VolumeKind::Room { .. }).then_some(half_extents);
                (box_mesh(half_extents, open, v.material), room)
            }
            VolumeShape::Mesh(id) => {
                let Some(src) = self.meshes.get(id.0 as usize) else {
                    return Err(invalid(format!("mesh {} does not exist", id.0)));
                };
                (
                    AcousticMesh {
                        positions: src.positions.clone(),
                        triangles: src.triangles.clone(),
                        materials: vec![v.material],
                    },
                    None,
                )
            }
        };
        let (data, dropped) = self.validate_mesh(mesh)?;
        self.check_instance_room(1, data.triangles.len() as u64, v.mobility)?;
        let inverse = crate::world::validate_transform(
            &transform,
            &data.bounds,
            self.limits().max_coordinate_m as f32,
        )?;
        let mesh = self.push_mesh(data, dropped);
        let instance = self.push_instance(crate::world::InstanceData {
            mesh: mesh.0,
            transform,
            inverse,
            mobility: v.mobility,
            mask: u8::MAX,
        });
        self.volumes += 1;
        if let Some(half_extents) = room {
            self.rooms.push(RoomSeed {
                instance,
                half_extents,
                material: v.material,
                open_faces: open,
            });
        }
        Ok(instance)
    }
}
