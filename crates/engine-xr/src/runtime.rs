//! Runtime discovery, instance creation and headset system lookup.
use crate::{
    manifest::{self, DiscoveryInputs, LibraryLocation, RuntimeManifest},
    negotiate::{self, LibraryGuard, LoadedEntry},
    status::XrStatus,
};
use openxr::sys;
use std::path::PathBuf;

/// Application identity passed to `xrCreateInstance`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XrAppInfo {
    /// At most 127 bytes; longer names are truncated at a char boundary.
    pub application_name: String,
    pub application_version: u32,
}
impl Default for XrAppInfo {
    fn default() -> Self {
        Self {
            application_name: "engine-xr application".into(),
            application_version: 0,
        }
    }
}
fn bounded(name: &str, max: usize) -> &str {
    let mut end = name.len().min(max);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// How the OpenXR entry points are obtained.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LoaderMode {
    /// Resolve the active runtime manifest and negotiate with the runtime
    /// directly (no loader DLL, no API layers).
    #[default]
    BuiltIn,
    /// Load a Khronos OpenXR loader from this exact path (enables API layers).
    /// A path is required: bare-name loading would search the working
    /// directory, which is a DLL-planting risk.
    KhronosLoader(PathBuf),
}

/// A loaded runtime (or loader) before instance creation.
pub struct XrRuntime {
    loaded: LoadedEntry,
    manifest: Option<RuntimeManifest>,
    label: String,
}
impl XrRuntime {
    /// Discover the runtime with `mode`.
    pub fn load(inputs: &DiscoveryInputs, mode: &LoaderMode) -> Result<Self, XrStatus> {
        match mode {
            LoaderMode::BuiltIn => Self::discover(inputs),
            LoaderMode::KhronosLoader(path) => {
                let location = LibraryLocation::Path(path.clone());
                let loaded = negotiate::load_khronos_loader(&location).map_err(|reason| {
                    XrStatus::RuntimeLoadFailed {
                        library: path.display().to_string(),
                        reason,
                    }
                })?;
                Ok(Self {
                    loaded,
                    manifest: None,
                    label: format!("Khronos loader {}", path.display()),
                })
            }
        }
    }
    /// Resolve the active runtime manifest and negotiate with its library.
    pub fn discover(inputs: &DiscoveryInputs) -> Result<Self, XrStatus> {
        let manifest = manifest::resolve(inputs)?;
        let loaded = negotiate::negotiate_runtime(&manifest.library, &manifest.negotiate_symbol)
            .map_err(|reason| XrStatus::RuntimeLoadFailed {
                library: manifest.library.to_string(),
                reason,
            })?;
        Ok(Self {
            label: manifest.display_name(),
            manifest: Some(manifest),
            loaded,
        })
    }
    /// Manifest name (or loader path) before an instance reports its own name.
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn manifest(&self) -> Option<&RuntimeManifest> {
        self.manifest.as_ref()
    }
    /// Loader-runtime interface version (0 through the Khronos loader).
    pub fn interface_version(&self) -> u32 {
        self.loaded.interface_version
    }
    /// Create an instance, enabling `XR_KHR_vulkan_enable2` when offered.
    pub fn create_instance(self, app: &XrAppInfo) -> Result<XrInstance, XrStatus> {
        let failed = |result| XrStatus::InstanceCreationFailed {
            runtime: self.label.clone(),
            result,
        };
        let entry = &self.loaded.entry;
        let available = entry.enumerate_extensions().map_err(failed)?;
        let mut extensions = openxr::ExtensionSet::default();
        extensions.khr_vulkan_enable2 = available.khr_vulkan_enable2;
        let instance = entry
            .create_instance(
                &openxr::ApplicationInfo {
                    application_name: bounded(
                        &app.application_name,
                        sys::MAX_APPLICATION_NAME_SIZE - 1,
                    ),
                    application_version: app.application_version,
                    engine_name: "engine-xr",
                    engine_version: 0,
                    api_version: openxr::Version::new(1, 0, 0),
                },
                &extensions,
                &[],
                &(),
            )
            .map_err(failed)?;
        let runtime = match instance.properties() {
            Ok(p) => format!("{} {}", p.runtime_name, p.runtime_version),
            Err(_) => self.label.clone(),
        };
        Ok(XrInstance {
            instance,
            runtime,
            vulkan_enable2: available.khr_vulkan_enable2,
            _library: self.loaded.guard,
        })
    }
}

