//! Desktop input translation, split by concern:
//!
//! * `keyboard.rs`: physical key to stable binding token ([`key_token`]).
//! * `window.rs`: winit window events (keyboard, mouse, cursor, wheel, focus)
//!   folded into the project's [`engine_project::input::RawInput`].
//! * `gamepad.rs`: polled gamepad state ([`Gamepads`]).

mod gamepad;
mod keyboard;
mod window;

pub(crate) use gamepad::Gamepads;
pub use keyboard::key_token;
pub(crate) use window::window_input;
