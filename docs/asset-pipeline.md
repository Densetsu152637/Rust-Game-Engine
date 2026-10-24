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
original scene selections. Rendering currently uses base color and geometric
lighting; it does not claim full glTF PBR or automatic skin deformation.

Step/linear TRS clips become `animation` assets with per-channel key times. The
first supported clip is attached to the prefab root as `engine.AnimationPlayer`,
with bindings ordered by source node index. Prefab instantiation remaps those
bindings. Cubic clips, clips with over 4096 nodes, or binding components too large
for the 64 KiB authoring limit remain in model metadata and produce a warning.
Generated meshes stay static; explicit skinning adapters can use the retained
joint palette, inverse binds and original vertex weights.

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
