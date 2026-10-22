use super::*;

pub(super) fn wide(text: &str) -> Vec<u16> {
    text.replace('\0', "").encode_utf16().chain([0]).collect()
}

pub(super) fn last_error(action: &str) -> engine_core::anyhow::Error {
    // SAFETY: Reading the thread-local Win32 last-error value has no preconditions.
    anyhow!("{action}: Windows error {}", unsafe { GetLastError() })
}

pub(super) fn text(window: HWND) -> String {
    // SAFETY: All handles are owned by the current Ui on this thread. The buffer
    // includes space for the terminator and remains alive for the synchronous call.
    unsafe {
        let length = GetWindowTextLengthW(window).max(0) as usize;
        let mut buffer = vec![0_u16; length + 1];
        let copied =
            GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32).max(0) as usize;
        String::from_utf16_lossy(&buffer[..copied])
    }
}

pub(super) fn set_text(window: HWND, value: &str) {
    // SAFETY: Win32 copies this terminated string before returning.
    unsafe {
        SetWindowTextW(window, wide(value).as_ptr());
    }
}
