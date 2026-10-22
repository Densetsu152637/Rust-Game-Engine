# Desktop 2D synthetic lab

This native Rust example demonstrates engine desktop capabilities with original
fixture artwork, glyphs and sine tones. It is not Unsealed narrative or production
gameplay. Male/female are placeholder IDs sharing the same route. Developer passage
A is original test text; no scripture is included. Existing scenes, input mappings,
asset IDs, textures, font metrics and PCM16 tones are retained.

The adjacent standalone Cargo crate exports configure(&mut EngineApp). It registers
native script factories by the same authored asset IDs used by the scenes.
progression.rs is game-owned pure progression; controller.rs connects it to the
engine's public collision, trigger, UI, persistence, audio and scene APIs. It does
not reproduce engine service implementations. No Lua remains; active manifests
reference Rust sources only.

The crate is its own Cargo workspace root (empty [workspace] table), so the engine
workspace does not build it and its Cargo.lock/target are generated locally and
ignored by examples/.gitignore. Engine crates are path dependencies; serde and
tempfile are pinned exactly.

## Run and verify

From the repository root:

    cargo test --manifest-path examples/desktop-2d/Cargo.toml
    cargo fmt --check --manifest-path examples/desktop-2d/Cargo.toml
    cargo clippy --manifest-path examples/desktop-2d/Cargo.toml --all-targets -- -D warnings
    python examples/desktop-2d/tools/validate_fixture.py
    cargo run -p engine-cli -- run examples/desktop-2d/project.json --ticks 36000 --user-data build/desktop-example-user

The native compiler entry uses the manifest's rust.manifest Cargo.toml. First native
compilation may take minutes. After the engine CLI is built, the end-to-end runner
also verifies its compilation/launch/JSON contract and retains stdout/stderr:

    python examples/desktop-2d/tools/run_walkthrough.py --engine PATH_TO_ENGINE --artifacts build/desktop-example-check --timeout 600

Use a fresh artifacts directory for each independent new walkthrough. The Rust
integration suite runs each male/female new session and restore session in separate
processes with isolated repository-local temporary user-data directories. A negative
fixture omits ui_confirm:6 and must fail, so reaching the tick limit is insufficient.
The driver binary lab-walkthrough uses the same configure function and scripts as
native visible play; it calls real Runtime services and mixes audio offline.

The walkthrough checks locked text concealment, solid sweep contacts, stable trigger
overlaps/events, modal input masking and release quarantine, selection/confirm/cancel,
premature and duplicate quest awards, midquest durable save/read, reward provenance,
scene cancellation without healing, ordinary defeat, three seconds of fixed update
time before exactly one authored healing, complete rebindings/settings, room
transitions with followers, missing-scene rejection, and teardown without audio
voices. Both restore processes verify room B, exact position (3.25,-2.25), accepted
choice 2, one healing completion, and both quest/story passage provenance before
applying the saved position in the accepted destination room.

Native traversal retains the player's Entity and script instance. The controller
observes accepted room changes, reloads its display/music lifecycle from shared
GameState and applies pending saved placement only in the target room. Saved data
uses stable authored IDs, never transient ECS handles. Progress version 1 migrates
to version 2, including legacy boolean-ID maps; unsupported versions, unknown passage
IDs and malformed coordinates/choices reject. Mutation is game-owned typed state;
progress publication validates its serializable snapshot through the engine store.

## Automated coverage

All tests are headless and need no window, GPU or audio device. src/progression.rs
unit tests cover the quest/reward/story/defeat/healing state machine, idempotence,
provenance, version-1 migration, legacy boolean-ID maps and rejection of unsupported
versions, unknown IDs and malformed checkpoints. tests/gameplay.rs drives the real
Runtime with injected actions: no-input, cardinal, opposite and normalized diagonal
movement at 3 units/s, analog scaling, preserved Z, determinism, solid sweeps, modal
and authored-scene input locks, followers, dialogue/quest/variant flows, the full
cross-room quest route, ordinary defeat versus the three-second authored healing,
new-game, save/restore (new then load in a second runtime lifetime), corrupt-save and
unsafe-position refusal, rebinding persistence and offline audio ownership.
tests/walkthrough.rs runs the lab-walkthrough driver in separate processes for
new/restore of both variants plus negative fixtures.

## Controls and manual walkthrough

| Action | Keyboard / pointer | Gamepad |
| --- | --- | --- |
| Move | WASD / arrows | Left stick |
| Inspect marker | E; Q after F remap | A |
| Open developer page | J | Y |
| Rest/save at green marker | R / inspect | X / A |
| Load save / new session | L / N | Back / none |
| Choose placeholder variant | P | none |
| Developer story reveal / injury / defeat | T / H / B | none |
| Save remapping / mute | F / M | none |
| Confirm / back | Enter / Escape | A / B |
| UI selection / scroll | Up/Down, wheel, click | Dpad |

Move against borders and the raised obstacle. Inspect the yellow marker for locked
text, reveal it with T, then scroll the original long page including Ω and 十.
Accept the orange marker quest, visit the lower objective in room B, return and
record its reward. Rest at the green marker; success displays SAVE CONFIRMED and
plays a cue only after durable write success. Failure displays SAVE FAILED.
Restart and press L to restore room/position/choice/progression. H starts a
cancellable three-second descent; Escape cancels without healing, B ordinary
defeat never heals, and completing H again cannot duplicate its award/followers.
F saves a complete map with inspect on Q, music .25 and reduced motion; M mutes.

Inspect UI glyphs, text focus, clipping and scaling at small windows. Resize,
minimize/restore, release held controls before returning focus and neutralize a
reconnected gamepad. These native-device/visual journeys and subjective audio
remain manual acceptance work; headless tests do not establish them.

## Assets and evidence boundaries

assets/provenance.json records original fixture files. tools/generate_assets.py
reproduces sprite/font PNG and WAV content with Python's standard library. The
font covers printable ASCII, Ω and 十; it is not a universal font. Static checks
verify paths, stable references, atlas bounds, PNG CRC/alpha and PCM16 headers.
All fixture assets and Rust game code use the engine's MIT license.

The native audio backend is checked separately by engine-audio's explicit device
test. This example's deterministic tests establish offline mixed output and voice
ownership, not physical audibility. The separate examples/native-extension crate
verifies arbitrary native components/resources/bundles/systems and a custom wgpu
render pass through actual offscreen pixel readback.