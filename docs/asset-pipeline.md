# Asset import and runtime ownership

`engine-assets` imports glTF 2.0 JSON and GLB into `ImportedScene`, without a GPU,
ECS world, or dependency on authoring tooling. Existing project manifests remain
the authority for stable asset IDs. The crate does not add a second registry or
write another manifest. Runtime consumers may map those stable IDs to process-local
`AssetHandle<ImportedScene>` values. Existing CGMESH and PNG/PPM loaders remain in
`engine-project`; this crate is an additional import path rather than a replacement
for those formats.

## Import contract

Call `import_gltf(project_root, source, &ImportLimits)`. Relative source paths are
relative to the project root; references inside a glTF are relative to its source
directory. The importer uses gltf 1.4.1's validated parser and typed utility readers,
then checks buffer/accessor ranges, formats, hierarchy, numeric data and budgets
before constructing output. It never asks gltf's convenience importer to resolve
an external reference against the process working directory.

The output retains original glTF indices across nodes, meshes, materials, textures,
images and skins. Nodes include parents, children, local TRS, the original local
matrix, mesh/skin association and names. Scene root lists and the default scene
index remain distinct; importing a file does not select or instantiate a scene.
Transforms keep glTF's coordinate convention and quaternion x/y/z/w ordering.
Matrices use column-major arrays. No axis or unit conversion is performed.

Triangle-list primitives retain material association and indexed geometry;
nonindexed geometry receives sequential indices. Positions are required. Normals,
tangents, UV sets 0/1, color set 0, joint set 0 and weight set 0 are optional and
stay optional. Integer normalized attributes are converted through gltf's readers.
Missing normals/tangents are not generated. Joint weights are retained rather
than renormalized. A skinned node requires paired joints/weights and valid joint
palette indices. Meshes can be shared by multiple nodes.

PBR materials preserve base color, metallic/roughness, normal, occlusion and
emissive properties and texture references, UV set numbers, alpha mode/cutoff and
double-sided state. Texture samplers retain glTF/OpenGL filter/wrap constants.
Images decode to source-space RGBA8; the GPU adapter selects sRGB interpretation
for each material usage. PNG is the enabled image codec, consistent with the
existing engine's image dependency. Image URIs and embedded image buffer views
are supported.

Skins retain joint node indices in palette order, the skeleton root and inverse
bind matrices; absent inverse binds become identity matrices. Animation clips
retain target node indices, strictly increasing nonnegative key times, TRS outputs
and step/linear/cubic-spline interpolation. Cubic output retains incoming tangent,
value, outgoing tangent triples per key. The importer does not sample animations.
A runtime that supports only step and linear interpolation must reject cubic
channels explicitly. Skin adapters must preserve transforms of non-joint ancestor
nodes when computing global joint matrices.

Explicitly unsupported inputs return errors: required glTF extensions, sparse
accessors, non-triangle topology, morph targets/weight animation, extra vertex
attribute sets beyond those listed above, non-base64 data URIs, remote URIs and
image codecs other than PNG. Optional extensions are ignored in favor of the base
glTF representation. Cameras, lights and application-specific extras are not
imported. This is a bounded subset importer, not a promise of all glTF features.

## Filesystem and resource boundary

The source and every external buffer/image path must canonicalize inside the
canonical project root. Percent escapes are decoded before checking a URI path;
absolute external paths, URI schemes, backslashes and ambiguous query/fragment
syntax are rejected. Canonicalization also rejects symlink/junction targets
outside the root. Paths may use `..` when they still resolve within the project.
The caller must prevent concurrent hostile filesystem replacement during import;
canonical path checks are not an OS sandbox or atomic directory capability.

Defaults are 64 MiB source bytes, 256 MiB total input bytes, 100,000 aggregate
objects/edges/attributes/channels, one million vertices, three million indices,
one million animation keys, 8192 pixels per image dimension and 256 MiB aggregate
decoded RGBA image bytes. `ImportLimits` exposes each limit. The total input count
includes source bytes and loaded buffer data, so embedded GLB/base64 content also
counts when materialized. Repeated external loads count toward the budget.
Decoded image output size is checked before pixel decoding and RGBA conversion;
the image decoder also receives allocation and dimension limits. Limits bound
logical input/output work, not exact process peak memory: parsers, temporary
attribute arrays and decoders use additional memory.

