//! Device feature composition: several crates contribute wgpu features, limits
//! and (optionally) the experimental-features token to one device request.
//!
//! Each contributor adds one named [`DeviceFeatureRequest`] to
//! [`GpuRequirements`]. [`GpuRequirements::compose`] merges them against an
//! adapter: required features must be supported (else device creation fails
//! with the requester's name), optional features are enabled exactly when the
//! adapter supports them, limits merge to the best value any request needs, and
//! experimental features need a token supplied by the requesting crate (this
//! crate never creates one). The resulting [`DeviceFeatureReport`] says, per
//! requester, which features were actually enabled.

use bevy_ecs::prelude::Resource;

/// Longest requester name accepted.
pub const MAX_REQUESTER_BYTES: usize = 64;
/// Most requests one device may compose.
pub const MAX_DEVICE_REQUESTS: usize = 32;

/// One crate's contribution to device creation.
#[derive(Clone, Debug)]
pub struct DeviceFeatureRequest {
    /// Stable, unique name of the requesting crate or subsystem (for example
    /// `engine-render-rt`), used in reports and error messages.
    pub requester: String,
    /// Device creation fails when the adapter lacks any of these.
    pub required: wgpu::Features,
    /// Enabled when the adapter supports them; the report lists the rest.
    pub optional: wgpu::Features,
    /// Minimum limits this requester needs; merged with every other request
    /// through [`wgpu::Limits::or_better_values_from`].
    pub limits: wgpu::Limits,
    /// Opt-in to `EXPERIMENTAL_*` features, created (with its own `unsafe`
    /// acknowledgement) by the requesting crate. Experimental features of a
    /// request without an enabled token are never enabled.
    pub experimental: Option<wgpu::ExperimentalFeatures>,
}

impl DeviceFeatureRequest {
    /// A request needing nothing beyond the WebGPU defaults.
    pub fn new(requester: impl Into<String>) -> Self {
        Self {
            requester: requester.into(),
            required: wgpu::Features::empty(),
            optional: wgpu::Features::empty(),
            limits: wgpu::Limits::default(),
            experimental: None,
        }
    }

    pub fn require(mut self, features: wgpu::Features) -> Self {
        self.required |= features;
        self
    }

    pub fn prefer(mut self, features: wgpu::Features) -> Self {
        self.optional |= features;
        self
    }

    pub fn with_limits(mut self, limits: wgpu::Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Attach the requesting crate's experimental-features token.
    pub fn with_experimental(mut self, token: wgpu::ExperimentalFeatures) -> Self {
        self.experimental = Some(token);
        self
    }

    fn has_token(&self) -> bool {
        self.experimental.is_some_and(|t| t.is_enabled())
    }
}

/// The device request of a host: every contributor's [`DeviceFeatureRequest`].
/// Insert into the world before launching the runner, or pass through
/// [`crate::WgpuOptions::requirements`]. Unsupported required features fail
/// device creation with the requester named.
#[derive(Resource, Clone, Debug, Default)]
pub struct GpuRequirements {
    requests: Vec<DeviceFeatureRequest>,
}

impl GpuRequirements {
    /// Add a request. Names must be nonempty, unique and at most
    /// [`MAX_REQUESTER_BYTES`]; at most [`MAX_DEVICE_REQUESTS`] are kept.
    pub fn request(&mut self, request: DeviceFeatureRequest) -> Result<(), String> {
        let name = request.requester.as_str();
        if name.is_empty() || name.len() > MAX_REQUESTER_BYTES {
            return Err(format!(
                "device feature requester names must be 1..={MAX_REQUESTER_BYTES} bytes"
            ));
        }
        if self.requests.iter().any(|r| r.requester == name) {
            return Err(format!("duplicate device feature requester {name}"));
        }
        if self.requests.len() >= MAX_DEVICE_REQUESTS {
            return Err(format!(
                "at most {MAX_DEVICE_REQUESTS} device feature requests are supported"
            ));
        }
        self.requests.push(request);
        Ok(())
    }

    /// [`Self::request`] by value, for builder-style setup.
    pub fn with(mut self, request: DeviceFeatureRequest) -> Result<Self, String> {
        self.request(request)?;
        Ok(self)
    }

    pub fn requests(&self) -> &[DeviceFeatureRequest] {
        &self.requests
    }

