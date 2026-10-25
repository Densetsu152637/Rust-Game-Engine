//! GPU-visible record layouts shared with `shaders/ray_query.wgsl`, and the
//! scene bind group layout every ray-query pass binds.

use bytemuck::{Pod, Zeroable};

/// `RtInstanceData` in WGSL: one per TLAS slot, indexed by the hit's
/// `instance_index`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct GpuInstanceData {
    /// Index into the geometry records.
    pub geometry: u32,
    pub material: u32,
    pub custom_index: u32,
    pub flags: u32,
}

/// `RtGeometryData` in WGSL: one per mesh.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct GpuGeometryData {
    /// First vertex in the shared position and normal buffers.
    pub vertex_offset: u32,
    /// First index in the shared `u32` index buffer.
    pub index_offset: u32,
    pub triangle_count: u32,
    /// Bit 0: the mesh has vertex normals.
    pub flags: u32,
}

pub(crate) const GEOMETRY_HAS_NORMALS: u32 = 1;

/// Scene bindings (WGSL group chosen by [`crate::wgsl::library`]):
///
/// | binding | resource |
/// | --- | --- |
/// | 0 | `acceleration_structure` (TLAS) |
/// | 1 | `array<RtInstanceData>` |
/// | 2 | `array<RtGeometryData>` |
/// | 3 | `array<u32>` indices |
/// | 4 | `array<f32>` positions (xyz per vertex) |
/// | 5 | `array<f32>` normals (xyz per vertex) |
pub(crate) fn scene_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let storage = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("engine-render-rt scene"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::AccelerationStructure {
                    vertex_return: false,
                },
                count: None,
            },
            storage(1),
            storage(2),
            storage(3),
            storage(4),
            storage(5),
        ],
    })
}

/// One probe ray for [`crate::RtDevice::cast_rays`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct ProbeRay {
    pub origin: [f32; 3],
    pub t_min: f32,
    pub direction: [f32; 3],
    pub t_max: f32,
    /// Instance mask (low 8 bits are used).
    pub mask: u32,
    pub _pad: [u32; 3],
}

impl ProbeRay {
    pub fn new(origin: [f32; 3], direction: [f32; 3], t_min: f32, t_max: f32, mask: u8) -> Self {
        Self {
            origin,
            t_min,
            direction,
            t_max,
            mask: mask as u32,
            _pad: [0; 3],
        }
    }
}

/// Result of one probe ray: `rt_closest_hit`, `rt_surface` and `rt_occluded`
/// for the same ray.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct ProbeHit {
    pub t: f32,
    /// Barycentrics of vertices 1 and 2 of the hit triangle.
    pub barycentrics: [f32; 2],
    pub flags: u32,
    pub custom_index: u32,
    pub instance_index: u32,
    pub primitive_index: u32,
    pub material_index: u32,
    /// World-space unit normal from the object-space winding `(v1 - v0) x (v2 - v0)`.
    pub geometric_normal: [f32; 3],
    pub _pad0: f32,
    /// Interpolated world-space vertex normal, or the geometric normal.
    pub shading_normal: [f32; 3],
    pub _pad1: f32,
    pub position: [f32; 3],
    pub _pad2: f32,
}

pub(crate) const PROBE_HIT: u32 = 1;
pub(crate) const PROBE_FRONT_FACE: u32 = 2;
pub(crate) const PROBE_OCCLUDED: u32 = 4;

impl ProbeHit {
    pub fn hit(&self) -> bool {
        self.flags & PROBE_HIT != 0
    }
    pub fn front_face(&self) -> bool {
        self.flags & PROBE_FRONT_FACE != 0
    }
    /// `rt_occluded` for the same ray.
    pub fn occluded(&self) -> bool {
        self.flags & PROBE_OCCLUDED != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_wgsl_structs() {
        // WGSL sizes: RtInstanceData and RtGeometryData are 4 x u32; ProbeRay is
        // vec3+f32, vec3+f32, u32 padded to 48; ProbeHit is 96.
        assert_eq!(size_of::<GpuInstanceData>(), 16);
        assert_eq!(size_of::<GpuGeometryData>(), 16);
        assert_eq!(size_of::<ProbeRay>(), 48);
        assert_eq!(size_of::<ProbeHit>(), 80);
    }
}