Buffers are checked before readers access them, including checked range
arithmetic, stride and accessor shape. Aggregate budgets cover duplicated
primitive/animation references. Cycles/multiple parents, invalid indices,
nonfinite vertex/transform/animation/material values, invalid skin references and
malformed key counts fail the import. Import errors do not publish partial output.

## Fingerprints and lifecycle

`ImportedScene.dependencies` contains canonical project-relative source and
external dependency paths, BLAKE3 content digests and byte lengths, sorted by path.
Embedded data is covered by the source digest. The scene fingerprint hashes
`IMPORTER_VERSION`, framed relative paths, byte lengths and those dependency
content digests. Thus source or dependency changes invalidate the cache without
mtime reliance; identical files at the same project-relative paths produce the
same fingerprint after moving the project to another checkout. Import semantics
changes require bumping `IMPORTER_VERSION`. There is no persistent cache writer in
this crate.

`AssetStore<T>::load` owns a value and fingerprint and issues a typed generational
handle. Handles are scoped to the issuing store and are not stable manifest IDs.
`get` and `fingerprint` reject stale handles. `reload` first validates the handle,
then invokes a loader; only a successful replacement changes the stored asset.
A failed reload retains the previous value and fingerprint. Successful reload
keeps the handle valid. `unload` increments its slot generation, invalidating old
handles before slot reuse; generation overflow retires the slot.

## Verification

`cargo test -p engine-assets` constructs real triangle GLBs with vertex channels,
PBR material, hierarchy, skin and skeletal animation. Tests also cover PNG texture
association, base64 buffers, nonindexed triangles, cubic tangent triples,
source-relative external dependencies, checkout-stable fingerprints, dependency
changes, malformed accessors, invalid skin/index/key data, unsupported required
extensions, traversal/remote references, byte/entity/vertex/index/key/image limits,
and transactional reload/stale handles. No hardware or editor is required.

## Project, CLI and MCP import

The shared authoring entry point is
`engine_project::model_import::import_model(&project, source, prefix)`. Both the
CLI and MCP call this API:

```text
engine project import-model path/to/project.json source/model.glb --prefix ship
```

`source/model.glb` is relative to the directory containing `project.json`, even
when the CLI is run elsewhere. MCP exposes `asset_import_model` with `source`
and `prefix` arguments only when the server starts with `--allow-write`.
Neither path compiles or executes the project's Rust code. Source files and all
external references must be inside the project, outside `.git`, `.derived` and
`target`. Prefixes accept 1-64 ASCII letters, digits, hyphens and underscores.

The importer creates `assets/imported-ship/` and appends stable manifest IDs:
`ship:model`, `ship:prefab`, `ship:mesh:<mesh>:<primitive>`,
`ship:texture:<image>`, and supported `ship:animation:<clip>` entries. Existing
IDs and output directories are rejected; imports never overwrite generated files.
The result lists asset IDs/paths, the source fingerprint, new manifest revision,
and explicit conversion warnings. The startup scene is unchanged. Instantiate
the prefab through the normal `InstantiatePrefab` scene edit operation, using
an instance prefix.

Generated JSON meshes preserve positions, UVs, vertex colors and indices in the
existing runtime mesh contract. Both JSON meshes and legacy CGMESH files use
`content::load_mesh`, with bounds, finite-value and triangle-index validation.
PNG files retain decoded texture pixels. Prefabs retain local TRS and parent
relationships, with identity child entities for individual primitives and
`MeshMaterial` base-color texture/tint/transparency. Matrix-form transforms that
cannot be represented faithfully as authored TRS fail the import. Every source
node is included; multiple original scene selections remain in model metadata.

The versioned `model` asset is self-contained JSON:
`ModelDocument { version: 1, scene: ImportedScene, node_entities: Vec<String> }`.
`model_import::load_model` reads and validates bounded model data, including
references, numeric data, hierarchy and aggregate capacities. `node_entities[i]`
is the prefab entity corresponding to source node `i`, before the instance prefix.
This preserves richer vertex channels, full PBR properties, skin data, clips and
original scene selections. The renderer consumes base color, metallic/roughness,
normal maps and source normals; emissive, occlusion, alpha mask and sampler modes
are preserved as metadata ([3D rendering](rendering3d.md)).

