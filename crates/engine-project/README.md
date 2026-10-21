# Authored projects and runtime

`engine-project` keeps schema-1 authored JSON, stable asset/entity IDs, native scene
components, deterministic collision/input/UI/persistence services, and CPU graphics
separate from the window/GPU backend. Script asset paths now name `.rs` sources.
Native Rust code is trusted executable code and has full access to the Bevy world.

Load with `Project::load(manifest)` and prepare with `Runtime::new(project)`. Configure
custom component systems through `runtime.app_mut()`. Register each attached script
with `register_script(app, asset_id, factory)`, where a factory returns a
`Box<dyn NativeScript>`. `start`, `tick(dt, InputSnapshot)`, and `stop` call lifecycle
callbacks; missing factories fail startup. All ECS storage and native system
scheduling belong to Bevy through engine-core. Callbacks provide entity lifecycle
integration, not a second ECS scheduler.

Public CPU rendering types are `content::{Vertex, Mesh, Texture}` and
`runtime::{Draw, DrawSpace, DrawState, BlendMode, RenderFrame}`. World geometry is
positioned in world units, UI geometry in top-left logical pixels. Atlas rectangles
use top-left pixels. RGBA textures and unchanged positioned geometry retain their
Arc identity across extraction. World and UI draws have deterministic layer/ID
ordering; mesh draws retain depth testing/writes and opaque blending, sprites/text
use straight alpha. Camera center already resolves entity following.

`SceneDocument` owns revision-checked editable scene snapshots, history, atomic save,
reload, and isolated play-project clones. It never serializes arbitrary native ECS
state into the authored document. Scene transitions validate destination data,
bindings, colliders and safe spawn before retiring the old authored scene. A retained
traveller keeps its Entity and arbitrary custom components. Native callback effects
are not sandboxed or reversible: lifecycle failure stops the runtime generation;
it cannot promise to roll back arbitrary user Rust code.

`InputSnapshot` aliases `ActionFrame` (pressed/held/released/value, logical pointer,
wheel and focus). Physical input uses canonical binding tokens through ActionMapper;
focus/rebind/gamepad reconnect quarantine held controls. `input::merge_input` merges
one-tick injected actions with physical holds. `runtime::InputSettings` is a resource
whose `rebind` increments a revision for the host to synchronize.

Collision is swept AABB with positive-area overlap, deterministic penetration
resolution, layer/mask filtering, sorted trigger phases, safe-position search and
follow/flock helpers. UI has bounded Unicode text, choices, focus, scrolling,
modal control capture and suppressed releases after closing.

Persistence roots are host-selected. Save/settings directories are separate; JSON
envelopes have schema/content versions and bounded size/depth. Cross-process slot
locks protect writes. A flushed temporary file replaces atomically; explicit backup
read/recover never silently rewrites a corrupt primary. Named failure stages support
deterministic durability tests.

Validation: `cargo test -p engine-project`, `cargo fmt --check`, and
`cargo clippy -p engine-project --all-targets -- -D warnings`. Native window/GPU/audio
device and external compiler execution require separate integration validation.