/// View configuration of one eye (`XrViewConfigurationView`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EyeConfig {
    pub recommended: [u32; 2],
    pub max: [u32; 2],
    pub recommended_samples: u32,
}

/// The head-mounted display system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XrSystem {
    pub id: openxr::SystemId,
    pub name: String,
    pub vendor_id: u32,
    pub eyes: Vec<EyeConfig>,
    pub blend_mode: openxr::EnvironmentBlendMode,
    pub orientation_tracking: bool,
    pub position_tracking: bool,
}

/// An OpenXR instance plus the library that implements it.
pub struct XrInstance {
    pub(crate) instance: openxr::Instance,
    runtime: String,
    vulkan_enable2: bool,
    /// Last field: keeps the runtime library mapped until after the instance drops.
    _library: LibraryGuard,
}
impl XrInstance {
    /// Runtime name and version from `xrGetInstanceProperties`.
    pub fn runtime(&self) -> &str {
        &self.runtime
    }
    pub fn raw(&self) -> &openxr::Instance {
        &self.instance
    }
    /// Whether the runtime offers `XR_KHR_vulkan_enable2`.
    pub fn supports_vulkan_enable2(&self) -> bool {
        self.vulkan_enable2
    }
    /// The head-mounted display system with stereo views.
    pub fn hmd_system(&self) -> Result<XrSystem, XrStatus> {
        let instance = &self.instance;
        let id = instance
            .system(openxr::FormFactor::HEAD_MOUNTED_DISPLAY)
            .map_err(|result| XrStatus::NoHeadset {
                runtime: self.runtime.clone(),
                result,
            })?;
        let unavailable = |what: &str, result: sys::Result| XrStatus::GraphicsBindingUnavailable {
            runtime: self.runtime.clone(),
            reason: format!("{what} failed with {}", crate::result_name(result)),
        };
        let properties = instance
            .system_properties(id)
            .map_err(|r| unavailable("xrGetSystemProperties", r))?;
        let views = instance
            .enumerate_view_configuration_views(id, openxr::ViewConfigurationType::PRIMARY_STEREO)
            .map_err(|r| unavailable("primary stereo view configuration", r))?;
        if views.len() != 2 {
            return Err(XrStatus::GraphicsBindingUnavailable {
                runtime: self.runtime.clone(),
                reason: format!("primary stereo reports {} views, expected 2", views.len()),
            });
        }
        let blend_mode = instance
            .enumerate_environment_blend_modes(id, openxr::ViewConfigurationType::PRIMARY_STEREO)
            .map_err(|r| unavailable("xrEnumerateEnvironmentBlendModes", r))?
            .into_iter()
            .next()
            .unwrap_or(openxr::EnvironmentBlendMode::OPAQUE);
        Ok(XrSystem {
            id,
            name: properties.system_name,
            vendor_id: properties.vendor_id,
            eyes: views
                .into_iter()
                .map(|v| EyeConfig {
                    recommended: [
                        v.recommended_image_rect_width,
                        v.recommended_image_rect_height,
                    ],
                    max: [v.max_image_rect_width, v.max_image_rect_height],
                    recommended_samples: v.recommended_swapchain_sample_count,
                })
                .collect(),
            blend_mode,
            orientation_tracking: properties.tracking_properties.orientation_tracking,
            position_tracking: properties.tracking_properties.position_tracking,
        })
    }
    /// Check the Vulkan binding prerequisites without creating devices.
    pub fn check_graphics(&self, system: &XrSystem) -> Result<(), XrStatus> {
        let unavailable = |reason: String| XrStatus::GraphicsBindingUnavailable {
            runtime: self.runtime.clone(),
            reason,
        };
        if !self.vulkan_enable2 {
            return Err(unavailable(
                "the runtime does not offer XR_KHR_vulkan_enable2".into(),
            ));
        }
        if cfg!(not(any(windows, target_os = "linux"))) {
            return Err(unavailable(
                "the engine's Vulkan XR binding is built only for Windows and Linux".into(),
            ));
        }
        let requirements = self
            .instance
            .graphics_requirements::<openxr::Vulkan>(system.id)
            .map_err(|r| {
                unavailable(format!(
                    "xrGetVulkanGraphicsRequirements2KHR failed with {}",
                    crate::result_name(r)
                ))
            })?;
        let max = requirements.max_api_version_supported;
        if max.major() < 1 || (max.major() == 1 && max.minor() < 1) {
            return Err(unavailable(format!(
                "the runtime supports Vulkan up to {max}, the engine needs 1.1 or newer"
            )));
        }
        Ok(())
    }
}

