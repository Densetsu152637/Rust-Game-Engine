//! Built-in runtime negotiation: load the active runtime's library and call
//! `xrNegotiateLoaderRuntimeInterface` exactly as the Khronos loader would,
//! then build an [`openxr::Entry`] from the negotiated `xrGetInstanceProcAddr`.
//! API layers are not loaded on this path.
#![allow(unsafe_code)]

use crate::manifest::LibraryLocation;
use openxr::sys;
use std::{ffi::CString, path::Path, sync::Arc};

/// Keeps a dynamically loaded library mapped while any OpenXR handle created
/// through it is alive. Owners store it as their last field so it drops last.
#[derive(Clone)]
pub(crate) struct LibraryGuard(#[allow(dead_code)] Arc<libloading::Library>);

/// An entry plus the library that implements it.
pub(crate) struct LoadedEntry {
    pub entry: openxr::Entry,
    /// Interface version agreed with the runtime (0 for the Khronos loader).
    pub interface_version: u32,
    pub guard: LibraryGuard,
}

/// Loader-runtime interface version this negotiator implements.
const LOADER_INTERFACE_VERSION: u32 = sys::CURRENT_LOADER_RUNTIME_VERSION as u32;

fn open(location: &LibraryLocation) -> Result<libloading::Library, String> {
    match location {
        LibraryLocation::Path(path) => open_path(path),
        // SAFETY: loading runs the library's initialisers. The name comes from
        // the active runtime manifest the system (or XR_RUNTIME_JSON)
        // designates, which is the trust the Khronos loader extends as well.
        LibraryLocation::SystemSearch(name) => {
            unsafe { libloading::Library::new(name) }.map_err(|e| e.to_string())
        }
    }
}

#[cfg(windows)]
fn open_path(path: &Path) -> Result<libloading::Library, String> {
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
    };
    // SAFETY: as in `open`; the path was validated to exist by the manifest
    // parser and names the designated runtime.
    match unsafe { libloading::Library::new(path) } {
        Ok(library) => Ok(library),
        // Like the Khronos loader, retry so the runtime's own directory is
        // searched for its dependencies.
        // SAFETY: same library, only the dependency search path differs.
        Err(first) => unsafe {
            Library::load_with_flags(
                path,
                LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            )
        }
        .map(Into::into)
        .map_err(|second| format!("{first}; retry with the library directory: {second}")),
    }
}
#[cfg(not(windows))]
fn open_path(path: &Path) -> Result<libloading::Library, String> {
    // SAFETY: as in `open`; the path was validated to exist by the manifest
    // parser and names the designated runtime.
    unsafe { libloading::Library::new(path) }.map_err(|e| e.to_string())
}

/// Load a runtime library and negotiate the loader-runtime interface.
pub(crate) fn negotiate_runtime(
    location: &LibraryLocation,
    symbol: &str,
) -> Result<LoadedEntry, String> {
    let library = open(location)?;
    let name = CString::new(symbol).map_err(|_| "negotiation symbol contains NUL".to_owned())?;
    // SAFETY: the OpenXR loader-runtime interface declares this exported
    // function with exactly the `NegotiateLoaderRuntimeInterface` signature.
    let negotiate = unsafe {
        library.get::<sys::pfn::NegotiateLoaderRuntimeInterface>(name.as_bytes_with_nul())
    }
    .map(|symbol| *symbol)
    .map_err(|e| format!("runtime does not export {symbol}: {e}"))?;
    let loader_info = sys::NegotiateLoaderInfo {
        struct_type: sys::NegotiateLoaderInfo::TYPE,
        struct_version: sys::NegotiateLoaderInfo::VERSION,
        struct_size: size_of::<sys::NegotiateLoaderInfo>(),
        min_interface_version: 1,
        max_interface_version: LOADER_INTERFACE_VERSION,
        min_api_version: sys::Version::new(1, 0, 0),
        max_api_version: sys::Version::new(1, 0x3ff, 0xfff),
    };
    let mut request = sys::NegotiateRuntimeRequest {
        struct_type: sys::NegotiateRuntimeRequest::TYPE,
        struct_version: sys::NegotiateRuntimeRequest::VERSION,
        struct_size: size_of::<sys::NegotiateRuntimeRequest>(),
        runtime_interface_version: 0,
        runtime_api_version: sys::Version::new(0, 0, 0),
        get_instance_proc_addr: None,
    };
    // SAFETY: both structures are fully initialised with their declared type,
    // version and size and outlive the call; the runtime only writes `request`.
    // The library stays loaded for the call because `library` is in scope.
    let result = unsafe { negotiate(&loader_info, &mut request) };
    if result != sys::Result::SUCCESS {
        return Err(format!("{symbol} returned {}", crate::result_name(result)));
    }
    let version = request.runtime_interface_version;
    if !(1..=LOADER_INTERFACE_VERSION).contains(&version) {
        return Err(format!(
            "runtime negotiated unsupported loader interface version {version}"
        ));
    }
    let get_instance_proc_addr = request
        .get_instance_proc_addr
        .ok_or_else(|| format!("{symbol} returned no xrGetInstanceProcAddr"))?;
    let guard = LibraryGuard(Arc::new(library));
    // SAFETY: the pointer comes from a runtime that completed negotiation and
    // is therefore specified to return conformant core entry points; `guard`
    // keeps the library mapped for as long as the entry (and every handle made
    // from it) is stored next to it.
    let entry = unsafe { openxr::Entry::from_get_instance_proc_addr(get_instance_proc_addr, &()) }
        .map_err(|e| {
            format!(
                "xrGetInstanceProcAddr bootstrap failed with {}",
                crate::result_name(e)
            )
        })?;
    Ok(LoadedEntry {
        entry,
        interface_version: version,
        guard,
    })
}

/// Load an installed Khronos OpenXR loader (honours API layers).
pub(crate) fn load_khronos_loader(location: &LibraryLocation) -> Result<LoadedEntry, String> {
    let library = open(location)?;
    // SAFETY: the Khronos loader exports `xrGetInstanceProcAddr` with exactly
    // the OpenXR-specified `GetInstanceProcAddr` signature.
    let get_instance_proc_addr =
        unsafe { library.get::<sys::pfn::GetInstanceProcAddr>(b"xrGetInstanceProcAddr\0") }
            .map(|symbol| *symbol)
            .map_err(|e| format!("loader does not export xrGetInstanceProcAddr: {e}"))?;
    let guard = LibraryGuard(Arc::new(library));
    // SAFETY: a conformant loader returns spec-conformant entry points; the
    // guard keeps the loader mapped alongside the entry.
    let entry = unsafe { openxr::Entry::from_get_instance_proc_addr(get_instance_proc_addr, &()) }
        .map_err(|e| {
            format!(
                "xrGetInstanceProcAddr bootstrap failed with {}",
                crate::result_name(e)
            )
        })?;
    Ok(LoadedEntry {
        entry,
        interface_version: 0,
        guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_a_library_fails_to_load_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir
            .path()
            .join(if cfg!(windows) { "fake.dll" } else { "fake.so" });
        std::fs::write(&fake, b"not a shared library").unwrap();
        let Err(reason) = negotiate_runtime(
            &LibraryLocation::Path(fake),
            "xrNegotiateLoaderRuntimeInterface",
        ) else {
            panic!("a text file must not load");
        };
        assert!(!reason.is_empty());
    }
}
