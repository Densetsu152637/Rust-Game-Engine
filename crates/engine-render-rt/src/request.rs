//! What a renderer adds to its device creation to host the ray-tracing core,
//! and adapter discovery without opening a device.

use crate::device::experimental_features_token;
use crate::status::{RayTracingStatus, adapter_label, adapter_unavailable_reason};

/// Device features, limits and the experimental-feature token the hardware tier
/// needs. A plain value: the renderer merges it into the device descriptor it
/// already builds ([`RtFeatureRequest::merge_into`]) and afterwards hands the
/// device to [`crate::RtDevice::new`], which runs the GPU self-test.
///
/// Only [`RtFeatureRequest::for_adapter`] creates a request with
/// `EXPERIMENTAL_RAY_QUERY` and an enabled token, and only for a Vulkan adapter
/// that exposes the feature.
#[derive(Clone, Copy, Debug)]
pub struct RtFeatureRequest {
    /// `EXPERIMENTAL_RAY_QUERY`, plus `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`
    /// when the adapter has it (for `R8Unorm`/`R16Float` shadow-mask storage).
    pub features: wgpu::Features,
    /// Acceleration-structure limits (`max_blas_*`, `max_tlas_instance_count`,
    /// `max_acceleration_structures_per_shader_stage`,
    /// `max_buffers_and_acceleration_structures_per_shader_stage`): wgpu's
    /// minimum supported acceleration-structure values. Other fields are unused by
    /// [`Self::merge_limits`].
    pub limits: RtLimitsRequest,
    /// The audited experimental-feature opt-in (see docs/rendering-raytracing.md,
    /// "Token audit").
    pub experimental: wgpu::ExperimentalFeatures,
}

/// The acceleration-structure limits of an [`RtFeatureRequest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtLimitsRequest {
    pub max_blas_primitive_count: u32,
    pub max_blas_geometry_count: u32,
    pub max_tlas_instance_count: u32,
    pub max_acceleration_structures_per_shader_stage: u32,
    pub max_buffers_and_acceleration_structures_per_shader_stage: u32,
}

impl RtLimitsRequest {
    const NONE: Self = Self {
        max_blas_primitive_count: 0,
        max_blas_geometry_count: 0,
        max_tlas_instance_count: 0,
        max_acceleration_structures_per_shader_stage: 0,
        max_buffers_and_acceleration_structures_per_shader_stage: 0,
    };

    fn minimum_supported() -> Self {
        let l = wgpu::Limits::default().using_minimum_supported_acceleration_structure_values();
        Self {
            max_blas_primitive_count: l.max_blas_primitive_count,
            max_blas_geometry_count: l.max_blas_geometry_count,
            max_tlas_instance_count: l.max_tlas_instance_count,
            max_acceleration_structures_per_shader_stage: l
                .max_acceleration_structures_per_shader_stage,
            max_buffers_and_acceleration_structures_per_shader_stage: l
                .max_buffers_and_acceleration_structures_per_shader_stage,
        }
    }
}

impl RtFeatureRequest {
    /// A request that adds nothing (the raster-only fallback).
    pub const fn none() -> Self {
        Self {
            features: wgpu::Features::empty(),
            limits: RtLimitsRequest::NONE,
            experimental: wgpu::ExperimentalFeatures::disabled(),
        }
    }

