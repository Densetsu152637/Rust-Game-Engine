//! Scene inputs and their validation at the crate boundary.

use crate::error::{RtError, invalid};

/// Largest instance custom index: TLAS custom data has 24 bits.
pub const MAX_CUSTOM_INDEX: u32 = (1 << 24) - 1;

/// Mesh indices as supplied by the caller. `U16` indices are widened to `u32`
/// in the shared index buffer.
#[derive(Clone, Copy, Debug)]
pub enum RtIndices<'a> {
    U16(&'a [u16]),
    U32(&'a [u32]),
}

impl RtIndices<'_> {
    pub fn len(&self) -> usize {
        match self {
            Self::U16(i) => i.len(),
            Self::U32(i) => i.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn max(&self) -> Option<u32> {
        match self {
            Self::U16(i) => i.iter().copied().max().map(u32::from),
            Self::U32(i) => i.iter().copied().max(),
        }
    }

    pub(crate) fn to_u32(self) -> Vec<u32> {
        match self {
            Self::U16(i) => i.iter().copied().map(u32::from).collect(),
            Self::U32(i) => i.to_vec(),
        }
    }
}

/// One triangle mesh (one BLAS). Indices are relative to the mesh's own
/// vertices; triangle `k` is indices `3k..3k + 3` and is the hit's
/// `primitive_index`.
#[derive(Clone, Copy, Debug)]
pub struct RtMeshData<'a> {
    pub positions: &'a [[f32; 3]],
    pub indices: RtIndices<'a>,
    /// Optional per-vertex normals for `RtSurface::shading_normal`.
    pub normals: Option<&'a [[f32; 3]]>,
}

/// One TLAS instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RtInstanceDesc {
    pub mesh: crate::MeshId,
    /// Row-major 3x4 object-to-world transform (`[r0c0, r0c1, r0c2, tx, r1c0, ...]`),
    /// as `wgpu::TlasInstance` takes it. Must be finite and invertible.
    pub transform: [f32; 12],
    /// Instance mask: a ray with cull mask `m` sees this instance when `m & mask != 0`.
    pub mask: u8,
    /// Returned as `RtHit::custom_index`; at most [`MAX_CUSTOM_INDEX`].
    pub custom_index: u32,
    /// Returned as `RtSurface::material_index`; interpreted by the pass.
    pub material_index: u32,
}

/// Identity transform for [`RtInstanceDesc::transform`].
pub const IDENTITY_TRANSFORM: [f32; 12] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0,
];

/// Resource limits of one [`crate::RtScene`], checked on every edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtSceneLimits {
    pub max_meshes: u32,
    pub max_instances: u32,
    pub max_triangles_per_mesh: u32,
    pub max_vertices_per_mesh: u32,
    /// Sum over meshes; bounds the shared index buffer (4 bytes x 3 per triangle).
    pub max_total_triangles: u32,
    /// Sum over meshes; bounds the shared position and normal buffers (12 bytes each).
    pub max_total_vertices: u32,
}

impl Default for RtSceneLimits {
    fn default() -> Self {
        Self {
            max_meshes: 65_536,
            max_instances: 1 << 20,
            max_triangles_per_mesh: 1 << 24,
            max_vertices_per_mesh: 1 << 24,
            max_total_triangles: 1 << 25,
            max_total_vertices: 1 << 25,
        }
    }
}

impl RtSceneLimits {
    /// Checks the limits against the device: TLAS instance count (and the
    /// 24-bit custom index), BLAS primitive count and storage-binding sizes.
    pub fn check_device(&self, device: &wgpu::Limits) -> Result<(), RtError> {
        let fail = |what: &str, value: u64, cap: u64| {
            Err(invalid(format!(
                "RtSceneLimits::{what} = {value} exceeds the device limit {cap}"
            )))
        };
        if self.max_instances == 0 || self.max_meshes == 0 {
            return Err(invalid(
                "RtSceneLimits needs at least one mesh and instance",
            ));
        }
        if self.max_instances > device.max_tlas_instance_count {
            return fail(
                "max_instances",
                self.max_instances.into(),
                device.max_tlas_instance_count.into(),
            );
        }
        if self.max_triangles_per_mesh > device.max_blas_primitive_count {
            return fail(
                "max_triangles_per_mesh",
                self.max_triangles_per_mesh.into(),
                device.max_blas_primitive_count.into(),
            );
        }
        let binding = device
            .max_storage_buffer_binding_size
            .min(device.max_buffer_size);
        let index_bytes = u64::from(self.max_total_triangles) * 12;
        if index_bytes > binding {
            return fail(
                "max_total_triangles (x 12 index bytes)",
                index_bytes,
                binding,
            );
        }
        let vertex_bytes = u64::from(self.max_total_vertices) * 12;
        if vertex_bytes > binding {
            return fail(
                "max_total_vertices (x 12 position bytes)",
                vertex_bytes,
                binding,
            );
        }
        let instance_bytes = u64::from(self.max_instances) * 16;
        if instance_bytes > binding {
            return fail("max_instances (x 16 record bytes)", instance_bytes, binding);
        }
        Ok(())
    }