Step/linear TRS clips become `animation` assets with per-channel key times. The
first supported clip is attached to the prefab root as `engine.AnimationPlayer`,
with bindings ordered by source node index. Prefab instantiation remaps those
bindings. Cubic clips, clips with over 4096 nodes, or binding components too large
for the 64 KiB authoring limit remain in model metadata and produce a warning.
Skinned primitive entities receive `engine.SkinBinding` with the model asset ID,
source mesh-node/primitive indices and the complete source-node binding table.
Prefab instantiation remaps these entity references. The runtime skin adapter uses
the retained joint palette, inverse binds and original vertex weights. Skinned
imports require at most 4096 source nodes, 60 KiB per binding table and 8 MiB
aggregate duplicated binding data; exceeding these limits fails the import.

All content is staged in a temporary directory inside the project. Importers
coordinate through `.model-import.lock`, and an optimistic byte comparison rejects
manifest changes during preparation. New output files are created exclusively,
then validated as a candidate project. Only then is the manifest atomically
replaced; failures remove the files created by that import and leave the manifest
unchanged. Cleanup failures are reported. These guards coordinate importer calls
and detect ordinary concurrent edits; they are not an atomic compare-and-swap
protocol for unrelated filesystem writers or hostile path replacement.
Generated limits add 4096 assets per import, 512 MiB total output, 256 MiB model
JSON, 64 MiB per mesh JSON and 16 MiB per prefab/animation JSON. JSON serialization
stops at its per-file limit. `.model-import.lock` is an empty coordination file,
not another asset catalog.

Project tests exercise import/reload, prefab instantiation, material/PNG/mesh
association, animation binding remapping, conflicts, traversal and rollback.
MCP tests exercise write gating and actual import; a CLI test launches the
`engine` executable, imports a real source fixture, and reloads generated assets.

## Additional read-only content roots

Status: implemented in `engine-project` (`content_roots`) and `engine-steam` (`WorkshopContent`). The decisions D1 to D5 at the end were accepted by root; the text below is the design as built.

A project can mount extra, read-only content roots next to its own root. The mechanism is
generic: it knows nothing about Steam. A mod source (Steam Workshop installs, a local mods
folder, a downloaded pack) registers a directory; the Workshop glue in `engine-steam`
registers each installed item folder.

### Today's behaviour (what must stay identical)

`Project` holds `root` and an immutable `assets` map built at load. Each asset path goes
through `confined_file(root, relative)`: relative, no `..`, canonicalized inside the
canonical root, an existing regular file. `Project::resolve_asset(id)` and
`services::resolve_asset(world, id)` are the lookup; 41 call sites use the former, a few
loaders call `confined_file(&project.root, &asset.path)` directly after `assets.get`.
`ProjectResource` holds an `Arc<Project>`. A project with no extra roots must resolve
byte-for-byte as before.

### Registration API (new module in `engine-project`)

```rust
pub struct ContentRoots { /* shared, interior-mutable, cloneable handle */ }
impl ContentRoots {
    pub fn register(&self, mount: &str, root: &Path, declared_parent: &Path) -> Result<RootRevision>;
    pub fn unregister(&self, mount: &str) -> bool;
    pub fn mounts(&self) -> Vec<MountInfo>;          // mount, root, revision, asset count
    pub fn drain_events(&self) -> Vec<ContentRootEvent>; // Registered | Replaced | Unregistered
}
```

`Project` gains one field, an `Arc`-shared `ContentRoots` (empty by default, created by
`Project::load`). `Project` clones, and therefore `ProjectResource`, share the same handle, so
a registration is seen by every holder and every existing `project.resolve_asset(..)` call site
works unchanged. `Project::resolve_asset` and `services::resolve_asset` consult the project
map first and only then, for ids carrying a mount prefix, the mounted roots. Registration
happens on the tick thread between ticks (the Workshop glue does it while handling service
events); resolution takes a read lock for the lookup only and returns an owned `PathBuf`.

### Root manifest

A mounted directory contains `content.json`: `{ "schema": 1, "assets": [ {id, path, kind} ] }`,
the same `Asset` records and per-record validation as `project.json` (duplicate ids, duplicate
paths compared case-insensitively, `identifier()` rules), capped at 1 MiB and 4096 assets.
Mod authors write it next to their files before uploading. A directory without a valid manifest
is rejected as a whole; nothing is half-registered.

### Asset ids and collision policy

- A mod asset's full id is `@<mount>/<id>`, for example `@workshop-3354501696/tex:rock`. The
  mount name is 1 to 64 characters of `[A-Za-z0-9._-]` and unique among mounts.
