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

mod controls;
mod dialogs;
mod inspector;
mod messages;
mod smoke;
mod win;
mod window;

use win::{last_error, set_text, text, wide};
pub(crate) use window::run;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_are_nul_terminated_utf16() {
        assert_eq!(wide("Ab"), [0x41, 0x62, 0]);
        assert_eq!(wide(""), [0]);
    }

    #[test]
    fn wide_encodes_astral_characters_as_surrogate_pairs() {
        assert_eq!(wide("\u{1f600}"), [0xd83d, 0xde00, 0]);
    }

    #[test]
    fn wide_strips_embedded_nuls_so_win32_never_sees_a_short_string() {
        let value = wide("a\0b");
        assert_eq!(value, [0x61, 0x62, 0]);
        assert_eq!(value.iter().filter(|c| **c == 0).count(), 1);
    }

    #[test]
    fn control_identifiers_are_unique() {
        let ids = [
            MANIFEST,
            OPEN,
            SAVE,
            RELOAD,
            UNDO,
            REDO,
            PLAY,
            STOP,
            ENTITIES,
            POS_X,
            POS_Y,
            POS_Z,
            APPLY_POSITION,
            SCRIPT,
            MESH,
            TEXTURE,
            LOG,
            STATUS,
        ];
        let unique: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }
}
