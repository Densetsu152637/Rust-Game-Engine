//! Windows active-runtime lookup (`HKLM\SOFTWARE\Khronos\OpenXR\1\ActiveRuntime`).
#![allow(unsafe_code)]

use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf, ptr};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS},
    System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW},
};

/// Values beyond this are not plausible manifest paths.
const MAX_VALUE_BYTES: u32 = 64 * 1024;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// The `ActiveRuntime` manifest path (environment strings expanded), or why it
/// could not be read.
pub(crate) fn active_runtime() -> Result<PathBuf, String> {
    let subkey = wide(r"SOFTWARE\Khronos\OpenXR\1");
    let value = wide("ActiveRuntime");
    // REG_EXPAND_SZ values are expanded and returned as REG_SZ; read the
    // 64-bit view regardless of this process's bitness.
    let flags = RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY;
    let mut bytes: u32 = 0;
    for _ in 0..3 {
        // SAFETY: both name strings are NUL-terminated and live for the call;
        // a null data pointer only queries the required size into `bytes`.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                value.as_ptr(),
                flags,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut bytes,
            )
        };
        match status {
            ERROR_SUCCESS => {}
            ERROR_FILE_NOT_FOUND => return Err("key or value not present".into()),
            other => return Err(format!("RegGetValueW failed with Win32 error {other}")),
        }
        if bytes > MAX_VALUE_BYTES {
            return Err(format!("value is {bytes} bytes, above {MAX_VALUE_BYTES}"));
        }
        let mut buffer = vec![0u16; (bytes as usize).div_ceil(2).max(1)];
        let mut size = (buffer.len() * 2) as u32;
        // SAFETY: `buffer` provides `size` writable bytes for the call; the
        // name strings are NUL-terminated as above.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                value.as_ptr(),
                flags,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        match status {
            ERROR_SUCCESS => {
                let mut units = &buffer[..(size as usize / 2).min(buffer.len())];
                while let [rest @ .., 0] = units {
                    units = rest;
                }
                if units.is_empty() {
                    return Err("value is empty".into());
                }
                return Ok(PathBuf::from(OsString::from_wide(units)));
            }
            // Expansion can grow the value between the two calls; retry.
            ERROR_MORE_DATA => continue,
            ERROR_FILE_NOT_FOUND => return Err("key or value not present".into()),
            other => return Err(format!("RegGetValueW failed with Win32 error {other}")),
        }
    }
    Err("value kept growing while being read".into())
}
