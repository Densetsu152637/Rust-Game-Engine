//! Window procedure, class registration, and the message loop.
use super::*;

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: WM_NCCREATE supplies our boxed Ui pointer. It stays allocated until
    // after DestroyWindow returns. All other accesses are shared, on this thread.
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            if create.lpCreateParams.is_null() {
                return 0;
            }
            let ui = &*(create.lpCreateParams as *const Ui);
            ui.window.set(window);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
            return 1;
        }
        let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *const Ui;
        if !pointer.is_null() {
            let ui = &*pointer;
            if message == WM_NCDESTROY {
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            }
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ui.message(message, wparam, lparam)
            }));
            match outcome {
                Ok(Ok(Some(result))) => return result,
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    if message == WM_CREATE {
                        *ui.fatal.borrow_mut() = Some(error.to_string());
                        return -1;
                    }
                    ui.controller
                        .borrow_mut()
                        .record_error(format!("{error:#}"));
                    ui.refresh();
                    return 0;
                }
                Err(_) => {
                    *ui.fatal.borrow_mut() = Some("native editor callback panicked".into());
                    PostQuitMessage(1);
                    return 0;
                }
            }
        }
        DefWindowProcW(window, message, wparam, lparam)
    }
}

pub(crate) fn run(
    manifest: &Path,
    host: Box<dyn PlayHost>,
    smoke: Option<Duration>,
) -> Result<NativeSmokeReport> {
    let ui = Box::new(Ui {
        controller: RefCell::new(EditorController::new(host)),
        initial_manifest: manifest.to_path_buf(),
        window: Cell::new(null_mut()),
        controls: RefCell::new(Controls::default()),
        updating: Cell::new(false),
        smoke: smoke.is_some(),
        fatal: RefCell::new(None),
    });
    let class = wide("RustGameEngineEditor");
    // SAFETY: The callback and class name are valid for this process; Ui remains
    // boxed and alive through the message loop and final window destruction.
    unsafe {
        let instance = GetModuleHandleW(null());
        let registration = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as *mut c_void,
            ..Default::default()
        };
        if RegisterClassW(&registration) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(last_error("register editor window"));
        }
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Rust Game Engine Editor").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1000,
            710,
            null_mut(),
            null_mut(),
            instance,
            (&*ui as *const Ui).cast(),
        );
        if window.is_null() {
            return Err(anyhow!(
                "{}",
                ui.fatal
                    .borrow_mut()
                    .take()
                    .unwrap_or_else(|| last_error("create editor window").to_string())
            ));
        }
        ShowWindow(window, SW_SHOW);
        let mut report = if let Some(duration) = smoke {
            match ui.exercise_controls() {
                Ok(report) => {
                    if SetTimer(
                        window,
                        SMOKE_TIMER,
                        duration.as_millis().clamp(1, 2000) as u32,
                        None,
                    ) == 0
                    {
                        let error = last_error("start bounded native smoke timer");
                        DestroyWindow(window);
                        return Err(error);
                    }
                    report
                }
                Err(error) => {
                    DestroyWindow(window);
                    return Err(error);
                }
            }
        } else {
            NativeSmokeReport {
                controls_created: ui.controls.borrow().all.len(),
                entities_listed: 0,
                picker_edit_applied: false,
                window_closed: false,
            }
        };
        let mut message = MSG::default();
        let loop_result = loop {
            let result = GetMessageW(&mut message, null_mut(), 0, 0);
            if result == -1 {
                break Err(last_error("editor message loop"));
            }
            if result == 0 {
                break Ok(());
            }
            if IsDialogMessageW(window, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        };
        if IsWindow(window) != 0 {
            DestroyWindow(window);
        }
        report.window_closed = IsWindow(window) == 0;
        loop_result?;
        if let Some(error) = ui.fatal.borrow_mut().take() {
            bail!(error);
        }
        Ok(report)
    }
}
