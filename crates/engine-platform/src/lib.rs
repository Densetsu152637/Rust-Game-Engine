//! Host capabilities describe code paths, not installed devices or SDKs.

use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPlatform {
    Windows,
    Linux,
    Macos,
    Android,
    Ios,
    Web,
    Unknown,
}

impl HostPlatform {
    pub const ALL: [Self; 7] = [
        Self::Windows,
        Self::Linux,
        Self::Macos,
        Self::Android,
        Self::Ios,
        Self::Web,
        Self::Unknown,
    ];

    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "android") {
            Self::Android
        } else if cfg!(target_os = "ios") {
            Self::Ios
        } else if cfg!(target_arch = "wasm32") {
            Self::Web
        } else {
            Self::Unknown
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Android => "android",
            Self::Ios => "ios",
            Self::Web => "web",
            Self::Unknown => "unknown",
        }
    }

    pub fn capabilities(self) -> PlatformCapabilities {
        PlatformCapabilities {
            platform: self,
            capabilities: Capability::ALL
                .into_iter()
                .map(|capability| self.capability_status(capability))
                .collect(),
        }
    }

    pub fn require(self, capability: Capability) -> Result<(), UnsupportedCapability> {
        let status = self.capability_status(capability);
        if status.implemented {
            Ok(())
        } else {
            Err(UnsupportedCapability {
                platform: self,
                capability,
                reason: status.limitation,
            })
        }
    }

    fn capability_status(self, capability: Capability) -> CapabilityStatus {
        let implemented = matches!(self, Self::Windows | Self::Linux | Self::Macos);
        let limitation = if implemented {
            match capability {
                Capability::DesktopWindow => {
                    "Requires a graphical session and compatible GPU driver"
                }
                Capability::NativeAudio => "Requires an available native audio device",
                Capability::Gamepad => {
                    "Requires an attached supported controller and OS permissions"
                }
                Capability::NativePackaging => {
                    "Host-native directory packages; no cross-build, installer, signing or store submission"
                }
                Capability::CpuProfiling => {
                    "CPU stage wall time only; no GPU timestamps or per-system attribution"
                }
            }
        } else {
            match self {
                Self::Android | Self::Ios => {
                    "Mobile application host, lifecycle integration and SDK packaging are not implemented"
                }
                Self::Web => {
                    "Browser application host and browser device adapters are not implemented"
                }
                _ => {
                    "No engine host implementation for this target; console SDK integration is not available"
                }
            }
        };
        CapabilityStatus {
            capability,
            implemented,
            limitation: limitation.to_owned(),
        }
    }
}

impl FromStr for HostPlatform {
    type Err = String;
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|platform| platform.name() == name)
            .ok_or_else(|| format!("Unknown platform {name:?}; expected windows, linux, macos, android, ios, web or unknown"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    DesktopWindow,
    NativeAudio,
    Gamepad,
    NativePackaging,
    CpuProfiling,
}

impl Capability {
    pub const ALL: [Self; 5] = [
        Self::DesktopWindow,
        Self::NativeAudio,
        Self::Gamepad,
        Self::NativePackaging,
        Self::CpuProfiling,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityStatus {
    pub capability: Capability,
    pub implemented: bool,
    pub limitation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformCapabilities {
    pub platform: HostPlatform,
    pub capabilities: Vec<CapabilityStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedCapability {
    pub platform: HostPlatform,
    pub capability: Capability,
    pub reason: String,
}

impl fmt::Display for UnsupportedCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} is unavailable on {}: {}",
            self.capability,
            self.platform.name(),
            self.reason
        )
    }
}
impl Error for UnsupportedCapability {}

/// The packaging host is recorded explicitly. It does not establish the ABI of
/// an arbitrary executable supplied by a caller of the packaging library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageHost {
    pub schema: u32,
    pub platform: HostPlatform,
    pub architecture: String,
    pub runtime_filename: String,
    pub launcher_filename: String,
}

impl PackageHost {
    pub fn current() -> Result<Self, UnsupportedCapability> {
        let platform = HostPlatform::current();
        platform.require(Capability::NativePackaging)?;
        let windows = platform == HostPlatform::Windows;
        Ok(Self {
            schema: 1,
            platform,
            architecture: std::env::consts::ARCH.into(),
            runtime_filename: if windows { "runtime.exe" } else { "runtime" }.into(),
            launcher_filename: if windows { "run.cmd" } else { "run.sh" }.into(),
        })
    }
}
