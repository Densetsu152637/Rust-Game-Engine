# Scene rendering

`Runtime::extract_scene_frame` produces immutable `SceneRenderFrame` snapshots.
The desktop path renders a directional shadow map, depth-tested 3D meshes, then
2D sprites/UI. `RenderFrame`, `Draw`, `SpriteRenderer::render`, offscreen sprite
rendering, custom WGSL and native render callbacks remain available.

## Camera and lighting

`Camera3D` is an authored/native component. Projection is tagged `perspective`
(`fov_y_radians`, `near`, `far`) or `orthographic` (`height`, `near`, `far`).
Coordinates are right-handed, Y up, camera forward negative Z, GPU depth 0 to 1.
The camera transform must be invertible. Stable authored ID order selects the
first active camera. Without one, extraction retains the existing 2D behavior.

The camera's `lighting` object supplies one directional light: `direction` points
from the surface toward the light, `color` is linear RGB in 0..1, `intensity` is
0..100, and `ambient` is 0..1. Direction must be finite and nonzero. Default light
is `[0.4,0.7,1]`, white, intensity 3 and ambient 0.08. Native users edit the same
`Camera3D` component; MCP component inspection and edits expose the same schema.

```json
{"Camera3D":{"lighting":{"direction":[0.6,1,0.8],"intensity":3,
  "shadows":{"enabled":true,"resolution":1024,"center":[0,0,0],
             "extent":20,"depth":100,"bias":0.001}}}}
```

## Physically based materials

`MeshMaterial` supports linear RGBA `tint`, optional base-color `texture`,
`metallic` (default 0), `roughness` (default 0.5), `normal_texture`,
`normal_scale` (default 1, range 0..10), and `metallic_roughness_texture`.
Metallic/roughness factors must be finite and in 0..1. Texture references are
catalog IDs validated as texture assets. `shader` selects custom WGSL; omitting
it selects the built-in metallic-roughness shader. `unlit` bypasses lighting.
`transparent` enables straight-alpha blending, disables depth writes, and sorts
back-to-front by object origin, with stable IDs breaking ties. Opaque meshes
write depth. The existing 2D extraction forces authored materials to unlit.

The BRDF combines GGX/Trowbridge-Reitz normal distribution, separable Smith
masking-shadowing, Schlick Fresnel, and energy-weighted Lambert diffuse. The
nonmetal reflectance at normal incidence is 0.04; metal reflectance uses base
color. Perceptual roughness is squared for the GGX alpha parameter, with a 0.045
roughness floor to bound the specular singularity. Direct lighting is linear;
RGBA8 sRGB targets encode output. Values above the target range clip. Ambient is
a simple diffuse term, not environment-map lighting or global illumination.
There is no HDR tone mapper, IBL, clear coat, transmission or multi-scattering
compensation in this shader.

Base-color textures decode as sRGB, multiplied by linear vertex color and tint.
Metallic-roughness data uses **B for metalness and G for roughness**, multiplied
by the scalar factors. Normal maps use linear RGB mapped from 0..1 to -1..1,
positive Y convention, with `normal_scale` applied to XY. Data textures use a
linear GPU view of their original bytes, even when the same image is also used
as an sRGB base color. Sampling currently remains nearest/clamp without mips.

JSON meshes may provide `normals`, one finite vector per vertex. An empty array
uses flat geometric normals derived from transformed triangle positions. Source
normals use the inverse-transpose model transform, including nonuniform scale;
CPU skinning preserves deformed normals. Surfaces are two-sided. Tangent frames
for normal maps are reconstructed from world-position/UV derivatives; degenerate
UV triangles retain their geometric/smooth normal. Authored tangent vectors and
MikkTSpace-specific tangent seams are not consumed.

The model importer maps glTF base-color, metallic/roughness factors and textures,
normal textures/scale and source normals into renderable assets. These textures
must share UV set 0 or 1; conflicting sets are rejected rather than remapped
silently. Emissive, occlusion, alpha-mask/cutoff, sampler modes and material
extensions remain preserved in the model asset but are not implemented by this
material shader. The import warning reports these limits.

