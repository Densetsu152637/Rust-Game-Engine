//! The only unsafe module: HWNDs and callbacks remain on the creating thread.
//! A boxed Ui outlives its window. Shared callback access uses RefCell/Cell rather
//! than manufacturing overlapping mutable references during Win32 reentrancy.

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    time::Duration,
};

use engine_core::{
    Result,
    anyhow::{anyhow, bail, ensure},
};
use engine_project::{EditOperation, MeshRenderer};
use windows_sys::Win32::{
    Foundation::{ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject},
    System::LibraryLoader::GetModuleHandleW,
    UI::{Input::KeyboardAndMouse::EnableWindow, WindowsAndMessaging::*},
};

use crate::{EditorController, NativeSmokeReport, PlayHost};

const MANIFEST: usize = 100;
const OPEN: usize = 101;
const SAVE: usize = 102;
const RELOAD: usize = 103;
const UNDO: usize = 104;
const REDO: usize = 105;
const PLAY: usize = 106;
const STOP: usize = 107;
const ENTITIES: usize = 108;
const POS_X: usize = 109;
const POS_Y: usize = 110;
const POS_Z: usize = 111;
const APPLY_POSITION: usize = 112;
const SCRIPT: usize = 113;
const MESH: usize = 114;
const TEXTURE: usize = 115;
const LOG: usize = 116;
const STATUS: usize = 117;
const POLL_TIMER: usize = 1;
const SMOKE_TIMER: usize = 2;

#[derive(Default)]
struct Controls {
    all: Vec<HWND>,
}

struct Ui {
    controller: RefCell<EditorController>,
    initial_manifest: PathBuf,
    window: Cell<HWND>,
    controls: RefCell<Controls>,
    updating: Cell<bool>,
    smoke: bool,
    fatal: RefCell<Option<String>>,
}

fn wide(text: &str) -> Vec<u16> {
    text.replace('\0', "").encode_utf16().chain([0]).collect()
}

fn last_error(action: &str) -> engine_core::anyhow::Error {
    // SAFETY: Reading the thread-local Win32 last-error value has no preconditions.
    anyhow!("{action}: Windows error {}", unsafe { GetLastError() })
}

