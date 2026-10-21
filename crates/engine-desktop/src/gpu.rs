use anyhow::{Context, Result, bail};
use engine_core::bevy_ecs::prelude::Resource;
use std::sync::{Arc, Mutex};

/// Insert before launching to request native features/limits needed by custom
/// pipelines. Unsupported requirements fail device creation with diagnostics.
#[derive(Resource, Clone, Debug, Default)]
pub struct GpuRequirements {
    pub features: wgpu::Features,
    pub limits: wgpu::Limits,
}

/// Public native device access. Resources created here belong to this device;
/// they are never transferred across independent dynamic-library Rust ABIs.
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    errors: Arc<Mutex<Vec<String>>>,
}

impl GpuContext {
    pub fn instance() -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle())
    }

    pub async fn request(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self> {
        Self::request_with_requirements(instance, surface, GpuRequirements::default()).await
    }

    pub async fn request_with_requirements(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        requirements: GpuRequirements,
    ) -> Result<Self> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: surface,
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .context("no compatible GPU adapter")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("engine desktop device"),
                required_features: requirements.features,
                required_limits: requirements.limits,
                ..Default::default()
            })
            .await
            .context("GPU device creation failed")?;
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            if let Ok(mut errors) = sink.lock()
                && errors.len() < 16
            {
                errors.push(format!("GPU validation: {error}"));
            }
        }));
        let sink = errors.clone();
        device.set_device_lost_callback(move |reason, message| {
            if reason != wgpu::DeviceLostReason::Destroyed
                && let Ok(mut errors) = sink.lock()
                && errors.len() < 16
            {
                errors.push(format!("GPU device lost ({reason:?}): {message}"));
            }
        });
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            errors,
        })
    }

    pub async fn headless() -> Result<Self> {
        Self::request(Self::instance(), None).await
    }

    /// GPU failures remain sticky: subsequent frames cannot report success after
    /// validation or device loss. The caller can tear down and recreate the host.
    pub fn check_errors(&self) -> Result<()> {
        let errors = self
            .errors
            .lock()
            .map_err(|_| anyhow::anyhow!("GPU error mutex poisoned"))?;
        if !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        Ok(())
    }

    pub fn wait_idle(&self) -> Result<()> {
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(30)),
            })
            .context("waiting for GPU")?;
        self.check_errors()
    }
}