The model and texture-channel conventions follow the
[Khronos glTF 2.0 specification, materials and Appendix B](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html#materials)
(accessed 2026-10-24). This renderer implements the specified base BRDF components;
it does not claim complete glTF rendering conformance.

## Directional shadow mapping

`lighting.shadows.enabled` defaults to false. When enabled, the wgpu backend
renders opaque caster triangle geometry into a `Depth32Float` texture before
the color pass. The built-in shader projects world positions into this map and
performs a 3x3 percentage-closer filter using explicit depth comparisons and
receiver-plane depth correction for sloped surfaces. This
is an actual geometry depth pass, so moving occluders changes receiver pixels.
Shadowing affects direct lighting; the ambient term remains visible.

`resolution` must be a power of two in 64..4096 and fit the GPU limit. `center`
is the world-space shadow volume center (each coordinate within +/-1000000), `extent` its square width/height
(0.01..100000), and `depth` its length along the light direction (0.1..100000).
The volume is fixed in world space; positions outside it are lit. `bias` is a
normalized depth comparison offset in 0..0.1 (default 0.001). Adjust it with
volume depth/resolution to balance acne and detached shadows. There are no
cascades, automatic camera fitting, point/spot shadows, alpha-tested casters or
translucent shadow transmission. Transparent draws do not cast shadows.

`MeshMaterial.cast_shadows` and `receive_shadows` default to true. Custom vertex
shader displacement is not replayed in the shadow pass; such materials should
opt out of casting or provide a native implementation. Native render passes
still receive the shared device/queue and run after engine draws; the renderer
does not replace or claim DLSS/temporal reconstruction support.

## Shader and native contracts

Legacy vertex locations remain 0 position (`vec3<f32>`), 1 UV (`vec2<f32>`), and
2 color (`vec4<f32>`), at byte offsets 0, 12 and 20. Location 3 adds an optional
normal (`vec3<f32>`) at byte 36; the desktop upload stride is now 48 bytes.
Entry points remain `vs_main` and `fs_main`. Uniforms are supplied only when
reflected as used by the selected shader:

| Group/binding | Value | Bytes |
| --- | --- | --- |
| 0/0 | projection x view x model, column-major | 64 |
| 0/1 | model-to-world matrix | 64 |
| 0/2 | inverse-transpose model matrix | 64 |
| 2/0 | tint vec4; options vec4: unlit, metallic, roughness, receive-shadow | 32 |
| 2/1 | light VP matrix; eye, direction, radiance/ambient, shadow enable/bias vec4s | 128 |
| 2/2 | normal-map enable/scale, metallic-roughness-map enable, reserved | 16 |

Texture 1/0 and nearest clamp sampler 1/1 retain the existing contract. Linear
data textures use 1/2 and 1/3; the generated `texture_depth_2d` shadow map uses
3/0. Other resource types/uniform slots, wrong sizes, missing vertex inputs and
invalid WGSL fail before submission. `DrawCommand.shadow` exposes the neutral
caster matrix/resolution to native producers; all casters in a frame use one
resolution. The recording backend records commands but does not generate pixels.

## Verification and example

Run `cargo run -p engine-desktop --example pbr_shadows -- output.png`, or use
`--window`, for the authored scene in `examples/pbr-shadows`.

`cargo test -p engine-desktop --test scene3d -- --include-ignored --nocapture`
runs real GPU readback assertions for depth, custom WGSL/2D, material factors,
normal/MR maps and linear channel handling, shadow occlusion, caster/receiver
switches, and shadow-map resizing. GPU tests fail if an adapter is unavailable.
Set `ENGINE_PBR_CAPTURE` to an absolute PNG path to retain the shadow test image.
Project rendering/import tests cover authored round trips, invalid settings and
asset kinds, texture mappings, and normal-array boundary failures.
