//! Selection of the graphics API wgpu runs on.

use engine_render::RenderError;
use std::fmt;
use std::str::FromStr;

/// Which graphics APIs wgpu may use for adapter selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum WgpuBackends {
    /// Vulkan, DX12 or Metal, whichever the platform offers (wgpu's default
    /// preference; `WGPU_BACKEND` may override it).
    #[default]
    Primary,
    Vulkan,
    Dx12,
    Metal,
    /// OpenGL (ES), compiled in through wgpu's `gles` feature. May find no
    /// adapter on machines without a GL driver (an `Unavailable` error).
    Gl,
    /// Every API compiled into wgpu, including OpenGL.
    All,
}

impl WgpuBackends {
    pub const ALL: [WgpuBackends; 6] = [
        Self::Primary,
        Self::Vulkan,
        Self::Dx12,
        Self::Metal,
        Self::Gl,
        Self::All,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Vulkan => "vulkan",
            Self::Dx12 => "dx12",
            Self::Metal => "metal",
            Self::Gl => "gl",
            Self::All => "all",
        }
    }

    pub fn to_wgpu(self) -> wgpu::Backends {
        match self {
            Self::Primary => wgpu::Backends::PRIMARY,
            Self::Vulkan => wgpu::Backends::VULKAN,
            Self::Dx12 => wgpu::Backends::DX12,
            Self::Metal => wgpu::Backends::METAL,
            Self::Gl => wgpu::Backends::GL,
            Self::All => wgpu::Backends::all(),
        }
    }
}

impl fmt::Display for WgpuBackends {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for WgpuBackends {
    type Err = RenderError;

    fn from_str(text: &str) -> Result<Self, RenderError> {
        match text.trim().to_ascii_lowercase().as_str() {
            "primary" | "default" => Ok(Self::Primary),
            "vulkan" | "vk" => Ok(Self::Vulkan),
            "dx12" | "d3d12" | "directx12" => Ok(Self::Dx12),
            "metal" => Ok(Self::Metal),
            "gl" | "gles" | "opengl" => Ok(Self::Gl),
            "all" => Ok(Self::All),
            other => Err(RenderError::Unavailable(format!(
                "unknown wgpu backend {other:?}; expected primary, vulkan, dx12, metal, gl or all"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_aliases_and_rejects_unknowns() {
        assert_eq!(
            "Vulkan".parse::<WgpuBackends>().unwrap(),
            WgpuBackends::Vulkan
        );
        assert_eq!(
            " d3d12 ".parse::<WgpuBackends>().unwrap(),
            WgpuBackends::Dx12
        );
        assert_eq!("GLES".parse::<WgpuBackends>().unwrap(), WgpuBackends::Gl);
        assert_eq!(
            "default".parse::<WgpuBackends>().unwrap(),
            WgpuBackends::Primary
        );
        let error = "directdraw"
            .parse::<WgpuBackends>()
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("primary, vulkan, dx12, metal, gl or all"),
            "{error}"
        );
    }

    #[test]
    fn display_round_trips_through_parse() {
        for backends in WgpuBackends::ALL {
            assert_eq!(
                backends.to_string().parse::<WgpuBackends>().unwrap(),
                backends
            );
        }
    }

    #[test]
    fn maps_onto_wgpu_backend_sets() {
        assert_eq!(WgpuBackends::default(), WgpuBackends::Primary);
        assert_eq!(WgpuBackends::Vulkan.to_wgpu(), wgpu::Backends::VULKAN);
        assert_eq!(WgpuBackends::Dx12.to_wgpu(), wgpu::Backends::DX12);
        assert_eq!(WgpuBackends::Metal.to_wgpu(), wgpu::Backends::METAL);
        assert_eq!(WgpuBackends::Gl.to_wgpu(), wgpu::Backends::GL);
        assert!(
            WgpuBackends::Primary
                .to_wgpu()
                .contains(wgpu::Backends::VULKAN)
        );
        assert!(!WgpuBackends::Primary.to_wgpu().contains(wgpu::Backends::GL));
        assert_eq!(WgpuBackends::All.to_wgpu(), wgpu::Backends::all());
    }
}
