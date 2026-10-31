# Native Hi-Z depth producer

`engine-render-hiz` is an independent native wgpu compute building block. It
builds a per-view `R32Float` maximum-depth pyramid from an explicit,
single-sample `Depth32Float` texture and viewport. The engine's scene depth
uses forward Z: 0 is near and 1 is far. The caller records it after all depth
writers finish. It owns no render stage, camera projection, mesh selection,
LOD policy, or draw submission.

A `HiZKernel` holds device compute pipelines. Create one `HiZView` per
independent view using `create_view(gpu, size, query_capacity)`; call
`resize` when its viewport size changes. `encode` checks the source texture
format, usage, sample count, layer count and viewport bounds, then rewrites
every mip and returns a new `VisibilityOutput` GPU buffer. The viewport is
`[x, y, width, height]` in the source texture; `size` is `[width, height]`
for the pyramid. The texture must already be written in the same queue order.
The kernel does not read the preceding frame. Query input and output buffers
are created per encode, so queued frames do not overwrite each other's query
payloads or masks. A view's pyramid texture must not be shared by overlapping
GPU encodes that require different depth contents without ordered submission.

## Conservative reduction

Mip dimensions follow wgpu's floor-halving rule, clamped to 1. For a source
axis of length `S` and destination axis of length `D`, destination coordinate
`i` covers the half-open source interval
`[floor(i*S/D), floor((i+1)*S/D))`. Those intervals partition every source
texel, including the last pixel on odd edges. Each destination stores the
maximum over its full 2D footprint. Base depth samples outside `[0, 1]` and
NaNs become 1; a cleared source depth of 1 therefore cannot cause a false
occlusion. Every reduction reads the previous mip through a sampled
single-mip view and writes a distinct storage single-mip view.

The pyramid is capped at 4096 pixels per edge and 96 MiB total. Query capacity
is capped at 65,536; its input/output buffer sizes and dispatch dimensions
are checked against the device limits. Kernel creation also checks the 8×8
reduction and 64×1 query workgroup limits before creating pipelines.
`R32Float` storage, sampled, and copy usage are checked against adapter
format features before the pipelines are created.

## Visibility queries

`VisibilityQuery` is a preprojected, conservative rectangle with a nearest
possible object depth. `rect` is signed viewport-local half-open
`[x0, y0, x1, y1]`; the producer must include every touched pixel and mark
`uncertain` for near-plane clipping or any projection uncertainty. Empty,
offscreen, invalid-depth, or uncertain queries return visible. The shader
chooses a mip whose rectangle covers at most 4 by 4 texels, maps each bound
through the exact mip partition, and reads all covered texels. It reports
occluded only if `nearest_depth` is strictly greater than their maximum.
The result buffer holds one `u32` per query: 1 visible, 0 occluded.

This is a conservative GPU visibility result, not an active culling system.
A pipeline adapter must create sound projected bounds, coordinate the
depth-producing pass, and consume the GPU buffer through indirect draws or a
later GPU stage. CPU readback for immediate draw decisions would add a stall.
The adapter also owns camera history and LOD remapping. No performance claim
is made until that integrated path is measured.

## Verification

CPU references and unit tests cover odd footprints, invalid depth and
query conservatism. Ignored `gpu_hiz` tests request a real adapter and compare
every mip and query result against the CPU reference for 3×5, 9×7, 1×17 and
17×1 views, plus resize, replacement on later frames, independent views and
input errors and a restricted compute-limit device. Tests upload `R32Float`
samples, then write `Depth32Float` through a depth-only pass.

## Opt-in indirect draw adapter

`engine-render-pipeline::visibility::HiZIndirect` consumes the real GPU
`VisibilityOutput` from `HiZKernel::encode`. The caller provides one
`GpuInstance` per mask entry and ordered, contiguous
`VisibilityBatch` ranges with `GpuMesh` handles that cover every candidate
exactly once. An instance and its mask always share the same index.
The adapter compacts survivors in each range in source order and emits both
native draw and indexed-draw argument buffers. `IndirectOutput::draw_batch`
binds that batch's compacted group-3 range, sets its mesh buffers, and calls
the corresponding indirect draw. The caller must bind a compatible render
pipeline, view and material groups first. Its render pass may have depth
testing disabled; the generated `instance_count` still omits masked draws.

The output `GpuInstance` ABI is the existing 208-byte `engine::mesh`
group-3 record. Each batch starts at a record offset that is a common multiple
of 208 bytes and the device's minimum storage-buffer offset alignment. A
zero-candidate batch at the current cursor receives a legal nonzero
binding and a zero-instance command. Every indirect command uses
`first_instance = 0`, so the adapter does not require
`INDIRECT_FIRST_INSTANCE`. The output buffers retain `COPY_SRC` for readback and diagnostic consumers; argument buffers have
`STORAGE | INDIRECT | COPY_SRC`.

