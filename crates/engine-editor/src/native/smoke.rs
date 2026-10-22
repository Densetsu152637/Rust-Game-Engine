//! Bounded native smoke exercise of the control command paths.
use super::*;

impl Ui {
    pub(super) fn exercise_controls(&self) -> Result<NativeSmokeReport> {
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
