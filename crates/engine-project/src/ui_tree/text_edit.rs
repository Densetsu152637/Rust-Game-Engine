//! Text-input event model (UI contract 4). Platform layers translate typed
//! text, editing keys and IME events into [`UiTextEvent`]s and queue them with
//! `GameUi::push_text_event`; the next `GameUi` tick applies them to the
//! focused `TextInput`. Editing works on character indices.
use super::*;
use std::sync::Mutex;

/// Queued events per tick; pushing beyond it fails with `ui.text_event.overflow`.
pub const MAX_TEXT_EVENTS: usize = 256;
/// Bytes of text carried by one event.
pub const MAX_TEXT_EVENT_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiEditKey {
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    SelectAll,
    Copy,
    Cut,
    Paste,
    /// Newline in multiline inputs; submits single-line inputs.
    Enter,
    /// Leaves the input (focus is cleared, preedit discarded).
    Escape,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiEditModifiers {
    /// Extend the selection instead of collapsing it.
    pub shift: bool,
    /// Move or delete by word (Ctrl on Windows/Linux, Option on macOS).
    pub word: bool,
}

/// IME composition shown at the caret but not yet part of the value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiPreedit {
    pub text: String,
    /// Byte range of the IME cursor/selection inside `text`.
    pub cursor: Option<[usize; 2]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiTextEvent {
    /// Committed typed characters (keyboard text). Control characters other
    /// than newline (multiline only) and tab are dropped.
    Insert(String),
    Key {
        key: UiEditKey,
        modifiers: UiEditModifiers,
    },
    /// Replace the IME composition; empty text clears it.
    Preedit(UiPreedit),
    /// IME commit: clears the composition and inserts the text.
    Commit(String),
}

impl UiTextEvent {
    pub fn key(key: UiEditKey) -> Self {
        Self::Key {
            key,
            modifiers: UiEditModifiers::default(),
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        let text_ok = |t: &str| t.len() <= MAX_TEXT_EVENT_BYTES;
        let ok = match self {
            Self::Insert(t) | Self::Commit(t) => text_ok(t),
            Self::Preedit(p) => {
                text_ok(&p.text)
                    && p.cursor.is_none_or(|[a, b]| {
                        a <= b
                            && b <= p.text.len()
                            && p.text.is_char_boundary(a)
                            && p.text.is_char_boundary(b)
                    })
            }
            Self::Key { .. } => true,
        };
        if ok {
            Ok(())
        } else {
            Err(Error::new(
                "ui.text_event.invalid",
                "Text event exceeds 1024 bytes or has an invalid preedit cursor",
            ))
        }
    }
}

/// Clipboard used by copy/cut/paste. Implementations use interior mutability
/// so `GameUi` stays cloneable; the desktop runner can install the system
/// clipboard with `GameUi::set_clipboard`.
pub trait UiClipboard: Send + Sync {
    fn get(&self) -> Option<String>;
    fn set(&self, text: &str);
}

/// Process-local clipboard (the default).
#[derive(Debug, Default)]
pub struct MemoryClipboard(Mutex<String>);

impl UiClipboard for MemoryClipboard {
    fn get(&self) -> Option<String> {
        let text = self.0.lock().ok()?.clone();
        (!text.is_empty()).then_some(text)
    }
    fn set(&self, text: &str) {
        if let Ok(mut slot) = self.0.lock() {
            text.clone_into(&mut slot);
        }
    }
}

/// Mutable view of a `TextInput`'s authored state.
pub(super) struct Edit<'a> {
    pub value: &'a mut String,
    pub caret: &'a mut u32,
    pub anchor: &'a mut u32,
    pub max_length: u32,
    pub multiline: bool,
    pub password: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Outcome {
    pub changed: bool,
    pub submitted: bool,
    pub cancelled: bool,
}

fn word_start(chars: &[char], mut at: usize) -> usize {
    while at > 0 && chars[at - 1].is_whitespace() {
        at -= 1;
    }
    while at > 0 && !chars[at - 1].is_whitespace() {
        at -= 1;
    }
    at
}

fn word_end(chars: &[char], mut at: usize) -> usize {
    while at < chars.len() && chars[at].is_whitespace() {
        at += 1;
    }
    while at < chars.len() && !chars[at].is_whitespace() {
        at += 1;
    }
    at
}

fn line_start(chars: &[char], at: usize) -> usize {
    chars[..at]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |p| p + 1)
}

fn line_end(chars: &[char], at: usize) -> usize {
    chars[at..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |p| at + p)
}

impl Edit<'_> {
    fn selection(&self) -> (usize, usize) {
        let (a, b) = (*self.anchor as usize, *self.caret as usize);
        (a.min(b), a.max(b))
    }

    fn set(&mut self, chars: &[char], caret: usize, extend: bool) {
        *self.value = chars.iter().collect();
        *self.caret = caret as u32;
        if !extend {
            *self.anchor = caret as u32;
        }
    }

    /// Replace the selection with `text`, truncated to fit `max_length`.
    fn replace(&mut self, chars: &mut Vec<char>, text: &str) -> bool {
        let (start, end) = self.selection();
        let filtered: Vec<char> = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\t' || (*c == '\n' && self.multiline))
            .collect();
        let room = (self.max_length as usize).saturating_sub(chars.len() - (end - start));
        let inserted = &filtered[..filtered.len().min(room)];
        if start == end && inserted.is_empty() {
            return false;
        }
        chars.splice(start..end, inserted.iter().copied());
        self.set(chars, start + inserted.len(), false);
        true
    }

    pub fn apply(&mut self, event: &UiTextEvent, clipboard: &dyn UiClipboard) -> Outcome {
        let mut chars: Vec<char> = self.value.chars().collect();
        let len = chars.len();
        // Authored carets are validated, but clamp defensively.
        *self.caret = (*self.caret).min(len as u32);
        *self.anchor = (*self.anchor).min(len as u32);
        let caret = *self.caret as usize;
        let (start, end) = self.selection();
        let mut outcome = Outcome::default();
        let (key, modifiers) = match event {
            UiTextEvent::Insert(text) | UiTextEvent::Commit(text) => {
                outcome.changed = self.replace(&mut chars, text);
                return outcome;
            }
            UiTextEvent::Preedit(_) => return outcome,
            UiTextEvent::Key { key, modifiers } => (*key, *modifiers),
        };
        let shift = modifiers.shift;
        let moved = |edit: &mut Self, to: usize| {
            let to = to.min(len);
            *edit.caret = to as u32;
            if !shift {
                *edit.anchor = to as u32;
            }
        };
        match key {
            UiEditKey::Backspace | UiEditKey::Delete => {
                let range = if start != end {
                    start..end
                } else if key == UiEditKey::Backspace {
                    let from = if modifiers.word {
                        word_start(&chars, caret)
                    } else {
                        caret.saturating_sub(1)
                    };
                    from..caret
                } else {
                    let to = if modifiers.word {
                        word_end(&chars, caret)
                    } else {
                        (caret + 1).min(len)
                    };
                    caret..to
                };
                if !range.is_empty() {
                    let at = range.start;
                    chars.drain(range);
                    self.set(&chars, at, false);
                    outcome.changed = true;
                }
            }
            UiEditKey::Left => {
                let to = if start != end && !shift {
                    start
                } else if modifiers.word {
                    word_start(&chars, caret)
                } else {
                    caret.saturating_sub(1)
                };
                moved(self, to);
            }
            UiEditKey::Right => {
                let to = if start != end && !shift {
                    end
                } else if modifiers.word {
                    word_end(&chars, caret)
                } else {
                    caret + 1
                };
                moved(self, to);
            }
            UiEditKey::Home => {
                let to = if self.multiline {
                    line_start(&chars, caret)
                } else {
                    0
                };
                moved(self, to);
            }
            UiEditKey::End => {
                let to = if self.multiline {
                    line_end(&chars, caret)
                } else {
                    len
                };
                moved(self, to);
            }
            UiEditKey::Up | UiEditKey::Down if !self.multiline => {
                moved(self, if key == UiEditKey::Up { 0 } else { len });
            }
            UiEditKey::Up => {
                let here = line_start(&chars, caret);
                let column = caret - here;
                let to = if here == 0 {
                    0
                } else {
                    let previous = line_start(&chars, here - 1);
                    previous + column.min(here - 1 - previous)
                };
                moved(self, to);
            }
            UiEditKey::Down => {
                let here = line_start(&chars, caret);
                let column = caret - here;
                let this_end = line_end(&chars, caret);
                let to = if this_end == len {
                    len
                } else {
                    let next = this_end + 1;
                    next + column.min(line_end(&chars, next) - next)
                };
                moved(self, to);
            }
            UiEditKey::SelectAll => {
                *self.anchor = 0;
                *self.caret = len as u32;
            }
            UiEditKey::Copy | UiEditKey::Cut => {
                // Password values never leave the widget through the clipboard.
                if start != end && !self.password {
                    clipboard.set(&chars[start..end].iter().collect::<String>());
                    if key == UiEditKey::Cut {
                        chars.drain(start..end);
                        self.set(&chars, start, false);
                        outcome.changed = true;
                    }
                }
            }
            UiEditKey::Paste => {
                if let Some(text) = clipboard.get() {
                    let text: String = text.chars().take(MAX_TEXT_EVENT_BYTES).collect();
                    outcome.changed = self.replace(&mut chars, &text);
                }
            }
            UiEditKey::Enter if self.multiline => outcome.changed = self.replace(&mut chars, "\n"),
            UiEditKey::Enter => outcome.submitted = true,
            UiEditKey::Escape => outcome.cancelled = true,
        }
        outcome
    }
}