- `@` becomes a reserved first character of project asset ids: `Project::load` rejects a
  project id starting with `@` (`project.asset.id.reserved`). Today's ids use no such prefix
  (decision D5 below).
- Therefore ids never collide across roots and a mod can never shadow or replace a project
  asset: **the project root always wins** and mods cannot override it in v1. Ids inside one
  root must be unique. Re-registering a mount replaces that mount atomically (below).
- Mod files are leaf data and name no other assets (see kinds), so ids inside a mod's own
  files never need cross-root resolution.

### Kinds a mod root may provide

Allowed in v1, all passive data parsed by existing bounded loaders and referencing no other
asset: `mesh` (CGMESH), `texture` (PNG/PPM), `audio` (WAV), `lut`, `string_table` (Fluent)
and `font` (bitmap-atlas JSON; it names a texture asset, so a mod font may only name a
texture of the same root, checked at registration).

Refused: `script` and `text_script` (they run code with engine capabilities; a hostile
Workshop item must never execute), `graph`, `shader` and `pcg` (user-authored executable or
GPU-compiled programs), `gameplay`, `font_face`/`font_config`/`locale_config` and `video`
(large native parsers over hostile bytes or global configuration), and the composite kinds
`scene`, `prefab`, `model`, `animation`, `tilemap`, `tileset`, `terrain`, `foliage`,
`navigation`, `acoustics`, `partition`, `indirect`, `caption_track` (they reference other
assets by id and need per-root id resolution). Composite kinds can be admitted in a later
stage by resolving local ids against the owning root. An unknown or refused kind fails the
whole registration with `content_root.kind`.

### Checking the kind before loading

The kind of a mounted asset is whatever its `content.json` declares, and a mod author controls
it. `Project::resolve_asset`, `services::resolve_asset` and `ml::load_asset` return or load
the file without looking at the kind, so a mod id handed to them can name a texture where the
game expects a model, and the loader then parses that file as the wrong format (it fails or
reads unintended bytes; the loaders are bounded, so the cost is an error, not code
execution). Game code that accepts an asset id from data, a save file or a mod must check the
kind first:

```rust
// Project ids and mod ids alike; Err(content_root.kind | project.asset.kind) on a mismatch.
let path = project.resolve_asset_kind(id, &["mesh"])?;
// Mounted ids only, or to read the declared kind without touching the disk:
let path = project.content_roots.resolve_kind("@workshop-1/rock", &["mesh", "texture"])?;
let kind = project.content_roots.asset("@workshop-1/rock").map(|a| a.kind);
```

### Confinement and validation

1. `register(mount, root, declared_parent)`: both must be absolute. The root's path relative
   to `declared_parent` is walked component by component with `symlink_metadata`: no
   component may be a symlink, a Windows junction or any reparse point (attribute `0x400`,
   the check `persistence/atomic.rs` already uses), and the canonical root must equal the
   canonical parent joined with that relative path (so it cannot escape the declared parent).
   For Workshop installs the declared parent is the parent of the installed folder reported
   by Steam (`.../workshop/content/<app>`).
2. The root must be a directory. Its `content.json` and each asset file are opened through
   `confined_file(canonical_root, relative)`, so absolute paths, `..`, and canonical paths
   outside the root are rejected exactly as for the project root.
3. Stricter than the project root (decision D4): inside a mod root no component of an asset
   path may be a symlink or reparse point either, and a file that is one is rejected even if
   it resolves inside the root. Content from other players has no legitimate use for links.
4. Asset files are re-validated on every resolution (`confined_file` plus the link walk), so
   a file replaced by a link after registration fails with a path error. As elsewhere,
   this is not an OS sandbox and cannot stop hostile replacement racing a read.
5. Existing per-kind size and format limits apply at load time. Registration reads only the
   manifest, plus each `font` asset's JSON (at most 4 MiB) to check the texture it names.

### Limits

At most 64 mounted roots, 4096 assets per root, 16384 mounted assets in total, 1 MiB
manifest. Exceeding a limit fails the registration with `content_root.limit` and leaves the
mounts as they were.

### Reload, update and unregister

- `register` on an existing mount validates the new root completely, then swaps it in; on
  any failure the old registration stays. Each successful registration bumps the mount's
  `RootRevision` and queues `Replaced`/`Registered`; loaders that cache decoded data key it
  by `(full id, revision)` and drop entries when `drain_events` reports the mount changed.
