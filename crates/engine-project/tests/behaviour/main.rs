//! Behavioural simulations of the authored runtime: manifests, collision, input,
//! UI, scene transitions, persistence, documents, audio and render extraction.
//!
//! Table macros expand one distinct `#[test]` per case; every case carries its own
//! inputs and a real assertion in the shared checker.

/// `table!(checker; name => (args...); ...)` expands to one test per row.
macro_rules! table {
    ($checker:ident; $($name:ident => ($($arg:expr),* $(,)?));* $(;)?) => {
        $( #[test] fn $name() { $checker($($arg),*); } )*
    };
}

mod common;

mod assets;
mod audio;
mod collision;
mod document;
mod input;
mod persistence;
mod project;
mod render;
mod runtime;
mod scene;
mod ui;