This prototype accepts at most 65,536 instances, 256 batches and 1,024
candidates per batch. The compute shader runs one 64-lane workgroup per
batch, scans a fixed 16 blocks and preserves stable source order. Kernel
creation checks indirect-execution support, six compute storage bindings,
and four bind groups required for group 3.
Encoding rejects overlapping, gapped, reordered and incomplete batch ranges.
It also checks mask count, buffer usage, byte arithmetic, alignment,
binding sizes, dispatch count and device limits before encoding
work. Errors are explicit; the caller owns direct-draw fallback for the
whole affected frame. The scene pipeline's opt-in use of this adapter is
described under [Camera visibility consumer](#camera-visibility-consumer).

Ignored real-GPU tests build masks through `HiZKernel::encode`, compare
compacted IDs and both argument formats with a CPU reference, and render
indexed and nonindexed diagnostic tiles with depth disabled. They cover
mixed, all-visible, zero-visible and empty batches, aligned multibatch
bindings, and restricted storage-binding and bind-group devices.

## Camera visibility consumer

The scene pipeline consumes the adapter for opaque camera draws. It is opt-in
per camera: enable `engine.hiz.visibility` in `Camera3D.render_features` (the
feature is disabled by default) and the host must supply per-draw visibility
metadata (`ScenePipeline::render_view_with_visibility`, `MeshVisibility`). The
desktop host does so whenever a draw authors `MeshLods` or `HiZOccluder`, or the
camera requests the feature. Without the feature the opaque camera draws are the
unchanged direct path; shadow draws differ only for instances whose selected LOD
is coarser than level zero, which cast from level-zero geometry.

Frame order for an enabled view:

1. The host selects each draw's CPU LOD for this view, before any camera
   prepass (`MeshLods`, see [rendering-lod.md](rendering-lod.md#authored-meshlods)).
   The selected mesh is the draw's geometry for every camera stage.
2. `OccluderDepth` (`engine.hiz.depth`) draws only certified occluders with the
   selected geometry into a separate `Depth32Float` texture cleared to 1. The
   normal depth, the normals/motion prepasses and custom-shader prepass depth
   are never used: they are not trusted and not read.
3. `Visibility` (`engine.hiz.visibility`) builds the forward-Z MAX pyramid of
   that texture, runs one query per opaque instance and compacts the survivors
   with `HiZIndirect` into indirect draws.
4. `Opaque` issues the generated indexed/nonindexed indirect commands instead of
   direct draws. Transparent draws, the normals and motion prepasses and every
   shadow pass keep direct draws.

Certified occluders: the draw carries `HiZOccluder` (enabled), its material is the
built-in opaque shader (no custom material shader, not `transparent`), it is not a
runtime geometry override (skinning, World-generated meshes), it has finite bounds,
and the per-view budget holds (64 occluders, 200,000 triangles; excess markers stay
ordinary candidates). Because the depth pass rasterises exactly the vertex
transform of the colour pass, the certified depth is the depth the colour pass
writes for those draws; it cannot over-occlude.

Queries: the host supplies one local AABB per draw that encloses every level of
its LOD chain. Each query projects all eight corners with the same jittered
view-projection used for rasterisation, pads the rectangle by 2 pixels and
lowers the nearest depth by 1e-4. Anything uncertain stays visible: a corner
behind the near plane or outside clip space, a rectangle leaving the view, an
invalid or missing bound, a nonfinite matrix, an occluder itself, and any
instance with a custom material shader (displacement or alpha discard can move
or remove its pixels, so its bound is not trusted).

Shadows use their own caster plan: they draw every shadow-casting opaque
instance directly, with level-zero geometry (`MeshVisibility::shadow_mesh`),
whatever the camera culled. An occluded or off-screen caster still shadows the
view.

Explicit direct fallback. A view that requested the feature but cannot use it
renders exactly as if it had not, and `ViewReport::visibility` says why
(`requested`, `active`, `occluders`, `candidates`, `indirect_batches`,
`fallback`): no metadata, no certified occluder, missing indirect or compute
capabilities, more than 65,536 opaque instances or 256 indirect batches, a Hi-Z
view the device cannot create (edge above 4,096 pixels), `engine.hiz.depth`
disabled, or an encoding error. The desktop `RenderStats` expose
`hiz_occluders`, `hiz_candidates` and `hiz_indirect`.

Limits and honesty. The camera still pays the normals/motion prepass for every
candidate and the occluder depth pass; the saving is the opaque colour draw of
skipped instances. No performance claim is made: culling was verified for
correctness, not speed. The pyramid is built from the current frame only (no
history), so a very large occluder set or fine geometry costs a second raster
pass. The survivor count lives only on the GPU; `ViewReport.instances` counts
candidates. Scenes whose opaque geometry is mostly custom-material, mostly
generated by the World, or has no `HiZOccluder` get no culling. Group-3 instance
records are still the 208-byte layout; a later lightmap instance-layout
migration must update the compaction WGSL in step.

### Verification

`ScenePipeline::visibility_diagnostic` (tests and tooling) blocks on the GPU and
returns the `instance_count` each indirect command holds plus a depth-disabled
replay of those exact commands: pixel alpha counts how many surviving instances
cover it, so an occluded instance is absent whatever the draw order. Ignored
real-adapter tests (`gpu_hiz_consumer`, `gpu_hiz_lod`; RTX 5080/Vulkan) cover
the occluded instance missing from commands and replay with a pixel-identical
frame, occluder movement (hidden, reappears, hides again), resize, jitter, a
second independent view, every fallback above, an occluded and an off-camera
caster that still shadow the floor, the selected LOD geometry (not its bounds)
deciding occlusion, hysteresis in the rendered image, and the authored path
from project JSON.
