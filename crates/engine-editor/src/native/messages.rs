//! WM_COMMAND and window-message dispatch.
use super::*;

impl Ui {
    pub(super) fn command(&self, id: usize, notification: u16) -> Result<()> {
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

    pub(super) fn message(
        &self,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<Option<LRESULT>> {
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
}
