//! Hit materials shared by the path tracer, RT reflections and RT diffuse GI.

use crate::device::{Core, RtDevice};
use crate::error::{RtError, invalid};
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Lambertian hit material. An instance's `material_index` selects one;
/// indices past the end use the last material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RtMaterial {
    /// Diffuse reflectance per channel, in [0, 1].
    pub albedo: [f32; 3],
    /// Emitted radiance per channel, >= 0 (both sides).
    pub emissive: [f32; 3],
}

/// Most materials one table holds.
pub const MAX_RT_MATERIALS: usize = 65_536;

/// `RtMaterial` in WGSL: `{ albedo: vec4<f32>, emissive: vec4<f32> }`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct GpuMaterial {
    albedo: [f32; 4],
    emissive: [f32; 4],
}

pub(crate) fn validate_materials(materials: &[RtMaterial]) -> Result<(), RtError> {
    if materials.is_empty() || materials.len() > MAX_RT_MATERIALS {
        return Err(invalid(format!(
            "{} materials; between 1 and {MAX_RT_MATERIALS} are allowed",
            materials.len()
        )));
    }
    for (i, m) in materials.iter().enumerate() {
        if !m.albedo.iter().all(|a| (0.0..=1.0).contains(a)) {
            return Err(invalid(format!(
                "material {i} albedo {:?} must be within [0, 1]",
                m.albedo
            )));
        }
        if !m.emissive.iter().all(|e| e.is_finite() && *e >= 0.0) {
            return Err(invalid(format!(
                "material {i} emissive {:?} must be finite and >= 0",
                m.emissive
            )));
        }
    }
    Ok(())
}

/// Validates `materials` and uploads them as a read-only storage buffer.
pub(crate) fn upload_materials(
    device: &wgpu::Device,
    materials: &[RtMaterial],
) -> Result<(wgpu::Buffer, u32), RtError> {
    validate_materials(materials)?;
    let records: Vec<GpuMaterial> = materials
        .iter()
        .map(|m| GpuMaterial {
            albedo: [m.albedo[0], m.albedo[1], m.albedo[2], 0.0],
            emissive: [m.emissive[0], m.emissive[1], m.emissive[2], 0.0],
        })
        .collect();
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("engine-render-rt materials"),
        contents: bytemuck::cast_slice(&records),
        usage: wgpu::BufferUsages::STORAGE,
    });
    Ok((buffer, materials.len() as u32))
}

/// Validated hit materials on the GPU, shared by [`crate::ReflectionKernel`]
/// and [`crate::DiffuseGiKernel`] (binding: `array<RtMaterial>` storage).
pub struct RtMaterialTable {
    core: Arc<Core>,
    buffer: wgpu::Buffer,
    count: u32,
}

impl RtMaterialTable {
    pub fn new(rt: &RtDevice, materials: &[RtMaterial]) -> Result<Self, RtError> {
        let core = rt.core().clone();
        core.ensure_usable()?;
        let (buffer, count) = upload_materials(&core.device, materials)?;
        Ok(Self {
            core,
            buffer,
            count,
        })
    }

    /// Number of materials (at least 1).
    pub fn material_count(&self) -> u32 {
        self.count
    }

    pub(crate) fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    pub(crate) fn is_from(&self, core: &Arc<Core>) -> bool {
        Arc::ptr_eq(&self.core, core)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materials_are_validated() {
        let ok = RtMaterial {
            albedo: [0.5; 3],
            emissive: [0.0; 3],
        };
        assert!(validate_materials(&[ok]).is_ok());
        assert!(validate_materials(&[]).is_err());
        let bright = RtMaterial {
            albedo: [1.2, 0.5, 0.5],
            ..ok
        };
        assert!(
            validate_materials(&[bright])
                .unwrap_err()
                .to_string()
                .contains("[0, 1]")
        );
        let nan = RtMaterial {
            emissive: [f32::NAN, 0.0, 0.0],
            ..ok
        };
        assert!(validate_materials(&[nan]).is_err());
        assert_eq!(size_of::<GpuMaterial>(), 32);
    }
}