    /// The request for `adapter`, or the unavailable status explaining why the
    /// hardware tier cannot run on it (non-Vulkan backend, missing feature,
    /// software adapter). A returned request still needs the device self-test of
    /// [`crate::RtDevice::new`] before anything is traced.
    pub fn for_adapter(adapter: &wgpu::Adapter) -> Result<Self, RayTracingStatus> {
        let info = adapter.get_info();
        let adapter_features = adapter.features();
        if let Some((cause, reason)) = adapter_unavailable_reason(
            info.backend,
            info.device_type,
            adapter_features.contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY),
            &info.name,
        ) {
            return Err(RayTracingStatus::unavailable(
                cause,
                reason,
                Some(adapter_label(&info)),
            ));
        }
        let mut features = wgpu::Features::EXPERIMENTAL_RAY_QUERY;
        if adapter_features.contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES) {
            features |= wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        }
        Ok(Self {
            features,
            limits: RtLimitsRequest::minimum_supported(),
            experimental: experimental_features_token(),
        })
    }

    /// `true` when this request enables hardware ray query.
    pub fn is_hardware(&self) -> bool {
        self.features
            .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY)
    }

    /// Raises the acceleration-structure fields of `limits` to this request's
    /// values; every other field is left as the renderer chose it.
    pub fn merge_limits(&self, limits: &mut wgpu::Limits) {
        let r = &self.limits;
        limits.max_blas_primitive_count = limits
            .max_blas_primitive_count
            .max(r.max_blas_primitive_count);
        limits.max_blas_geometry_count = limits
            .max_blas_geometry_count
            .max(r.max_blas_geometry_count);
        limits.max_tlas_instance_count = limits
            .max_tlas_instance_count
            .max(r.max_tlas_instance_count);
        limits.max_acceleration_structures_per_shader_stage = limits
            .max_acceleration_structures_per_shader_stage
            .max(r.max_acceleration_structures_per_shader_stage);
        limits.max_buffers_and_acceleration_structures_per_shader_stage = limits
            .max_buffers_and_acceleration_structures_per_shader_stage
            .max(r.max_buffers_and_acceleration_structures_per_shader_stage);
    }

    /// Merges features, limits and the token into separately held device
    /// requirements (the shape of a renderer's device-feature composition hook).
    pub fn merge_into(
        &self,
        features: &mut wgpu::Features,
        limits: &mut wgpu::Limits,
        experimental: &mut wgpu::ExperimentalFeatures,
    ) {
        *features |= self.features;
        self.merge_limits(limits);
        if self.experimental.is_enabled() {
            *experimental = self.experimental;
        }
    }

    /// Merges this request into a device descriptor.
    pub fn apply(&self, descriptor: &mut wgpu::DeviceDescriptor<'_>) {
        self.merge_into(
            &mut descriptor.required_features,
            &mut descriptor.required_limits,
            &mut descriptor.experimental_features,
        );
    }
}

/// One adapter and whether it can run the hardware tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterRayTracing {
    pub name: String,
    pub backend: wgpu::Backend,
    pub device_type: wgpu::DeviceType,
    /// The adapter is eligible (a device self-test is still required).
    pub ray_query: bool,
    /// Why `ray_query` is `false`.
    pub reason: Option<String>,
}

/// Lists every adapter on `backends` and whether it is eligible for the
/// hardware tier, without opening a device.
pub fn probe_adapters(backends: wgpu::Backends) -> Vec<AdapterRayTracing> {
    let instance = crate::device::create_instance(backends);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    adapters
        .iter()
        .map(|adapter| {
            let info = adapter.get_info();
            let reason = adapter_unavailable_reason(
                info.backend,
                info.device_type,
                adapter
                    .features()
                    .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY),
                &info.name,
            )
            .map(|(_, reason)| reason);
            AdapterRayTracing {
                name: info.name.clone(),
                backend: info.backend,
                device_type: info.device_type,
                ray_query: reason.is_none(),
                reason,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_request_changes_nothing() {
        let request = RtFeatureRequest::none();
        assert!(!request.is_hardware());
        let mut descriptor = wgpu::DeviceDescriptor::default();
        let before_limits = descriptor.required_limits.clone();
        request.apply(&mut descriptor);
        assert_eq!(descriptor.required_features, wgpu::Features::empty());
        assert_eq!(descriptor.required_limits, before_limits);
        assert!(!descriptor.experimental_features.is_enabled());
    }

    #[test]
    fn limit_merge_only_raises_acceleration_structure_fields() {
        let request = RtFeatureRequest {
            limits: RtLimitsRequest::minimum_supported(),
            ..RtFeatureRequest::none()
        };
        let mut limits = wgpu::Limits::downlevel_defaults();
        let before = limits.clone();
        request.merge_limits(&mut limits);
        assert_eq!(limits.max_tlas_instance_count, (1 << 24) - 1);
        assert_eq!(limits.max_blas_primitive_count, 1 << 28);
        assert_eq!(limits.max_acceleration_structures_per_shader_stage, 1);
        // Everything else keeps the renderer's choice.
        assert_eq!(
            limits.max_texture_dimension_2d,
            before.max_texture_dimension_2d
        );
        assert_eq!(
            limits.max_storage_buffer_binding_size,
            before.max_storage_buffer_binding_size
        );
        // Larger renderer values are never lowered.
        let mut big = wgpu::Limits {
            max_tlas_instance_count: u32::MAX,
            ..wgpu::Limits::default()
        };
        request.merge_limits(&mut big);
        assert_eq!(big.max_tlas_instance_count, u32::MAX);
    }
}
