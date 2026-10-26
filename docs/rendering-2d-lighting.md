# Native lit sprites and tile-shaped meshes

This source milestone adds an optional native `SpriteLit` material, validated
point/directional light snapshots and real-backend readback test sources. It does
not install Project components, desktop extraction/submission adapters or a new
World tilemap renderer. No Cargo tests, compilation or GPU validation have run for
this milestone. Authored Runtime World tilemap-to-desktop pixels remain an
acceptance requirement after the native gate.

## Native API and submission

The module is `engine_render_pipeline::sprite_lighting`:

```rust,ignore
let point = SpriteLight::new(
    "lamp".into(),
    Light::point(glam::Vec3::new(0.0, 0.0, 2.0), glam::Vec3::ONE, 1.0, 8.0),
)?;
let lighting = SpriteLighting::new(glam::Vec3::splat(0.1), vec![point])?;
// Call once for the WHOLE view, in its existing painter order.
let per_draw = lighting.prepare_draws(&gpu.device.limits(), &presenter_offsets)?;
let source = compose_material(SPRITE_LIT_WGSL, MaterialTarget::SpriteLit)?;
let surface = SpriteMaterialUniforms::new(material_params, SpriteTextureMaps {
    normal: normal_handle.is_some(),
    metallic_roughness: mr_handle.is_some(),
})?;
```

Create the shader through the existing `RenderDevice`/`ShaderResource` API and
submit existing `DrawCommand`s. There is no second renderer implementation.
Bind each draw as follows:

| Binding | Data |
| --- | --- |
| 0/0 | 64-byte **unshifted** clip-from-world matrix |
| 1/0, 1/1 | existing base texture and nearest/clamp sampler |
| 1/2, 1/3 | existing linear MR/normal data textures, supplied through `DrawCommand.data_textures` in that order |
| 2/0 | 32-byte `surface.material()` |
| 2/1 | 5168-byte `per_draw[i]` POD value |
| 2/2 | 16-byte `surface.maps()` |

The light uniform is ambient `vec4`, count `vec4<u32>`, presenter offset `vec4`,
then 64 existing 80-byte `GpuLight` records. `check_limits` checks this separate
three-group variant: at least a 5168-byte uniform binding, four dynamic uniform
bindings overall, three fragment-stage uniform bindings, three sampled textures
and one sampler. Rejection is explicit; a caller may retain its previous accepted
snapshot or deliberately select the legacy unlit sprite path. No optional device
feature, fifth group, native mesh instance change or shared material-layout
replacement is introduced. Actual custom-shader limits still pass through ordinary
shader reflection/device validation.

`SpriteMaterialUniforms` reuses `MaterialParams` validation and its tint, unlit,
metallic, roughness and normal-scale values. Shadow flags do not request any work;
the SpriteLit material has no shadow pass or shadow sampling. Ordinary sprite
straight-alpha blending, disabled depth test/write and authored scissor remain
caller-supplied draw state. Native preparation never reorders draws or changes
that state. Only light IDs are sorted, for deterministic accumulation.

## Coordinates, materials and units

Input vertices retain the existing 48-byte position/UV/colour/normal record. They
are already in world space. The reference vertex shader adds presenter offset
**once**, to both clip positioning and the world position used for lighting. Do
not additionally offset the camera matrix or the vertices. Normals supplied in
these baked meshes must also be world-space; an absent/zero normal uses the
view-facing geometric normal from derivatives. The reference view direction is
constant +Z for the orthographic 2D camera.

`SPRITE_LIT_WGSL` is the native reference shader. Raw custom SpriteLit materials
import `engine::material` and use `engine_sprite_view_direction()` for the
orthographic direction. The old `engine_camera_position()` helper remains for
source compatibility but a fixed position cannot represent that direction across
a wide sprite. Existing mesh `LIT_WGSL`, legacy `MaterialTarget::Sprite`, native
IBL materials and their shader/layout contracts are unchanged. The later authored
material-graph adapter must select/adapt the SpriteLit reference template; this
milestone does not silently rewrite the graph compiler or current desktop material
selection.