    /// Merge every request against an adapter's `features` and `limits`.
    /// `Err` names the first requester whose required features or limits the
    /// adapter cannot provide.
    pub fn compose(
        &self,
        features: wgpu::Features,
        limits: &wgpu::Limits,
    ) -> Result<ComposedDevice, String> {
        let experimental_mask = wgpu::Features::all_experimental_mask();
        let mut enabled = wgpu::Features::empty();
        let mut merged = wgpu::Limits::default();
        let mut token = wgpu::ExperimentalFeatures::disabled();
        let mut outcomes = Vec::with_capacity(self.requests.len());
        for request in &self.requests {
            let name = &request.requester;
            let missing = request.required - features;
            if !missing.is_empty() {
                return Err(format!(
                    "{name} requires GPU features the adapter does not support: {missing:?}"
                ));
            }
            let experimental = request.required & experimental_mask;
            if !experimental.is_empty() && !request.has_token() {
                return Err(format!(
                    "{name} requires experimental GPU features {experimental:?} without an enabled \
                     wgpu::ExperimentalFeatures token"
                ));
            }
            let mut failed = Vec::new();
            request
                .limits
                .check_limits_with_fail_fn(limits, false, |limit, wanted, allowed| {
                    failed.push(format!("{limit} {wanted} (adapter {allowed})"));
                });
            if !failed.is_empty() {
                return Err(format!(
                    "{name} requires GPU limits the adapter does not support: {}",
                    failed.join(", ")
                ));
            }
            let mut optional = request.optional & features;
            if !request.has_token() {
                optional = optional - experimental_mask;
            }
            let granted = request.required | optional;
            if request.has_token() && granted.intersects(experimental_mask) {
                token = request.experimental.unwrap_or(token);
            }
            enabled |= granted;
            merged = merged.or_better_values_from(&request.limits);
            outcomes.push(RequestOutcome {
                requester: name.clone(),
                required: request.required,
                optional: request.optional,
                enabled: granted,
                unavailable: request.optional - granted,
            });
        }
        Ok(ComposedDevice {
            features: enabled,
            limits: merged,
            experimental: token,
            report: DeviceFeatureReport {
                requests: outcomes,
                device_features: enabled,
                experimental_enabled: token.is_enabled(),
            },
        })
    }

    /// Check a device created outside the engine (for example an OpenXR
    /// runtime's Vulkan device wrapped by the caller through wgpu-hal) against
    /// these requests: required features and limits must be present on the
    /// device; optional features count as enabled when the device has them.
    pub fn verify_device(
        &self,
        device_features: wgpu::Features,
        device_limits: &wgpu::Limits,
    ) -> Result<DeviceFeatureReport, String> {
        let mut composed = self.compose(device_features, device_limits)?;
        composed.report.device_features = device_features;
        composed.report.experimental_enabled = device_features
            .intersects(wgpu::Features::all_experimental_mask())
            || composed.report.experimental_enabled;
        Ok(composed.report)
    }
}

/// The merged request handed to `Adapter::request_device`.
#[derive(Clone, Debug)]
pub struct ComposedDevice {
    pub features: wgpu::Features,
    pub limits: wgpu::Limits,
    pub experimental: wgpu::ExperimentalFeatures,
    pub report: DeviceFeatureReport,
}

impl ComposedDevice {
    pub fn descriptor(&self, label: &'static str) -> wgpu::DeviceDescriptor<'static> {
        wgpu::DeviceDescriptor {
            label: Some(label),
            required_features: self.features,
            required_limits: self.limits.clone(),
            experimental_features: self.experimental,
            ..Default::default()
        }
    }
}

/// What one requester asked for and what the device enabled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestOutcome {
    pub requester: String,
    pub required: wgpu::Features,
    pub optional: wgpu::Features,
    /// Required plus the supported optional features.
    pub enabled: wgpu::Features,
    /// Optional features the adapter (or a missing experimental token) denied.
    pub unavailable: wgpu::Features,
}

/// Which requested features a device actually enabled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceFeatureReport {
    pub requests: Vec<RequestOutcome>,
    /// Every feature the device was created with.
    pub device_features: wgpu::Features,
    /// Whether the device accepts `EXPERIMENTAL_*` features.
    pub experimental_enabled: bool,
}

impl DeviceFeatureReport {
    /// The outcome for `requester`, if it contributed a request.
    pub fn outcome(&self, requester: &str) -> Option<&RequestOutcome> {
        self.requests.iter().find(|r| r.requester == requester)
    }

    /// Whether `requester` got every feature in `features`.
    pub fn enabled_for(&self, requester: &str, features: wgpu::Features) -> bool {
        self.outcome(requester)
            .is_some_and(|o| o.enabled.contains(features))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::Features as F;

    fn adapter() -> (F, wgpu::Limits) {
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: 1 << 30,
            ..Default::default()
        };
        (F::TIMESTAMP_QUERY | F::SHADER_F16, limits)
    }