- `unregister` removes the mount at once. Later resolutions return `project.asset.unknown`;
  data already decoded stays valid for its owner. Reads racing the removal or an on-disk
  update fail with the ordinary missing-file error, never undefined behaviour.
- Steam glue (`engine-steam`, with the S9 `UgcService` events): at startup it registers every
  subscribed item that has install info; on `Reply(Downloaded)` success it re-reads
  `installed(id)` and registers (or replaces) mount `workshop-<id>`; on a completed
  unsubscribe it unregisters; an item whose `needs_update` is true stays mounted at its old
  revision until the download completes. A folder that fails validation is skipped and
  reported as an event; it never stops the game.

### Test plan

In `engine-project` (fixtures built with `tempfile`): registration succeeds for a valid root
and resolves `@mount/id`; traversal (`..`, absolute) in a manifest path; a symlink and (on
Windows) a junction as the root itself, as a component between parent and root, and as an
asset file or directory inside the root (skipped where the OS cannot create them, as the
existing link tests do); a root outside its declared parent; every refused kind and one
allowed kind per loader; duplicate ids and paths; the `@` reservation; limits; a project id
always winning because mod ids are prefixed; replacement keeping the old root on failure;
unregister making ids unknown; and the existing project/asset tests unchanged (an empty
`ContentRoots` resolves identically). In `engine-steam`: a fake installed item folder
registers on download completion, its texture/audio/mesh loads through
`services::resolve_asset`, update replaces it, unsubscribe unregisters it, and an invalid
folder is skipped with an event. A manual live check on App 480 (subscribe, install, load an
asset) is recorded in the plan Outcome, or recorded as not possible with the reason.

### Fonts, residual limits and lifetime

- **Mounted fonts.** A font file names its texture by local id. Anything that resolves
  `font.texture` as-is would look it up in the project namespace and could draw a project
  texture of the same id. `ContentRoots::font_texture_id("@mount/font")` re-reads the font
  through the same checks, requires a texture asset of the same root and returns
  `@mount/<texture>`; use it wherever a mounted font's texture is resolved. The built-in
  consumers (runtime rendering, UI preview, MCP asset inspection) iterate `project.assets`
  only, so they never see a mounted font in v1 and needed no change. At most 16 fonts per
  root, because each one is parsed at registration.
- **Check-then-open race.** Resolution validates a path and the caller opens it afterwards;
  a replacement of the file between the two is not prevented (no OS sandbox or directory
  capability). This is accepted because mounted folders are written by the mod source
  (Steam), not by untrusted code running on the machine. Reads of a file that vanished fail
  with the ordinary missing-file error.
- **Trusted ancestors.** The link walk covers the path from `declared_parent` down to the
  root. The ancestors of `declared_parent` itself, and the parent, are trusted: choose the
  parent so that it is a directory the mod source controls.
- **Reloaded projects start empty.** `Project::load` creates an empty `ContentRoots`. After
  a project reload the host must mount the roots again (for Workshop items call
  `WorkshopContent::mount_subscribed` with the new project's `content_roots`).
- **Locking.** `resolve` copies what it needs under the read lock and validates the
  filesystem afterwards, so slow disk access never blocks a registration.

### Scope: v1 and the planned v2

v1 mounts leaf data only and resolves it by full id through `Project::resolve_asset` and
`services::resolve_asset`; the loaders that take an asset id (audio, texture, mesh, string
table) work with mounted ids because they resolve through the project. Scene component
fields cannot reference mod ids yet: scenes keep validating at load, before any root is
mounted, and `require_asset` returns a borrowed `&Asset`. Planned v2: composite kinds
(`scene`, `prefab`, `model`, `animation`, `tilemap` and so on) with ids inside a mod's files
resolved against the owning root, and mod ids accepted in scene component fields (an owned
or lazily checked asset lookup).

### Decisions (accepted by root)

- D1: the root manifest is `content.json` with the same asset records as `project.json`.
- D2: the v1 kind allow-list above (leaf data only; no scripts, graphs, shaders or composites).
- D3: mod ids resolve by full id now; scene component fields referencing mod ids are v2.
- D4: every link inside a mod root is rejected, re-checked on each resolution.
- D5: a leading `@` is reserved in project asset ids and rejected at load
  (`project.asset.id.reserved`); `docs/project-format.md` says so.