Base colour samples decode from sRGB through the existing backend. Normal and MR
textures use existing **linear** data views. MR green scales roughness and blue
scales metallic. Normal RGB decodes to `2*RGB-1`; +R follows increasing u, +G
increasing v and +B points out of the surface. On an unrotated y-up quad whose v
grows downward, +G therefore points down. Tangents are derived from the submitted
positions and UVs, so mirrored/transposed UVs orient normal detail with the art.
Keep base/normal/MR atlas dimensions and layout identical; no second UV set or new
atlas format is added. Missing map handles must have their corresponding flags
false; the backend's neutral texture is not an encoded normal map.

Lighting uses the existing regularized GGX/Smith/Schlick BRDF and range-windowed
inverse-square point attenuation. Inputs are the same exposed shading units as
`Light`: directional illuminance and point candela, multiplied by caller exposure
exactly once. `Light::directional` takes the direction rays travel; its existing
GPU conversion produces the direction toward the light. Point height is its
world-space z relative to the sprite plane; a point exactly on a flat plane has no
positive Lambert cosine. Ambient retains `albedo*(1-metallic)*ambient` weighting.
There is no screen GI, IBL, temporal history, shadow, scene HDR or post-processing
integration here. Existing sRGB target conversion/clamping applies; large finite
lighting values are not a claim of HDR display output.

## Boundaries and lifetime

- At most 64 lights, unique nonempty IDs of at most 64 bytes; point and directional
  shapes only. Shadow requests and unsupported shapes fail rather than disappear.
- Light colours are finite in 0..1; intensity and ambient are finite in
  0..1,000,000. Positions/offsets are finite within +/-1,000,000; local ranges
  retain the existing `(0,1,000,000]` limit. Directions are validated and normalized.
- `prepare_draws` accepts at most 4096 offsets for the whole view. It validates
  the complete set and its aligned lighting-buffer/u32 dynamic-offset budgets before allocating a
  result. Results stay in caller order. The maximum unpadded lighting payload is
  21,168,128 bytes; the existing backend also validates its complete frame arena.
- Snapshots own no GPU resources or temporal state. Replacing a snapshot or
  changing target size requires new draw uniforms, without retaining stale
  per-view lighting groups. Existing RenderDevice owns texture/mesh handles,
  deferred retirement, dynamic uniform arenas and bind-group cache identity.

## World and desktop follow-up

Reuse [World's committed chunk contract](world-tilemap.md#render-data-handed-to-the-sprite-path):
16x16 chunks, the existing `GeometryOverride(Arc<Mesh>)`, y-up positions, oriented
v-down atlas UVs, baked tint/layer opacity, stable IDs/revisions and existing
`Draw` state/order. Do not regenerate chunks in this native module. The GPU tests
use small quad/UV fixtures; they do not establish actual Runtime World integration.

Remaining adapter work includes authored `engine.Light2D`, `engine.SpriteLighting2D`
and `engine.Lighting2D`, texture resolution/snapshot population, owner material
inheritance through `TileChunkRender.owner`, sprite material/normal uploads and
keeping opted-in tile chunks on the 2D path when a Camera3D exists. That routing
must preserve `(space,layer,id)` interleaving and avoid duplicate 3D submission.
Lighting remains in the existing ordered sprite/late-presentation path. Feeding 2D
world layers into scene HDR, temporal AA, or frame generation requires a separate
shared-stage contract; no such stage is added here.

## Validation gates (unrun)

CPU test sources cover invalid shapes/values/IDs, light and draw budgets,
restricted uniform/dynamic limits, preserved draw offset order, material flags and
exact reflected binding size. Ignored GPU test sources submit real native
`DrawCommand`s through `RenderDevice<WgpuBackend>` and read pixels: independent
f64 BRDF point/directional/MR references; linear normal sampling; all eight oriented
UV fixtures; legacy/lit coexistence, serial straight-alpha order and clipping;
single presenter offset in geometry and lighting; distinct draw snapshots,
replacement/resize and all 64 admitted lights.

```text
cargo test -p engine-render-pipeline --test sprite_lighting
cargo test -p engine-render-pipeline --test gpu_sprite_lighting -- --ignored --test-threads=1 --nocapture
```

Before full feature acceptance, execute those gates plus existing material/IBL
interop checks, then render an authored World tilemap through Runtime extraction
and desktop submission. Read back atlas orientation, normal response, opacity and
sprite/UI interleaving, and prove unchanged-chunk upload reuse, edit/erase/recreate,
visibility and hybrid-camera routing. Native source tests are only the first slice.