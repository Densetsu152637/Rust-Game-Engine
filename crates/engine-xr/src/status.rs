//! Honest XR availability. Each unavailable state names what was checked and
//! the exact failure, so hosts and tools can report it verbatim.
use openxr::sys;
use std::{fmt, path::PathBuf};

/// Where an active-runtime manifest location came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestSource {
    /// `XR_RUNTIME_JSON` environment override (exclusive when set).
    EnvironmentOverride,
    /// `HKEY_LOCAL_MACHINE\SOFTWARE\Khronos\OpenXR\1` value `ActiveRuntime`.
    WindowsRegistry,
    /// `$XDG_CONFIG_HOME/openxr/1/` (default `$HOME/.config/openxr/1/`).
    XdgConfigHome,
    /// `<dir>/openxr/1/` for each `$XDG_CONFIG_DIRS` entry (default `/etc/xdg`).
    XdgConfigDirs,
    /// `/etc/openxr/1/`.
    SystemConfig,
}
impl ManifestSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::EnvironmentOverride => "XR_RUNTIME_JSON",
            Self::WindowsRegistry => "registry",
            Self::XdgConfigHome => "XDG_CONFIG_HOME",
            Self::XdgConfigDirs => "XDG_CONFIG_DIRS",
            Self::SystemConfig => "system config",
        }
    }
}

/// One manifest location that was examined and why it did not yield a runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedLocation {
    pub source: ManifestSource,
    pub location: String,
    pub outcome: String,
}
impl fmt::Display for CheckedLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}: {}",
            self.source.label(),
            self.location,
            self.outcome
        )
    }
}

/// OpenXR session state as seen by the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPhase {
    /// Created, waiting for the runtime (headset not worn or app not ready).
    Idle,
    /// Runtime asked the app to begin; `xrBeginSession` follows.
    Ready,
    /// Running, frames are synchronized but not displayed.
    Synchronized,
    /// Running and displayed, without input focus.
    Visible,
    /// Running, displayed and receiving input.
    Focused,
    /// Runtime asked the app to stop; `xrEndSession` follows.
    Stopping,
    /// Session is ending; it will be destroyed.
    Exiting,
}
impl SessionPhase {
    /// True for the states in which the frame loop runs.
    pub fn is_running(self) -> bool {
        matches!(self, Self::Synchronized | Self::Visible | Self::Focused)
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Ready => "ready",
            Self::Synchronized => "synchronized",
            Self::Visible => "visible",
            Self::Focused => "focused",
            Self::Stopping => "stopping",
            Self::Exiting => "exiting",
        }
    }
}

/// `XR_<NAME> (<code>)`, for example `XR_ERROR_FORM_FACTOR_UNAVAILABLE (-35)`.
pub fn result_name(result: sys::Result) -> String {
    format!("XR_{result:?} ({})", result.into_raw())
}

