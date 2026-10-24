use crate::{
    AcousticMaterial, AcousticVolume, Footprint, MaterialId, Proxy2DSettings, ProxyMapping, Result,
    unimplemented,
};
use std::convert::Infallible;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MeshId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstanceId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mobility {
    Static,
    Dynamic,
}
/// Row-major 3x4 affine transform, the layout of `wgpu::TlasInstance::transform`.
pub type Affine3 = [f32; 12];

/// `materials` has length 1 (uniform) or `triangles.len()`.
#[derive(Clone, Debug, PartialEq)]
pub struct AcousticMesh {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub materials: Vec<MaterialId>,
}

/// Build limits (section 5.3). The default is the documented limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldLimits {
    pub max_mesh_triangles: u32,
    pub max_static_triangles: u32,
    pub max_dynamic_triangles: u32,
    pub max_instances: u32,
    pub max_dynamic_instances: u32,
    pub max_materials: u32,
    pub max_volumes: u32,
    pub max_proxy_footprints: u32,
    pub max_footprint_vertices: u32,
    pub max_coordinate_m: u32,
    pub build_work_budget: u64,
    pub max_plane_clusters: u32,
}
impl Default for WorldLimits {
    fn default() -> Self {
        Self {
            max_mesh_triangles: 262_144,
            max_static_triangles: 524_288,
            max_dynamic_triangles: 65_536,
            max_instances: 16_384,
            max_dynamic_instances: 1_024,
            max_materials: 1_024,
            max_volumes: 4_096,
            max_proxy_footprints: 8_192,
            max_footprint_vertices: 32,
            max_coordinate_m: 100_000,
            build_work_budget: 64_000_000,
            max_plane_clusters: 65_536,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct WorldStats {
    pub triangles: u32,
    pub instances: u32,
    pub materials: u32,
    pub dropped_degenerate: u32,
    pub bvh_nodes: u32,
    pub plane_clusters: u32,
    /// `[min, max]` in acoustic metres.
    pub bounds: [[f32; 3]; 2],
}

pub struct AcousticWorldBuilder {
    limits: WorldLimits,
}
impl AcousticWorldBuilder {
    pub fn new(limits: WorldLimits) -> Self {
        Self { limits }
    }
    pub fn limits(&self) -> WorldLimits {
        self.limits
    }
    pub fn add_material(&mut self, _m: AcousticMaterial) -> Result<MaterialId> {
        Err(unimplemented("AcousticWorldBuilder::add_material"))
    }
    pub fn add_mesh(&mut self, _mesh: AcousticMesh) -> Result<MeshId> {
        Err(unimplemented("AcousticWorldBuilder::add_mesh"))
    }
    pub fn add_instance(
        &mut self,
        _mesh: MeshId,
        _transform: Affine3,
        _mobility: Mobility,
        _mask: u8,
    ) -> Result<InstanceId> {
        Err(unimplemented("AcousticWorldBuilder::add_instance"))
    }
    pub fn add_volume(&mut self, _v: AcousticVolume, _transform: Affine3) -> Result<InstanceId> {
        Err(unimplemented("AcousticWorldBuilder::add_volume"))
    }
    pub fn add_proxy_2d(
        &mut self,
        _footprints: &[Footprint],
        _settings: &Proxy2DSettings,
    ) -> Result<ProxyMapping> {
        Err(unimplemented("AcousticWorldBuilder::add_proxy_2d"))
    }
    pub fn build(self) -> Result<AcousticWorld> {
        Err(unimplemented("AcousticWorldBuilder::build"))
    }
}

/// Built world. It can only come from `AcousticWorldBuilder::build`, which is not
/// implemented yet, so no value exists and the methods below are unreachable.
pub struct AcousticWorld {
    never: Infallible,
}
impl AcousticWorld {
    pub fn revision(&self) -> u64 {
        match self.never {}
    }
    pub fn stats(&self) -> WorldStats {
        match self.never {}
    }
    /// Dynamic instances only.
    pub fn set_instance_transform(&mut self, _id: InstanceId, _t: Affine3) -> Result<()> {
        match self.never {}
    }
    pub fn raycast(
        &self,
        _origin: [f32; 3],
        _dir: [f32; 3],
        _t_max: f32,
        _mask: u8,
    ) -> Option<Hit> {
        match self.never {}
    }
    pub fn gpu_view(&self) -> GpuWorldView<'_> {
        match self.never {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub t: f32,
    pub instance: InstanceId,
    pub triangle: u32,
    pub material: MaterialId,
    pub normal: [f32; 3],
}

/// Borrowed slices of the flat layouts shared by CPU traversal and WGSL.
/// `top_nodes` is the instance-level BVH; `nodes` holds all per-mesh BVHs.
#[derive(Clone, Copy, Debug)]
pub struct GpuWorldView<'a> {
    pub nodes: &'a [gpu_layout::BvhNode],
    pub top_nodes: &'a [gpu_layout::BvhNode],
    pub triangles: &'a [gpu_layout::GpuTriangle],
    pub instances: &'a [gpu_layout::GpuInstance],
    pub materials: &'a [gpu_layout::GpuMaterial],
}

/// `#[repr(C)]` Pod layouts, identical for CPU traversal and WGSL.
pub mod gpu_layout {
    use bytemuck::{Pod, Zeroable};

    /// 32 bytes. `count == 0` is interior (children `left_or_first` and `+1`).
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
    pub struct BvhNode {
        pub min: [f32; 3],
        pub left_or_first: u32,
        pub max: [f32; 3],
        pub count: u32,
    }
    /// 48 bytes. `v0[3]` holds the material id as `f32` bits.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
    pub struct GpuTriangle {
        pub v0: [f32; 4],
        pub v1: [f32; 4],
        pub v2: [f32; 4],
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
    pub struct GpuInstance {
        pub world_from_object: [f32; 12],
        pub object_from_world: [f32; 12],
        pub node_offset: u32,
        pub triangle_offset: u32,
        pub mask: u32,
        pub _pad: u32,
    }
    /// `reflect` is `1 - absorption`.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
    pub struct GpuMaterial {
        pub reflect: [f32; 4],
        pub transmission: [f32; 4],
        pub scattering: f32,
        pub _pad: [f32; 3],
    }

    const _: () = {
        assert!(size_of::<BvhNode>() == 32);
        assert!(size_of::<GpuTriangle>() == 48);
        assert!(size_of::<GpuInstance>() == 112);
        assert!(size_of::<GpuMaterial>() == 48);
    };
}