    #[test]
    fn required_and_supported_optional_features_merge_and_are_reported() {
        let (features, limits) = adapter();
        let requirements = GpuRequirements::default()
            .with(DeviceFeatureRequest::new("timing").require(F::TIMESTAMP_QUERY))
            .unwrap()
            .with(
                DeviceFeatureRequest::new("half")
                    .prefer(F::SHADER_F16 | F::SHADER_F64)
                    .with_limits(wgpu::Limits {
                        max_storage_buffer_binding_size: 512 << 20,
                        ..Default::default()
                    }),
            )
            .unwrap();
        let composed = requirements.compose(features, &limits).unwrap();
        assert_eq!(composed.features, F::TIMESTAMP_QUERY | F::SHADER_F16);
        assert_eq!(composed.limits.max_storage_buffer_binding_size, 512 << 20);
        assert!(!composed.experimental.is_enabled());
        let half = composed.report.outcome("half").unwrap();
        assert_eq!(half.enabled, F::SHADER_F16);
        assert_eq!(half.unavailable, F::SHADER_F64);
        assert!(composed.report.enabled_for("timing", F::TIMESTAMP_QUERY));
        assert!(!composed.report.enabled_for("half", F::SHADER_F64));
        assert!(!composed.report.enabled_for("absent", F::empty()));
    }

    #[test]
    fn missing_required_features_and_limits_name_the_requester() {
        let (features, limits) = adapter();
        let error = GpuRequirements::default()
            .with(DeviceFeatureRequest::new("rt").require(F::EXPERIMENTAL_RAY_QUERY))
            .unwrap()
            .compose(features, &limits)
            .unwrap_err();
        assert!(error.starts_with("rt requires GPU features"), "{error}");
        let error = GpuRequirements::default()
            .with(DeviceFeatureRequest::new("big").with_limits(wgpu::Limits {
                max_storage_buffer_binding_size: 2 << 30,
                ..Default::default()
            }))
            .unwrap()
            .compose(features, &limits)
            .unwrap_err();
        assert!(
            error.contains("big requires GPU limits") && error.contains("max_storage_buffer"),
            "{error}"
        );
    }

    #[test]
    fn experimental_features_need_the_requesters_token() {
        let features = F::EXPERIMENTAL_RAY_QUERY | F::TIMESTAMP_QUERY;
        let limits = wgpu::Limits::default();
        let required = GpuRequirements::default()
            .with(DeviceFeatureRequest::new("rt").require(F::EXPERIMENTAL_RAY_QUERY))
            .unwrap();
        let error = required.compose(features, &limits).unwrap_err();
        assert!(error.contains("without an enabled"), "{error}");
        // A disabled token is no token.
        let disabled = GpuRequirements::default()
            .with(
                DeviceFeatureRequest::new("rt")
                    .prefer(F::EXPERIMENTAL_RAY_QUERY)
                    .with_experimental(wgpu::ExperimentalFeatures::disabled()),
            )
            .unwrap();
        let composed = disabled.compose(features, &limits).unwrap();
        assert!(composed.features.is_empty());
        assert_eq!(
            composed.report.outcome("rt").unwrap().unavailable,
            F::EXPERIMENTAL_RAY_QUERY
        );
        assert!(!composed.report.experimental_enabled);
    }

    #[test]
    fn requester_names_are_validated() {
        let mut requirements = GpuRequirements::default();
        assert!(requirements.request(DeviceFeatureRequest::new("")).is_err());
        assert!(
            requirements
                .request(DeviceFeatureRequest::new("x".repeat(65)))
                .is_err()
        );
        requirements
            .request(DeviceFeatureRequest::new("a"))
            .unwrap();
        let error = requirements
            .request(DeviceFeatureRequest::new("a"))
            .unwrap_err();
        assert!(error.contains("duplicate"), "{error}");
        for index in 1..MAX_DEVICE_REQUESTS {
            requirements
                .request(DeviceFeatureRequest::new(format!("r{index}")))
                .unwrap();
        }
        assert!(
            requirements
                .request(DeviceFeatureRequest::new("over"))
                .is_err()
        );
        assert_eq!(requirements.requests().len(), MAX_DEVICE_REQUESTS);
    }

    #[test]
    fn external_devices_are_verified_against_the_requests() {
        let requirements = GpuRequirements::default()
            .with(
                DeviceFeatureRequest::new("xr")
                    .require(F::MULTIVIEW)
                    .prefer(F::TIMESTAMP_QUERY),
            )
            .unwrap();
        let limits = wgpu::Limits::default();
        let report = requirements
            .verify_device(F::MULTIVIEW | F::SHADER_F16, &limits)
            .unwrap();
        assert_eq!(report.device_features, F::MULTIVIEW | F::SHADER_F16);
        assert_eq!(report.outcome("xr").unwrap().enabled, F::MULTIVIEW);
        assert_eq!(
            report.outcome("xr").unwrap().unavailable,
            F::TIMESTAMP_QUERY
        );
        let error = requirements
            .verify_device(F::SHADER_F16, &limits)
            .unwrap_err();
        assert!(error.contains("xr requires"), "{error}");
    }
}
