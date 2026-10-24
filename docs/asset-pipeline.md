# Asset import and runtime ownership

`engine-assets` imports glTF 2.0 JSON and GLB into `ImportedScene`, without a GPU,
ECS world, or dependency on authoring tooling. Existing project manifests remain
the authority for stable asset IDs. The crate does not add a second registry or
write another manifest. Project integration maps those stable IDs to process-local
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
