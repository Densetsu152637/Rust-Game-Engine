//! Render backend selection and the window host the runner draws through.

use crate::{
    BackendWindow, GpuRequirements, NativeRenderPasses, RenderLimits, RenderStats, WindowSurface,
};
use anyhow::{Result, bail};
use engine_core::bevy_ecs::world::World;
use engine_project::runtime::RenderFrame;
use engine_render::{BACKEND_ENV_VAR, BackendKind};
use std::sync::Arc;
use winit::window::Window;

/// Pick the render backend for a native run.
///
/// An explicit `option` wins and the environment is ignored. Otherwise
/// `env_value` (the content of [`BACKEND_ENV_VAR`], `ENGINE_RENDER_BACKEND`)
/// selects `wgpu` or `vulkan`; unset or blank means wgpu. Any other value,
/// including `headless` (headless runs use `RunOptions::headless` and never
/// open a backend), is an error naming the accepted values.
pub fn resolve_render_backend(
    option: Option<BackendKind>,
    env_value: Option<&str>,
) -> Result<BackendKind> {
    if let Some(kind) = option {
        return Ok(kind);
    }
    let Some(value) = env_value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(BackendKind::default());
    };
    match value.parse::<BackendKind>() {
        Ok(kind @ (BackendKind::Wgpu | BackendKind::Vulkan)) => Ok(kind),
        Ok(BackendKind::Headless) | Err(_) => bail!(
            "{BACKEND_ENV_VAR}={value:?} is not a valid render backend; expected `wgpu` or `vulkan`"
        ),
    }
}

/// [`resolve_render_backend`] reading the process environment.
pub(crate) fn resolve_from_environment(option: Option<BackendKind>) -> Result<BackendKind> {
    match std::env::var(BACKEND_ENV_VAR) {
        Ok(value) => resolve_render_backend(option, Some(&value)),
        Err(std::env::VarError::NotPresent) => resolve_render_backend(option, None),
        Err(std::env::VarError::NotUnicode(_)) => {
            if option.is_some() {
                resolve_render_backend(option, None)
            } else {
                bail!("{BACKEND_ENV_VAR} is not valid unicode; expected `wgpu` or `vulkan`")
            }
        }
    }
}

/// Reject configuration only the wgpu backend can honour.
pub(crate) fn ensure_wgpu_only_features_unused(kind: BackendKind, world: &World) -> Result<()> {
    if kind == BackendKind::Wgpu {
        return Ok(());
    }
    let passes = world
        .get_resource::<NativeRenderPasses>()
        .is_some_and(|passes| !passes.passes.is_empty());
    let requirements = world.get_resource::<GpuRequirements>().is_some_and(|r| {
        let default = GpuRequirements::default();
        r.features != default.features || r.limits != default.limits
    });
    if passes || requirements {
        bail!(
            "native render passes and GpuRequirements require the wgpu backend, but the {kind} \
             backend is selected (set {BACKEND_ENV_VAR}=wgpu or remove them)"
        );
    }
    Ok(())
}

/// The window host of a native run: the wgpu-native surface (custom passes,
/// `GpuRequirements`) or any other backend through the neutral adapter.
pub(crate) enum RenderHost {
    Wgpu(Box<WindowSurface>),
    Backend(Box<BackendWindow>),
}

