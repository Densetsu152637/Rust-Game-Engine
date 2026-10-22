//! Child control creation and lookup.
use super::*;

impl Ui {
    pub(super) fn control(&self, id: usize) -> HWND {
        // SAFETY: This window and its child handles remain live until run exits.
        unsafe { GetDlgItem(self.window.get(), id as i32) }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn add(
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

    pub(super) fn create_controls(&self) -> Result<()> {
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
}
