//! Adapter discovery without opening a device.

use crate::device::{create_instance, ray_query_unavailable_reason};
use crate::{AdapterCapability, GraphicsApi};
use engine_acoustics::BackendError;

/// Lists every adapter on the primary graphics APIs and whether it can run the
/// hardware ray-query tier. `reason` explains a `false`. This only reads adapter
/// features; a tier still needs its self-test before it is used.
pub fn probe_capabilities() -> Result<Vec<AdapterCapability>, BackendError> {
    let backends = GraphicsApi::Primary.backends();
    let instance = create_instance(backends);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    Ok(adapters
        .iter()
        .map(|adapter| {
            let info = adapter.get_info();
            let reason = if info.device_type == wgpu::DeviceType::Cpu {
                Some(format!(
                    "{} is a software adapter; no GPU tier uses it",
                    info.name
                ))
            } else {
                ray_query_unavailable_reason(adapter)
            };
            AdapterCapability {
                name: info.name.clone(),
                backend: format!("{:?}", info.backend),
                ray_query: reason.is_none(),
                reason,
            }
        })
        .collect())
}
