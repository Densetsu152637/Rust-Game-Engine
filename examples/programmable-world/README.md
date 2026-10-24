# Programmable world

A minimal reproducible integration example, not a finished game. One authored
scene combines a perspective camera, four lit cube meshes, a retained 2D HUD,
typed native components and event callbacks, rigid-body simulation, clip
animation, and waypoint navigation.

Run from the repository root:

```text
cargo run --manifest-path examples/programmable-world/Cargo.toml
cargo test --manifest-path examples/programmable-world/Cargo.toml
cargo run --manifest-path examples/programmable-world/Cargo.toml --features desktop -- --window
```

The default executable runs 241 fixed simulation ticks without opening a window,
injects one real HUD click, extracts a combined 3D/HUD frame, and prints the
score and observed events. `--headless` explicitly selects this mode. `--window`
requires the optional `desktop` feature and a working graphics adapter. The
desktop dependency stays optional so the default integration test needs no GPU
or window system. Closing the window stops the executable.

In the window, click **ADD SCORE**, or press Tab then Enter. The orange cube
falls and collides with the fixed floor. The blue cube bobs and rotates from an
authored clip. The green cube follows two world-space waypoints and stops.
The final positions persist; restarting the example resets the scene.

## What is connected

`project.json` declares assets and input bindings. `scenes/main.scene.json`
contains the complete scene, including `Camera3D`, `Camera2D` logical HUD size,
`MeshRenderer`, `MeshMaterial`, and the engine's namespaced simulation/UI
components. Builtin descriptors and native adapters come from the runtime.

`components.json` declares `game.Score`, its bounded integer schema, and its
dependency on `engine.UiRoot`. `src/lib.rs::configure` registers the actual Rust
`Score` component and two callback functions. The authored HUD's `EventBindings`
connect `ui.clicked` to `add_score`. That callback changes the typed score,
the HUD label, and a progress meter. These changes are visible in runtime scene
snapshots. No code is generated or evaluated from a callback string.

The simulation callback counts `physics.contact.start`, `animation.event`, and
`navigation.arrived`. Separate entities own physics, animation, and navigation
so they do not fight over one transform. Navigation follows an authored path;
this example does not claim obstacle avoidance or runtime path planning.

`tests/showcase.rs` loads the real project and uses normal `Runtime::tick`
calls. It checks next-tick UI callback delivery, score/label mutation, falling
body displacement, animation sampling, waypoint movement and arrival, contact
and animation event callbacks, snapshot persistence, and extraction of both
the 3D meshes and textured HUD glyphs. GPU pixel output and interactive window
behavior require separate visible verification.

## Assets and edits

The unit cube and animation clip are original plain-text example assets. The
bitmap font and atlas are copied unchanged from this repository's
`examples/desktop-2d/assets` fixture. No downloaded asset or external service is
needed.

Use the normal MCP component catalog/scene-edit tools or the editor's component
inspector to change UI nodes, materials, physics parameters, clip bindings,
waypoints, or the declared score. Change `src/lib.rs` and rebuild to implement
new native callbacks. The external compiler can load this project's ordinary
`configure(&mut EngineApp)` entrypoint just like the other native examples.