fn text(window: HWND) -> String {
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

fn set_text(window: HWND, value: &str) {
    // SAFETY: Win32 copies this terminated string before returning.
    unsafe {
        SetWindowTextW(window, wide(value).as_ptr());
    }
}

impl Ui {
    fn control(&self, id: usize) -> HWND {
        // SAFETY: This window and its child handles remain live until run exits.
        unsafe { GetDlgItem(self.window.get(), id as i32) }
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &self,
        class: &str,
        label: &str,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        id: usize,
    ) -> Result<()> {
        // SAFETY: Strings live through creation; no child stores pointers to them.
        // Child IDs are represented by HMENU as required for WS_CHILD windows.
        let handle = unsafe {
            CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(label).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                x,
                y,
                width,
                height,
                self.window.get(),
                id as *mut c_void,
                GetModuleHandleW(null()),
                null(),
            )
        };
        ensure!(!handle.is_null(), "{}", last_error("create editor control"));
        // SAFETY: DEFAULT_GUI_FONT is a borrowed stock object; we never delete it.
        unsafe {
            SendMessageW(
                handle,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as WPARAM,
                1,
            );
        }
        self.controls.borrow_mut().all.push(handle);
        Ok(())
    }

    fn create_controls(&self) -> Result<()> {
        self.add(
            "EDIT",
            &self.initial_manifest.to_string_lossy(),
            WS_BORDER | ES_AUTOHSCROLL as u32 | WS_TABSTOP,
            12,
            12,
            560,
            26,
            MANIFEST,
        )?;
        for (id, label, x) in [
            (OPEN, "Open", 580),
            (SAVE, "Save", 652),
            (RELOAD, "Reload", 724),
            (UNDO, "Undo", 800),
            (REDO, "Redo", 872),
        ] {
            self.add(
                "BUTTON",
                label,
                BS_PUSHBUTTON as u32 | WS_TABSTOP,
                x,
                12,
                68,
                26,
                id,
            )?;
        }
        self.add(
            "BUTTON",
            "Play",
            BS_PUSHBUTTON as u32 | WS_TABSTOP,
            800,
            46,
            68,
            26,
            PLAY,
        )?;
        self.add(
            "BUTTON",
            "Stop",
            BS_PUSHBUTTON as u32 | WS_TABSTOP,
            872,
            46,
            68,
            26,
            STOP,
        )?;
        self.add("STATIC", "Entities", 0, 12, 52, 260, 20, 0)?;
        self.add(
            "LISTBOX",
            "",
            WS_BORDER | LBS_NOTIFY as u32 | WS_VSCROLL | WS_TABSTOP,
            12,
            78,
            260,
            500,
            ENTITIES,
        )?;
        self.add("STATIC", "Position (world units)", 0, 292, 78, 250, 20, 0)?;
        for (id, label, y) in [(POS_X, "X", 110), (POS_Y, "Y", 144), (POS_Z, "Z", 178)] {
            self.add("STATIC", label, 0, 292, y + 3, 18, 20, 0)?;
            self.add(
                "EDIT",
                "0",
                WS_BORDER | ES_AUTOHSCROLL as u32 | WS_TABSTOP,
                316,
                y,
                180,
                26,
                id,
            )?;
        }
        self.add(
            "BUTTON",
            "Apply position",
            BS_PUSHBUTTON as u32 | WS_TABSTOP,
            510,
            178,
            140,
            26,
            APPLY_POSITION,
        )?;
        for (id, label, y) in [
            (SCRIPT, "Rust script asset", 232),
            (MESH, "MeshRenderer mesh", 292),
            (TEXTURE, "MeshRenderer texture", 352),
        ] {
            self.add("STATIC", label, 0, 292, y - 22, 300, 20, 0)?;
            self.add(
                "COMBOBOX",
                "",
                WS_BORDER | CBS_DROPDOWNLIST as u32 | WS_VSCROLL | WS_TABSTOP,
                292,
                y,
                500,
                240,
                id,
            )?;
        }
        self.add("STATIC", "Validation and log", 0, 292, 395, 300, 20, 0)?;
        self.add(
            "EDIT",
            "",
            WS_BORDER
                | ES_MULTILINE as u32
                | ES_AUTOVSCROLL as u32
                | ES_READONLY as u32
                | WS_VSCROLL,
            292,
            418,
            648,
            160,
            LOG,
        )?;
        self.add("STATIC", "No project open", 0, 12, 596, 928, 40, STATUS)?;
        Ok(())
    }

    fn fill_picker(&self, id: usize, kind: &str, selected: Option<&str>) {
        let picker = self.control(id);
        // SAFETY: Combo box messages are synchronous and string data is copied.
        unsafe {
            SendMessageW(picker, CB_RESETCONTENT, 0, 0);
            SendMessageW(picker, CB_ADDSTRING, 0, wide("(none)").as_ptr() as LPARAM);
            SendMessageW(picker, CB_SETCURSEL, 0, 0);
            if let Some(document) = self.controller.borrow().document() {
                for (index, asset) in document
                    .project()
                    .assets
                    .values()
                    .filter(|a| a.kind == kind)
                    .enumerate()
                {
                    SendMessageW(
                        picker,
                        CB_ADDSTRING,
                        0,
                        wide(&format!("{} | {}", asset.id, asset.path.display())).as_ptr()
                            as LPARAM,
                    );
                    if selected == Some(asset.id.as_str()) {
                        SendMessageW(picker, CB_SETCURSEL, index + 1, 0);
                    }
                }
            }
        }
    }

    fn selected_asset(&self, id: usize, kind: &str) -> Option<String> {
        // SAFETY: Message targets a live combo box on this thread.
        let index = unsafe { SendMessageW(self.control(id), CB_GETCURSEL, 0, 0) };
        if index <= 0 {
            return None;
        }
        self.controller
            .borrow()
            .document()?
            .project()
            .assets
            .values()
            .filter(|a| a.kind == kind)
            .nth(index as usize - 1)
            .map(|a| a.id.clone())
    }

    fn refresh(&self) {
        self.updating.set(true);
        {
            let controller = self.controller.borrow();
            let title = format!(
                "Rust Game Engine Editor{}",
                if controller.is_dirty() { " *" } else { "" }
            );
            set_text(self.window.get(), &title);
            set_text(self.control(STATUS), controller.status());
            let lines = controller
                .diagnostics()
                .iter()
                .chain(controller.log().iter())
                .cloned()
                .collect::<Vec<_>>()
                .join("\r\n");
            set_text(self.control(LOG), &lines);
            // SAFETY: All IDs below are live child controls; only copied strings
            // and scalar selection indices cross this synchronous message boundary.
            unsafe {
                SendMessageW(self.control(ENTITIES), LB_RESETCONTENT, 0, 0);
                if let Some(document) = controller.document() {
                    for (index, entity) in document.snapshot().entities.iter().enumerate() {
                        SendMessageW(
                            self.control(ENTITIES),
                            LB_ADDSTRING,
                            0,
                            wide(&format!("{} | {}", entity.name, entity.id)).as_ptr() as LPARAM,
                        );
                        if controller.selected() == Some(entity.id.as_str()) {
                            SendMessageW(self.control(ENTITIES), LB_SETCURSEL, index, 0);
                        }
                    }
                }
                for id in [SAVE, RELOAD, UNDO, REDO] {
                    EnableWindow(self.control(id), i32::from(controller.document().is_some()));
                }
                EnableWindow(
                    self.control(PLAY),
                    i32::from(controller.document().is_some() && !controller.is_playing()),
                );
                EnableWindow(self.control(STOP), i32::from(controller.is_playing()));
            }
        }
        let entity = {
            let controller = self.controller.borrow();
            controller.document().and_then(|document| {
                document
                    .snapshot()
                    .entities
                    .iter()
                    .find(|e| Some(e.id.as_str()) == controller.selected())
                    .cloned()
            })
        };
        let position = entity
            .as_ref()
            .and_then(|e| e.components.transform.as_ref())
            .map(|t| t.position)
            .unwrap_or([0.0; 3]);
        for (id, value) in [POS_X, POS_Y, POS_Z].into_iter().zip(position) {
            set_text(self.control(id), &value.to_string());
        }
        self.fill_picker(
            SCRIPT,
            "script",
            entity
                .as_ref()
                .and_then(|e| e.components.script.as_ref())
                .map(|s| s.asset.as_str()),
        );
        self.fill_picker(
            MESH,
            "mesh",
            entity
                .as_ref()
                .and_then(|e| e.components.mesh_renderer.as_ref())
                .map(|m| m.mesh.as_str()),
        );
        self.fill_picker(
            TEXTURE,
            "texture",
            entity
                .as_ref()
                .and_then(|e| e.components.mesh_renderer.as_ref())
                .and_then(|m| m.texture.as_deref()),
        );
        // SAFETY: Controls exist until the parent window is destroyed.
        unsafe {
            for id in [POS_X, POS_Y, POS_Z, APPLY_POSITION, SCRIPT, MESH] {
                EnableWindow(self.control(id), i32::from(entity.is_some()));
            }
            EnableWindow(
                self.control(TEXTURE),
                i32::from(
                    entity
                        .as_ref()
                        .is_some_and(|e| e.components.mesh_renderer.is_some()),
                ),
            );
        }
        self.updating.set(false);
    }

    fn confirm_replace(&self) -> Result<Option<bool>> {
        if !self.controller.borrow().is_dirty() {
            return Ok(Some(false));
        }
        if self.smoke {
            return Ok(Some(true));
        }
        // SAFETY: Modal UI is created on the owning thread. No mutable controller
        // borrow is held while it pumps nested messages.
        let choice = unsafe {
            MessageBoxW(
                self.window.get(),
                wide("Save unsaved scene edits before continuing?").as_ptr(),
                wide("Unsaved scene edits").as_ptr(),
                MB_YESNOCANCEL | MB_ICONWARNING | MB_TASKMODAL,
            )
        };
        match choice {
            IDYES => {
                self.controller.borrow_mut().save()?;
                Ok(Some(false))
            }
            IDNO => Ok(Some(true)),
            _ => Ok(None),
        }
    }

    fn edit(&self, id: usize) -> Result<()> {
        let (entity_id, revision) = {
            let controller = self.controller.borrow();
            (
                controller
                    .selected()
                    .ok_or_else(|| anyhow!("select an entity first"))?
                    .to_owned(),
                controller
                    .document()
                    .ok_or_else(|| anyhow!("no project open"))?
                    .revision()
                    .to_owned(),
            )
        };
        let operation = match id {
            APPLY_POSITION => {
                let mut position = [0.0; 3];
                for (value, control) in position.iter_mut().zip([POS_X, POS_Y, POS_Z]) {
                    *value = text(self.control(control))
                        .trim()
                        .parse::<f32>()
                        .map_err(|_| anyhow!("position must contain finite numbers"))?;
                    ensure!(value.is_finite(), "position must contain finite numbers");
                }
                EditOperation::SetPosition {
                    entity_id,
                    position,
                }
            }
            SCRIPT => EditOperation::SetScript {
                entity_id,
                asset: self.selected_asset(SCRIPT, "script"),
            },
            MESH | TEXTURE => {
                let mesh = self.selected_asset(MESH, "mesh");
                let texture = self.selected_asset(TEXTURE, "texture");
                EditOperation::SetMeshRenderer {
                    entity_id,
                    value: mesh.map(|mesh| MeshRenderer {
                        version: 1,
                        mesh,
                        texture,
                    }),
                }
            }
            _ => bail!("unknown inspector field"),
        };
        self.controller.borrow_mut().apply(operation, &revision)
    }

    fn command(&self, id: usize, notification: u16) -> Result<()> {
        if self.updating.get() {
            return Ok(());
        }
        match id {
            OPEN => {
                if let Some(discard) = self.confirm_replace()? {
                    self.controller
                        .borrow_mut()
                        .open(PathBuf::from(text(self.control(MANIFEST))), discard)?;
                }
            }
            RELOAD => {
                if let Some(discard) = self.confirm_replace()? {
                    self.controller.borrow_mut().reload(discard)?;
                }
            }
            SAVE => self.controller.borrow_mut().save()?,
            UNDO => self.controller.borrow_mut().undo()?,
            REDO => self.controller.borrow_mut().redo()?,
            PLAY => self.controller.borrow_mut().play()?,
            STOP => self.controller.borrow_mut().stop()?,
            ENTITIES if notification == LBN_SELCHANGE as u16 => {
                // SAFETY: This is a live listbox; an absent selection is LB_ERR.
                let index = unsafe { SendMessageW(self.control(ENTITIES), LB_GETCURSEL, 0, 0) };
                let selected = if index < 0 {
                    None
                } else {
                    self.controller
                        .borrow()
                        .document()
                        .and_then(|d| d.snapshot().entities.get(index as usize))
                        .map(|e| e.id.clone())
                };
                self.controller.borrow_mut().select(selected)?;
            }
            APPLY_POSITION => self.edit(APPLY_POSITION)?,
            SCRIPT | MESH | TEXTURE if notification == CBN_SELCHANGE as u16 => self.edit(id)?,
            _ => return Ok(()),
        }
        self.refresh();
        Ok(())
    }

    fn message(&self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Result<Option<LRESULT>> {
        match message {
            WM_CREATE => {
                self.create_controls()?;
                let _ = self
                    .controller
                    .borrow_mut()
                    .open(&self.initial_manifest, false);
                self.refresh();
                // SAFETY: Timer belongs to this live window; callback uses WM_TIMER.
                ensure!(
                    unsafe { SetTimer(self.window.get(), POLL_TIMER, 100, None) } != 0,
                    "{}",
                    last_error("start editor polling timer")
                );
                Ok(Some(0))
            }
            WM_COMMAND => {
                self.command(wparam & 0xffff, ((wparam >> 16) & 0xffff) as u16)?;
                Ok(Some(0))
            }
            WM_CLOSE => {
                if let Some(discard) = self.confirm_replace()? {
                    self.controller.borrow_mut().close(discard)?;
                    // SAFETY: Destroying our window is confined to its owner thread.
                    unsafe {
                        DestroyWindow(self.window.get());
                    }
                }
                Ok(Some(0))
            }
            WM_TIMER if wparam == POLL_TIMER => {
                let was_playing = self.controller.borrow().is_playing();
                if was_playing {
                    let _ = self.controller.borrow_mut().poll();
                    // Avoid replacing inspector text while the user is typing.
                    set_text(self.control(STATUS), self.controller.borrow().status());
                    if !self.controller.borrow().is_playing() {
                        self.refresh();
                    }
                }
                Ok(Some(0))
            }
            WM_TIMER if wparam == SMOKE_TIMER => {
                // SAFETY: Posting close does not borrow controller state reentrantly.
                unsafe {
                    KillTimer(self.window.get(), SMOKE_TIMER);
                    PostMessageW(self.window.get(), WM_CLOSE, 0, 0);
                }
                Ok(Some(0))
            }
            WM_GETMINMAXINFO => {
                // SAFETY: Win32 supplies a writable MINMAXINFO for this message.
                unsafe {
                    let limits = &mut *(lparam as *mut MINMAXINFO);
                    limits.ptMinTrackSize.x = 990;
                    limits.ptMinTrackSize.y = 700;
                }
                Ok(Some(0))
            }
            WM_DESTROY => {
                unsafe {
                    KillTimer(self.window.get(), POLL_TIMER);
                    PostQuitMessage(0);
                }
                Ok(Some(0))
            }
            _ => Ok(None),
        }
    }

    fn exercise_controls(&self) -> Result<NativeSmokeReport> {
        ensure!(
            self.controller.borrow().document().is_some(),
            "smoke project failed to open: {}",
            self.controller.borrow().status()
        );
        let count = self
            .controller
            .borrow()
            .document()
            .unwrap()
            .snapshot()
            .entities
            .len();
        ensure!(
            count > 0,
            "native smoke requires at least one authored entity"
        );
        let mut rectangle = RECT::default();
        // SAFETY: RECT is valid and all stored handles are children of this window.
        unsafe {
            for handle in &self.controls.borrow().all {
                ensure!(IsWindow(*handle) != 0, "native child control is invalid");
                ensure!(
                    GetWindowRect(*handle, &mut rectangle) != 0
                        && rectangle.right > rectangle.left
                        && rectangle.bottom > rectangle.top,
                    "native control has empty geometry"
                );
            }
            SendMessageW(self.control(ENTITIES), LB_SETCURSEL, 0, 0);
            SendMessageW(
                self.window.get(),
                WM_COMMAND,
                ENTITIES | ((LBN_SELCHANGE as usize) << 16),
                self.control(ENTITIES) as LPARAM,
            );
        }
        let initial_revision = self
            .controller
            .borrow()
            .document()
            .unwrap()
            .revision()
            .to_owned();
        let x = text(self.control(POS_X)).parse::<f32>()? + 1.0;
        set_text(self.control(POS_X), &x.to_string());
        // SAFETY: Exercises the same native WM_COMMAND dispatch as a user click.
        unsafe {
            SendMessageW(self.window.get(), WM_COMMAND, APPLY_POSITION, 0);
        }
        ensure!(
            self.controller.borrow().document().unwrap().revision() != initial_revision,
            "position command did not edit the selected entity"
        );
        unsafe {
            SendMessageW(self.window.get(), WM_COMMAND, UNDO, 0);
        }
        ensure!(
            self.controller.borrow().document().unwrap().revision() == initial_revision,
            "native undo did not restore the authored scene"
        );
        let mut picker_edit_applied = false;
        for picker in [SCRIPT, MESH, TEXTURE] {
            // SAFETY: Each picker is a live native combobox.
            unsafe {
                if SendMessageW(self.control(picker), CB_GETCOUNT, 0, 0) > 1 {
                    let selected = SendMessageW(self.control(picker), CB_GETCURSEL, 0, 0);
                    let choice = if selected == 1 && picker == SCRIPT {
                        0
                    } else {
                        1
                    };
                    SendMessageW(self.control(picker), CB_SETCURSEL, choice, 0);
                    SendMessageW(
                        self.window.get(),
                        WM_COMMAND,
                        picker | ((CBN_SELCHANGE as usize) << 16),
                        self.control(picker) as LPARAM,
                    );
                    ensure!(
                        self.controller.borrow().diagnostics().is_empty(),
                        "native picker edit failed: {:?}",
                        self.controller.borrow().diagnostics()
                    );
                    picker_edit_applied = true;
                }
            }
        }
        Ok(NativeSmokeReport {
            controls_created: self.controls.borrow().all.len(),
            entities_listed: count,
            picker_edit_applied,
            window_closed: false,
        })
    }
}

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