    /// The defaults reduced to what `device` allows.
    pub fn for_device(device: &wgpu::Limits) -> Self {
        let d = Self::default();
        let binding = device
            .max_storage_buffer_binding_size
            .min(device.max_buffer_size);
        let cap = |bytes_each: u64| (binding / bytes_each).min(u64::from(u32::MAX)) as u32;
        Self {
            max_meshes: d.max_meshes,
            max_instances: d
                .max_instances
                .min(device.max_tlas_instance_count)
                .min(cap(16)),
            max_triangles_per_mesh: d
                .max_triangles_per_mesh
                .min(device.max_blas_primitive_count),
            max_vertices_per_mesh: d.max_vertices_per_mesh,
            max_total_triangles: d.max_total_triangles.min(cap(12)),
            max_total_vertices: d.max_total_vertices.min(cap(12)),
        }
    }
}

fn finite3(v: &[f32; 3]) -> bool {
    v.iter().all(|c| c.is_finite())
}

/// Validates one mesh against `limits`; returns its triangle count.
pub(crate) fn validate_mesh(mesh: &RtMeshData<'_>, limits: &RtSceneLimits) -> Result<u32, RtError> {
    let vertices = mesh.positions.len();
    if vertices == 0 {
        return Err(invalid("mesh has no positions"));
    }
    if vertices as u64 > u64::from(limits.max_vertices_per_mesh) {
        return Err(invalid(format!(
            "mesh has {vertices} vertices; the limit is {}",
            limits.max_vertices_per_mesh
        )));
    }
    let index_count = mesh.indices.len();
    if index_count == 0 || !index_count.is_multiple_of(3) {
        return Err(invalid(format!(
            "mesh has {index_count} indices; a triangle list needs a non-zero multiple of 3"
        )));
    }
    let triangles = index_count / 3;
    if triangles as u64 > u64::from(limits.max_triangles_per_mesh) {
        return Err(invalid(format!(
            "mesh has {triangles} triangles; the limit is {}",
            limits.max_triangles_per_mesh
        )));
    }
    if let Some((i, p)) = mesh.positions.iter().enumerate().find(|(_, p)| !finite3(p)) {
        return Err(invalid(format!("position {i} is not finite: {p:?}")));
    }
    if let Some(max) = mesh.indices.max()
        && max as usize >= vertices
    {
        return Err(invalid(format!(
            "index {max} is out of range for {vertices} vertices"
        )));
    }
    if let Some(normals) = mesh.normals {
        if normals.len() != vertices {
            return Err(invalid(format!(
                "mesh has {} normals for {vertices} vertices",
                normals.len()
            )));
        }
        if let Some((i, n)) = normals
            .iter()
            .enumerate()
            .find(|(_, n)| !finite3(n) || (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]) < 1e-12)
        {
            return Err(invalid(format!(
                "normal {i} is not finite or has zero length: {n:?}"
            )));
        }
    }
    Ok(triangles as u32)
}

/// Validates the transform, mask and custom index of an instance (the mesh
/// reference is checked by the scene).
pub(crate) fn validate_instance(desc: &RtInstanceDesc) -> Result<(), RtError> {
    validate_transform(&desc.transform)?;
    if desc.custom_index > MAX_CUSTOM_INDEX {
        return Err(invalid(format!(
            "custom_index {} exceeds the 24-bit TLAS custom data (max {MAX_CUSTOM_INDEX})",
            desc.custom_index
        )));
    }
    Ok(())
}

