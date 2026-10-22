# First Project

This is a native Rust authored-scene example. The project manifest keeps stable
asset:player-script and entity:player IDs and points the script asset at src/lib.rs.
The adjacent Cargo.toml is a standalone game crate; the runtime compiler links its
configure function and the engine into one executable with one Rust type universe.
The original mesh, texture, scene, input action and one authored player are retained.

The Player callback initializes the game-owned PlayerStats component on its authored
owner. A pressed move_right action moves X by one unit, preserving Y/Z. The
PlayerStatsTick native system counts enabled ticks and pressed moves. Window and
injected inputs use the same ActionFrame resource. The script creates no duplicate
player. ReloadedPlayer is a separate-generation fixture that moves by ten units.

The lifecycle fixtures cover create-then-fail cleanup, teardown error reporting,
native system jobs, query iteration, deferred component removal/spawn and a later
query observing those edits on the next tick. The fixture uses native Bevy commands
and explicit ordering; it does not implement ECS storage or a schema interpreter.
The abort fixture validates a candidate before publication and returns an error,
leaving its live AbortState at zero.

Native code can mutate arbitrary heap-owning components and resources. Those
mutations are not automatically rollbackable. Callbacks should validate/stage an
operation before publishing when atomic behavior is needed. Errors stop the runtime
and run owned-entity teardown. Authored scene preflight remains separate. Script
replacement means compiling/launching a new native generation; the example does not
claim Lua-style in-process callback hot replacement or sandboxed native code.

From the repository root:

    cargo test --manifest-path examples/first-project/Cargo.toml
    cargo run -p engine-tooling --bin engine -- project validate examples/first-project/project.json
    cargo run -p engine-tooling --bin engine -- run examples/first-project/project.json --headless --ticks 120 --input move_right:0

The tests exercise the real public Runtime and EngineApp APIs. They verify owner
identity, exact positions and counters, lifecycle cleanup, error propagation and
structural publication, plus no-input, repeated-press, preserved Y/Z and stop
behavior. No Lua remains; the manifest and game crate use Rust exclusively. The
crate is its own Cargo workspace root; Cargo.lock and target are generated locally
and ignored by examples/.gitignore.

fixtures/invalid is an intentionally uncompilable standalone candidate replacing
the old invalid.lua fixture. Cargo check against that manifest must fail with a
Rust diagnostic. It is outside the valid example crate's module graph. The native
compiler must reject it while preserving a prior working generation.
