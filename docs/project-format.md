# Project and scene format, version 1

The authoring contract is JSON, parsed and validated by `engine-project`
(`Project::load`, `load_scene`, `Scene::validate`). A project directory contains a
`project.json` manifest, a startup scene, source assets and, for gameplay, an
adjacent Rust crate (see [Native game crates](scripting.md)). Generated data belongs
in a derived directory such as `.derived/`.

## Manifest

Required fields are `schema` (must be `1`), `name`, `startup_scene` and `assets`.
Optional fields are `rust`, `inputActions` and `inputBindings`. Unknown fields are
rejected rather than dropped.

```json
{
  "schema": 1,
  "name": "First Project",
  "rust": { "manifest": "Cargo.toml" },
  "startup_scene": "scenes/main.scene.json",
  "inputActions": { "move_right": "Right" },
  "assets": [
    { "id": "asset:player-script", "path": "src/lib.rs", "kind": "script" },
    { "id": "asset:player-mesh", "path": "assets/player.mesh", "kind": "mesh" },
    { "id": "asset:player-texture", "path": "assets/player.ppm", "kind": "texture" }
  ]
}
```

- `rust.manifest` selects the external gameplay crate's `Cargo.toml`. It defaults to
  an adjacent `Cargo.toml`; an explicit value must exist inside the project root.
- `inputActions` maps an action name to one binding token. A bare name such as
  `Right` is shorthand for the keyboard token `Key:Right`.
- `inputBindings` maps an action name to 1-16 unique physical tokens, for example
  `{"move_up": ["Key:W", "Gamepad:LeftYNegative"]}`. At most 256 actions are
  accepted. An explicit `inputBindings` entry overrides the `inputActions` entry
  for the same action. Tokens come from the shared binding registry (keyboard keys,
  mouse buttons and wheel, gamepad buttons and signed axes); see
  [Input and UI](desktop-input-ui.md).

### Assets

Each asset has a stable `id`, a project-relative `path` and a `kind`. Kinds are
`script`, `mesh`, `texture`, `font`, `font_face`, `font_config`, `audio`, `scene` and `shader`
(plus `prefab`, `model`, `animation`, `navigation`, `string_table`, `locale_config` and
`caption_track`; see their pages).

- IDs are stable authored identifiers (non-empty, at most 256 bytes, no control
  characters). Moving or renaming a file changes only `path`; scenes keep the ID.
- Duplicate IDs and paths that collide (compared case-insensitively) are errors.
- `script` assets must name a `.rs` source and are limited to 1 MiB.
- `texture` assets are PNG (up to 8192 per axis, 128 MiB encoded) or ASCII P3 PPM
  (maximum value 255); pixels load as top-left RGBA8 with straight alpha. `mesh`
  assets are the text format `CGMESH 1` followed by `vertex x y z u v` triples
  (a non-empty triangle list of at most 1,000,000 vertices). `font` assets are
  bitmap-atlas JSON referencing a separately declared texture asset; `audio`
  assets are WAV; `scene` assets are scene JSON for runtime room changes; `shader`
  assets are WGSL. `font_face` assets are TrueType/OpenType files (`.ttf`, `.otf`, `.ttc`,
  `.otc`, up to 32 MiB) and `font_config` assets configure their families and fallback
  chains (see [Text engine](text.md#font-assets)). `string_table` assets are Project Fluent
  `.ftl` files (at most 1 MiB) and the single `locale_config` asset lists which tables make up
  which locale (see [Localization](localization.md#project-integration)).
- All referenced files must exist inside the project root. Absolute paths, `..`
  traversal and symlink or junction targets outside the root are rejected.
  `Project::load` loads and validates every declared mesh, texture, font and scene.

## Scenes

A scene has `schema` (`1`), a persistent `scene_id` and entities with unique,
persistent string IDs, names and a `components` object.

```json
{
  "schema": 1,
  "scene_id": "scene:first-project",
  "entities": [{
    "id": "entity:player",
    "name": "Player",
    "components": {
      "Transform": { "position": [0.0, 0.0, 0.0] },
      "Script": { "asset": "asset:player-script" },
      "MeshRenderer": { "mesh": "asset:player-mesh", "texture": "asset:player-texture" }
    }
  }]
}
```

Positions are finite single-precision world coordinates in metres in a
right-handed basis: +X right, +Y up, +Z toward the viewer. There is no rotation,
parent or local-transform field in version 1. A scene holds at most 65,536
entities. Every component accepts an optional `version` field that defaults to `1`;
saving writes it explicitly. Unknown component names and unsupported versions are
errors. Component references to assets must match the declared kind.

| Component | Fields |
| --- | --- |
| `Transform` | `position` `[x,y,z]` |
| `Script` | `asset` (a `script` asset ID) |
| `MeshRenderer` | `mesh` (mesh asset), optional `texture` (texture asset) |
| `SpriteRenderer` | `texture` (required); optional `size` `[1,1]`, `pivot` `[0.5,0.5]`, `tint` RGBA in `[0,1]` `[1,1,1,1]`, `source` and `frames` atlas rectangles `[x,y,w,h]`, `framesPerSecond` (0), `loop` (true), `layer` (0), `visible` (true) |
| `Camera2D` | optional `logicalSize` `[320,180]`, `pixelsPerUnit` (16), `center` `[0,0]`, `follow` (entity ID), `pixelSnap` (true) |
| `Camera3D` | optional `active` (true), `projection` tagged by `kind`: perspective uses `fov_y_radians`, `near`, `far`; orthographic uses `height`, `near`, `far`. Requires `Transform`. |
| `MeshMaterial` | optional shader/texture asset IDs, RGBA `tint`, `unlit` and `transparent`; applies to mesh and sprite rendering. See [scene rendering](rendering3d.md). |
| `Collider2D` | `size` (positive); optional `offset` `[0,0]`, `trigger` (false), `layer` (1), `mask` (4294967295) |

Rules enforced by validation:

- `SpriteRenderer` and `Collider2D` require a `Transform` on the same entity.
- Sprite atlas rectangles (`source` and each of at most 4096 `frames`) are measured
  in texture pixels from the top-left, have positive size and stay within 8192;
  `framesPerSecond` is in `[0, 1000]`.
- A scene permits at most one `Camera2D`. `follow` must name an entity in the same
  scene that has a `Transform`. `logicalSize` entries are 1-8192.
- `Collider2D` `layer` and `mask` are nonzero bitmasks; two bodies interact only
  when each body's layer intersects the other's mask.

Native game code can attach arbitrary additional components at runtime; those are
Rust types and are never serialized into the authored scene.

## Revisions and diagnostics

`Scene::revision()` is a BLAKE3 hash of the scene's serialized form. `SceneDocument`
requires the expected revision on every edit, undo, redo and save, so stale edits are
rejected, and saving replaces the file atomically. Failures are `Error` values with
`Diagnostic` entries holding a stable `code`, `severity`, `file`, `path` and
`message`; codes and paths are the machine contract and messages may change.
Schema version 1 is the baseline and has no migration; unsupported versions are
errors, and future format changes must define an explicit, tested migration.

## Extensible authored data

Scenes additionally support local quaternion/scale transforms, entity parents,
`EventBindings` and namespaced custom components. Project manifests may reference
a declarative `componentSchemas` file and `prefab` scene-fragment assets. Defaults
preserve existing schema-1 scenes. See [components and scene commands](meta-programming.md)
for formats, validation, cloning and runtime integration.