/// Honest XR availability, from runtime discovery to a lost session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XrStatus {
    /// No OpenXR runtime convention exists on this platform (macOS) and no
    /// `XR_RUNTIME_JSON` override is set.
    PlatformUnsupported { platform: String },
    /// No active-runtime manifest exists; lists every location checked.
    RuntimeNotFound { checked: Vec<CheckedLocation> },
    /// A manifest exists but is unusable (malformed, oversize, missing library).
    ManifestInvalid { manifest: PathBuf, reason: String },
    /// The runtime (or Khronos loader) library could not be loaded or negotiated.
    RuntimeLoadFailed { library: String, reason: String },
    /// The runtime loaded but `xrEnumerateInstanceExtensionProperties` or
    /// `xrCreateInstance` failed.
    InstanceCreationFailed {
        runtime: String,
        result: sys::Result,
    },
    /// The instance exists but no head-mounted display system is available
    /// (`xrGetSystem` failed, normally `XR_ERROR_FORM_FACTOR_UNAVAILABLE`).
    NoHeadset {
        runtime: String,
        result: sys::Result,
    },
    /// A headset exists but the engine's renderer cannot bind to it.
    GraphicsBindingUnavailable { runtime: String, reason: String },
    /// Headset system and Vulkan binding are available; no session yet.
    SystemReady { runtime: String, system: String },
    /// A session exists in `phase`.
    Session {
        runtime: String,
        phase: SessionPhase,
    },
    /// The session or instance was lost (runtime shutdown, device removed).
    SessionLost { runtime: String, reason: String },
}
impl XrStatus {
    /// Stable snake_case state name for telemetry.
    pub fn state(&self) -> &'static str {
        match self {
            Self::PlatformUnsupported { .. } => "platform_unsupported",
            Self::RuntimeNotFound { .. } => "runtime_not_found",
            Self::ManifestInvalid { .. } => "manifest_invalid",
            Self::RuntimeLoadFailed { .. } => "runtime_load_failed",
            Self::InstanceCreationFailed { .. } => "instance_creation_failed",
            Self::NoHeadset { .. } => "no_headset",
            Self::GraphicsBindingUnavailable { .. } => "graphics_binding_unavailable",
            Self::SystemReady { .. } => "system_ready",
            Self::Session { phase, .. } => match phase {
                SessionPhase::Idle | SessionPhase::Ready => "session_idle",
                SessionPhase::Synchronized | SessionPhase::Visible | SessionPhase::Focused => {
                    "session_running"
                }
                SessionPhase::Stopping => "session_stopping",
                SessionPhase::Exiting => "session_exiting",
            },
            Self::SessionLost { .. } => "session_lost",
        }
    }
    /// The runtime description once a runtime was identified.
    pub fn runtime(&self) -> Option<&str> {
        match self {
            Self::InstanceCreationFailed { runtime, .. }
            | Self::NoHeadset { runtime, .. }
            | Self::GraphicsBindingUnavailable { runtime, .. }
            | Self::SystemReady { runtime, .. }
            | Self::Session { runtime, .. }
            | Self::SessionLost { runtime, .. } => Some(runtime),
            _ => None,
        }
    }
    /// True when a head-mounted display can be rendered to (session states
    /// included).
    pub fn headset_available(&self) -> bool {
        matches!(self, Self::SystemReady { .. } | Self::Session { .. })
    }
}
impl fmt::Display for XrStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlatformUnsupported { platform } => write!(
                f,
                "no OpenXR runtime convention exists on {platform}; set XR_RUNTIME_JSON to a runtime manifest to try one"
            ),
            Self::RuntimeNotFound { checked } => {
                f.write_str("no active OpenXR runtime manifest found; checked ")?;
                for (index, location) in checked.iter().enumerate() {
                    if index > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "{location}")?;
                }
                Ok(())
            }
            Self::ManifestInvalid { manifest, reason } => {
                write!(
                    f,
                    "runtime manifest {} is unusable: {reason}",
                    manifest.display()
                )
            }
            Self::RuntimeLoadFailed { library, reason } => {
                write!(
                    f,
                    "OpenXR runtime library {library} could not be used: {reason}"
                )
            }
            Self::InstanceCreationFailed { runtime, result } => {
                write!(
                    f,
                    "{runtime} was found but OpenXR instance creation failed with {}: {result}",
                    result_name(*result)
                )?;
                if *result == sys::Result::ERROR_RUNTIME_UNAVAILABLE {
                    f.write_str(
                        " (the runtime library loaded; its service is likely not running, e.g. the Oculus OVRService or SteamVR)",
                    )?;
                }
                Ok(())
            }
            Self::NoHeadset { runtime, result } => write!(
                f,
                "{runtime} reports no head-mounted display ({}: {result}); connect and power on the headset and start its runtime service",
                result_name(*result)
            ),
            Self::GraphicsBindingUnavailable { runtime, reason } => {
                write!(
                    f,
                    "{runtime} cannot bind the engine's Vulkan renderer: {reason}"
                )
            }
            Self::SystemReady { runtime, system } => {
                write!(
                    f,
                    "{runtime}: headset system {system} is available; no session yet"
                )
            }
            Self::Session { runtime, phase } => {
                write!(f, "{runtime}: session {}", phase.name())
            }
            Self::SessionLost { runtime, reason } => write!(f, "{runtime}: session lost: {reason}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_and_reasons_are_precise() {
        let status = XrStatus::NoHeadset {
            runtime: "Oculus OpenXR 1.2.3".into(),
            result: sys::Result::ERROR_FORM_FACTOR_UNAVAILABLE,
        };
        assert_eq!(status.state(), "no_headset");
        assert_eq!(status.runtime(), Some("Oculus OpenXR 1.2.3"));
        let text = status.to_string();
        assert!(
            text.contains("XR_ERROR_FORM_FACTOR_UNAVAILABLE (-35)"),
            "{text}"
        );
        let lost = XrStatus::Session {
            runtime: "r".into(),
            phase: SessionPhase::Focused,
        };
        assert_eq!(lost.state(), "session_running");
        assert!(lost.headset_available());
        let missing = XrStatus::RuntimeNotFound {
            checked: vec![CheckedLocation {
                source: ManifestSource::WindowsRegistry,
                location: r"HKEY_LOCAL_MACHINE\SOFTWARE\Khronos\OpenXR\1\ActiveRuntime".into(),
                outcome: "value not present".into(),
            }],
        };
        assert_eq!(
            missing.to_string(),
            r"no active OpenXR runtime manifest found; checked registry HKEY_LOCAL_MACHINE\SOFTWARE\Khronos\OpenXR\1\ActiveRuntime: value not present"
        );
        assert!(!missing.headset_available());
    }
}
