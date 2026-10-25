//! Desktop input translation, split by concern:
//!
//! * `keyboard.rs`: physical key to stable binding token ([`key_token`]).
//! * `window.rs`: winit window events (keyboard, mouse, cursor, wheel, focus)
//!   folded into the project's [`engine_project::input::RawInput`].
//! * `gamepad.rs`: polled gamepad state ([`Gamepads`]).
//! * `text.rs`: typed text, editing keys and IME translated into UI text events
//!   ([`translate_window_event`]) and the window's IME state.
//! * `latch.rs`: per-tick action frames when redraws and ticks do not coincide
//!   ([`sample_tick`], [`PressLatch`]).

mod gamepad;
mod keyboard;
mod latch;
mod text;
mod window;

pub(crate) use gamepad::Gamepads;
pub use keyboard::key_token;
pub(crate) use latch::{PressLatch, sample_tick};
pub(crate) use text::TextBridge;
#[cfg(feature = "system-clipboard")]
pub use text::clipboard;
pub use text::{
    ImeCursorArea, KeyPress, TextInputState, TextModifiers, ime_cursor_area, translate_ime,
    translate_key, translate_window_event,
};
pub(crate) use window::window_input;
