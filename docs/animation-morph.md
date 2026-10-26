# Morph targets and weight playback

The glTF/GLB importer retains POSITION, NORMAL and TANGENT target deltas in the model's
`scene.morphs` sidecar. Shared base vertices remain immutable. Each mesh primitive
has the same ordered target count; defaults come from the node's `weights`, then
the mesh's weights, then zero. Negative and extrapolated weights are preserved.
Importer fingerprints use `engine-gltf/3` to invalidate earlier derived caches.

Imported prefabs attach `engine.MorphWeights` to each morph primitive, alongside
`engine.SkinBinding` for skinned meshes. The binding identifies `model`,
`mesh_node`, and `primitive`; its `weights` array selects explicit weights, or
uses imported defaults when empty. Prefab entity renaming requires no remapping
of this model-index binding. Skin bindings retain their existing entity remapping.

`engine.MorphPlayer` on the same primitive selects a retained model `clip` index.
It samples that clip's weight channel for the bound mesh node. Its fields are
`time`, `speed`, `looping`, and `playing`; defaults are 0, 1, true, true. Native
code can seek by changing time, pause with playing=false, or remove the player to
restore explicit/default weights. Playback overrides explicit weights while the
player exists. STEP, LINEAR and CUBICSPLINE use the original key data; cubic
Hermite tangents are scaled by the key interval. Nonlooping sampling clamps to
clip endpoints; looping sampling wraps (including negative time). The native
clock advances by simulation dt multiplied by speed, bounded to +/-100.

Before render extraction, the deformation consumer evaluates weights into the
transient `EvaluatedMorphWeights` component, adds weighted deltas to source
positions and normals, normalizes normals, and then applies skin matrices when
present. Unskinned morphs use the same consumer without the skin stage. Every
frame starts from immutable source geometry, so deformation never accumulates.
UVs, colors and triangle indices survive. TRS clips and animation graphs remain
separate and can drive the skin hierarchy concurrently.

Simulation must be enabled and primitive entities require Transform. A player
requires a MorphWeights binding. Skin and morph bindings on one entity must
identify the same model/node/primitive. Cloth and soft-body components cannot
share this geometry authority. The consumer refuses an existing GeometryOverride
from another writer; on removal or scene reset it removes only the exact mesh
instance it last wrote, preserving another writer's replacement. Removing a
morph binding while retaining a skin returns to model defaults. Removing both
removes the deformation consumer and its transient evaluated weights.

Limits: at most 256 targets per primitive, one million aggregate imported delta
vectors, one million aggregate runtime delta vectors, one million runtime deformed vertices, 64 cached runtime models and
256 MiB cached source JSON (64 MiB per model). Animation output scalar counts
also consume the one-million-key import/model budget. Model JSON is revalidated
for finite data, indices, target/default lengths, duplicate bindings/channels and
budgets. Invalid playback, missing clips or missing weight channels fail clearly.
Sparse accessors remain unsupported by the existing importer and are rejected.
Tangent deltas are preserved and validated in metadata; the current mesh renderer
reconstructs tangent frames from UV derivatives and does not consume stored base
or morph tangents. No renderer vertex layout is changed.
Cubic TRS animation retains its previous metadata-only limitation; cubic weight
animation is evaluated. The default generated player selects the first clip with
a matching node weight channel; native code can select another retained clip.
