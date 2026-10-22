//! Modal confirmation prompts.
use super::*;

impl Ui {
    pub(super) fn confirm_replace(&self) -> Result<Option<bool>> {
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
}
