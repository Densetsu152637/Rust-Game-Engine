# Game template

Copy this directory to start a game that depends on the engine through one Cargo
dependency (`engine`), pinned to a release tag. See `docs/using-the-engine.md` in the
engine repository.

1. Copy `templates/game` out of the engine checkout (at the tag you want) and rename
   the package in `Cargo.toml` and the `my_game` path in `src/main.rs`.
2. Keep the `tag` in `Cargo.toml` equal to the engine version you copied from.
3. `cargo run` opens the window. `cargo test` runs your tests.

Layout: `project.json` (manifest), `scenes/main.scene.json` (startup scene),
`src/lib.rs` (`configure`, scripts), `src/main.rs` (desktop entry point).