pub(crate) fn validate_transform(t: &[f32; 12]) -> Result<(), RtError> {
    if let Some(v) = t.iter().find(|v| !v.is_finite()) {
        return Err(invalid(format!("transform has a non-finite element {v}")));
    }
    let m = |r: usize, c: usize| f64::from(t[r * 4 + c]);
    let det = m(0, 0) * (m(1, 1) * m(2, 2) - m(1, 2) * m(2, 1))
        - m(0, 1) * (m(1, 0) * m(2, 2) - m(1, 2) * m(2, 0))
        + m(0, 2) * (m(1, 0) * m(2, 1) - m(1, 1) * m(2, 0));
    if det.abs() < 1e-12 {
        return Err(invalid(format!(
            "transform is singular (determinant {det:e}); normals cannot be transformed"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRI: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];

    fn mesh<'a>(positions: &'a [[f32; 3]], indices: RtIndices<'a>) -> RtMeshData<'a> {
        RtMeshData {
            positions,
            indices,
            normals: None,
        }
    }

    #[test]
    fn valid_meshes_report_their_triangle_count() {
        let l = RtSceneLimits::default();
        assert_eq!(
            validate_mesh(&mesh(&TRI, RtIndices::U16(&[0, 1, 2])), &l),
            Ok(1)
        );
        assert_eq!(
            validate_mesh(&mesh(&TRI, RtIndices::U32(&[0, 1, 2, 2, 1, 0])), &l),
            Ok(2)
        );
    }

    #[test]
    fn malformed_meshes_are_rejected_with_the_reason() {
        let l = RtSceneLimits::default();
        let err = |m: RtMeshData<'_>| match validate_mesh(&m, &l) {
            Err(RtError::Invalid(message)) => message,
            other => panic!("expected Invalid, got {other:?}"),
        };
        assert!(err(mesh(&[], RtIndices::U32(&[0, 1, 2]))).contains("no positions"));
        assert!(err(mesh(&TRI, RtIndices::U32(&[0, 1]))).contains("multiple of 3"));
        assert!(err(mesh(&TRI, RtIndices::U16(&[]))).contains("multiple of 3"));
        assert!(err(mesh(&TRI, RtIndices::U32(&[0, 1, 3]))).contains("index 3 is out of range"));
        let nan = [[0.0, f32::NAN, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        assert!(err(mesh(&nan, RtIndices::U32(&[0, 1, 2]))).contains("position 0"));
        let short = RtMeshData {
            normals: Some(&[[0.0, 0.0, 1.0]]),
            ..mesh(&TRI, RtIndices::U32(&[0, 1, 2]))
        };
        assert!(err(short).contains("1 normals for 3 vertices"));
        let zero = RtMeshData {
            normals: Some(&[[0.0, 0.0, 1.0], [0.0; 3], [0.0, 0.0, 1.0]]),
            ..mesh(&TRI, RtIndices::U32(&[0, 1, 2]))
        };
        assert!(err(zero).contains("normal 1"));
    }

    #[test]
    fn per_mesh_limits_are_enforced() {
        let l = RtSceneLimits {
            max_triangles_per_mesh: 1,
            max_vertices_per_mesh: 3,
            ..Default::default()
        };
        assert!(validate_mesh(&mesh(&TRI, RtIndices::U32(&[0, 1, 2, 0, 2, 1])), &l).is_err());
        let four = [[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0; 3]];
        assert!(validate_mesh(&mesh(&four, RtIndices::U32(&[0, 1, 2])), &l).is_err());
    }

    #[test]
    fn instances_need_invertible_finite_transforms_and_24_bit_ids() {
        let desc = RtInstanceDesc {
            mesh: crate::MeshId::from_raw(0, 0),
            transform: IDENTITY_TRANSFORM,
            mask: 0xff,
            custom_index: MAX_CUSTOM_INDEX,
            material_index: 0,
        };
        assert!(validate_instance(&desc).is_ok());
        let too_big = RtInstanceDesc {
            custom_index: MAX_CUSTOM_INDEX + 1,
            ..desc
        };
        assert!(
            matches!(validate_instance(&too_big), Err(RtError::Invalid(m)) if m.contains("24-bit"))
        );
        let mut flat = IDENTITY_TRANSFORM;
        flat[10] = 0.0;
        assert!(
            matches!(validate_transform(&flat), Err(RtError::Invalid(m)) if m.contains("singular"))
        );
        let mut inf = IDENTITY_TRANSFORM;
        inf[3] = f32::INFINITY;
        assert!(validate_transform(&inf).is_err());
    }

    #[test]
    fn scene_limits_are_checked_against_device_limits() {
        let device =
            wgpu::Limits::default().using_minimum_supported_acceleration_structure_values();
        let fitted = RtSceneLimits::for_device(&device);
        assert!(fitted.check_device(&device).is_ok());
        // Default 128 MiB bindings cap the shared buffers.
        assert!(
            u64::from(fitted.max_total_triangles) * 12 <= device.max_storage_buffer_binding_size
        );
        assert!(RtSceneLimits::default().check_device(&device).is_err());
        let no_rt = wgpu::Limits::default();
        let err = RtSceneLimits::for_device(&device)
            .check_device(&no_rt)
            .unwrap_err();
        assert!(err.to_string().contains("max_instances"), "{err}");
    }
}