/// Discover, create an instance and look up the headset, stopping at the
/// first unavailable step. Ends at [`XrStatus::SystemReady`] when a session
/// could be created. Never creates GPU devices or sessions.
pub fn probe(inputs: &DiscoveryInputs, mode: &LoaderMode, app: &XrAppInfo) -> XrStatus {
    let instance =
        match XrRuntime::load(inputs, mode).and_then(|runtime| runtime.create_instance(app)) {
            Ok(instance) => instance,
            Err(status) => return status,
        };
    let system = match instance.hmd_system() {
        Ok(system) => system,
        Err(status) => return status,
    };
    match instance.check_graphics(&system) {
        Ok(()) => XrStatus::SystemReady {
            runtime: instance.runtime().to_owned(),
            system: system.name,
        },
        Err(status) => status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Platform;

    #[test]
    fn no_runtime_fallback_reports_runtime_not_found() {
        let mut inputs = DiscoveryInputs::empty(Platform::Windows);
        inputs.windows_active_runtime = Some(Err("key or value not present".into()));
        let status = probe(&inputs, &LoaderMode::BuiltIn, &XrAppInfo::default());
        assert_eq!(status.state(), "runtime_not_found");
        assert!(
            status
                .to_string()
                .contains(crate::manifest::WINDOWS_ACTIVE_RUNTIME),
            "{status}"
        );
    }

    #[test]
    fn a_manifest_naming_a_non_library_reports_runtime_load_failed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("runtime.bin"), b"not a library").unwrap();
        let manifest = dir.path().join("active_runtime.json");
        std::fs::write(
            &manifest,
            br#"{"file_format_version":"1.0.0","runtime":{"name":"Broken","library_path":"./runtime.bin"}}"#,
        )
        .unwrap();
        let mut inputs = DiscoveryInputs::empty(Platform::Linux);
        inputs.runtime_json = Some(manifest);
        let status = probe(&inputs, &LoaderMode::BuiltIn, &XrAppInfo::default());
        let XrStatus::RuntimeLoadFailed { library, reason } = &status else {
            panic!("expected runtime_load_failed, got {status:?}");
        };
        assert!(library.ends_with("runtime.bin"), "{library}");
        assert!(!reason.is_empty());
    }

    #[test]
    fn a_missing_khronos_loader_is_reported_with_its_path() {
        let path = std::env::temp_dir()
            .join("engine-xr-no-such-loader")
            .join("openxr_loader.dll");
        let status = probe(
            &DiscoveryInputs::empty(Platform::Windows),
            &LoaderMode::KhronosLoader(path.clone()),
            &XrAppInfo::default(),
        );
        let XrStatus::RuntimeLoadFailed { library, .. } = &status else {
            panic!("expected runtime_load_failed, got {status:?}");
        };
        assert_eq!(library, &path.display().to_string());
    }

    #[test]
    fn application_names_are_bounded_at_char_boundaries() {
        assert_eq!(bounded("abc", 2), "ab");
        assert_eq!(bounded("aé", 2), "a");
        assert_eq!(bounded("short", 127), "short");
    }
}