impl RenderHost {
    pub fn window(&self) -> &Arc<Window> {
        match self {
            Self::Wgpu(host) => &host.window,
            Self::Backend(host) => &host.window,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        match self {
            Self::Wgpu(host) => host.resize(width, height),
            Self::Backend(host) => host.resize(width, height),
        }
    }

    pub fn is_suspended(&self) -> bool {
        match self {
            Self::Wgpu(host) => host.is_suspended(),
            Self::Backend(host) => host.is_suspended(),
        }
    }

    pub fn present(
        &mut self,
        frame: &RenderFrame,
        world: Option<&World>,
    ) -> Result<Option<RenderStats>> {
        match self {
            Self::Wgpu(host) => host.present(frame, world),
            Self::Backend(host) => host.present(frame),
        }
    }

    pub fn wait_idle(&mut self) -> Result<()> {
        match self {
            Self::Wgpu(host) => host.renderer.gpu.wait_idle(),
            Self::Backend(host) => host.wait_idle(),
        }
    }

    /// Open the window host for `kind`, applying the wgpu-only extras (device
    /// requirements, native passes) when wgpu is selected.
    pub fn open(
        kind: BackendKind,
        window: Arc<Window>,
        world: &mut World,
        limits: RenderLimits,
    ) -> Result<Self> {
        match kind {
            BackendKind::Wgpu => {
                let requirements = world
                    .remove_resource::<GpuRequirements>()
                    .unwrap_or_default();
                let mut host = pollster::block_on(WindowSurface::with_requirements(
                    window,
                    limits,
                    requirements,
                ))?;
                let passes = world
                    .remove_resource::<NativeRenderPasses>()
                    .unwrap_or_default();
                host.renderer.set_passes(passes)?;
                Ok(Self::Wgpu(Box::new(host)))
            }
            other => Ok(Self::Backend(Box::new(BackendWindow::new(
                other, window, limits,
            )?))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_option_wins_over_the_environment() {
        assert_eq!(
            resolve_render_backend(Some(BackendKind::Vulkan), Some("wgpu")).unwrap(),
            BackendKind::Vulkan
        );
        assert_eq!(
            resolve_render_backend(Some(BackendKind::Wgpu), Some("garbage")).unwrap(),
            BackendKind::Wgpu
        );
    }

    #[test]
    fn environment_selects_wgpu_or_vulkan_case_insensitively() {
        for (value, expected) in [
            ("wgpu", BackendKind::Wgpu),
            ("WGPU", BackendKind::Wgpu),
            (" Vulkan ", BackendKind::Vulkan),
            ("vk", BackendKind::Vulkan),
        ] {
            assert_eq!(
                resolve_render_backend(None, Some(value)).unwrap(),
                expected,
                "{value}"
            );
        }
    }

    #[test]
    fn unset_or_blank_environment_defaults_to_wgpu() {
        assert_eq!(
            resolve_render_backend(None, None).unwrap(),
            BackendKind::Wgpu
        );
        assert_eq!(
            resolve_render_backend(None, Some("  ")).unwrap(),
            BackendKind::Wgpu
        );
        assert_eq!(
            resolve_render_backend(None, Some("")).unwrap(),
            BackendKind::Wgpu
        );
    }

    #[test]
    fn invalid_values_name_the_accepted_backends() {
        for value in ["metal", "opengl", "headless", "none", "recording"] {
            let error = resolve_render_backend(None, Some(value))
                .unwrap_err()
                .to_string();
            assert!(error.contains("ENGINE_RENDER_BACKEND"), "{error}");
            assert!(error.contains("`wgpu` or `vulkan`"), "{error}");
            assert!(error.contains(value), "{error}");
        }
    }

    #[test]
    fn vulkan_rejects_wgpu_only_configuration() {
        use crate::{GpuContext, RenderTargetInfo};
        use engine_core::bevy_ecs::world::World;

        struct Stub;
        impl crate::NativeRenderPass for Stub {
            fn prepare(&mut self, _: &GpuContext, _: RenderTargetInfo) -> anyhow::Result<()> {
                Ok(())
            }
            fn encode(
                &mut self,
                _: &GpuContext,
                _: &mut wgpu::CommandEncoder,
                _: &wgpu::TextureView,
                _: RenderTargetInfo,
                _: Option<&World>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let mut world = World::new();
        // Nothing configured: every backend is fine, and an empty registry or
        // default requirements do not count as configuration.
        ensure_wgpu_only_features_unused(BackendKind::Vulkan, &world).unwrap();
        world.insert_resource(NativeRenderPasses::default());
        world.insert_resource(GpuRequirements::default());
        ensure_wgpu_only_features_unused(BackendKind::Vulkan, &world).unwrap();

        world
            .resource_mut::<NativeRenderPasses>()
            .register("overlay", Stub)
            .unwrap();
        let error = ensure_wgpu_only_features_unused(BackendKind::Vulkan, &world)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("native render passes and GpuRequirements require the wgpu backend"),
            "{error}"
        );
        ensure_wgpu_only_features_unused(BackendKind::Wgpu, &world).unwrap();

        let mut world = World::new();
        world.insert_resource(GpuRequirements {
            features: wgpu::Features::TIMESTAMP_QUERY,
            ..Default::default()
        });
        assert!(ensure_wgpu_only_features_unused(BackendKind::Vulkan, &world).is_err());
        ensure_wgpu_only_features_unused(BackendKind::Wgpu, &world).unwrap();
    }
}
