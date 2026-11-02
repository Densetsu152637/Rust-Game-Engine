# Screen-space reflections

`engine.ssr` adds reflections of the opaque scene to the built-in lit
materials. It traces each reflective pixel's mirror direction through the
current frame's depth, with a bounded step count and thickness, and blends the
opaque colour found there over the image-based specular the opaque pass already
added. It is built into `engine-render-pipeline`, runs in the `OpaqueEffects`
stage (after `Opaque` and `Sky`, before `Transparent`), is disabled by default
and needs no history.

Everything below is screen space: only what the camera sees can be reflected,
and only the opaque scene before transparents.

## Pipeline

| Feature | Stage | Work |
| --- | --- | --- |
| `engine.surface_material` | `OpaqueEffects` (before `engine.ssr`) | draws the opaque built-in lit batches again into three render-size targets (below), depth-tested against the final scene depth |
| `engine.ssr` | `OpaqueEffects` | fullscreen trace and composite from the current scene colour into the distinct opaque-effect destination, which becomes `scene_color()` |

`engine.surface_material` is enabled by default but idle unless `engine.ssr`
runs in the view, like the normal prepass. A view that stops using SSR frees
its three targets.

### Surface material (the producer)

The opaque pass is unchanged (its instance layout, the custom-shader material
ABI and its single colour output stay as they were). A second pass over the same
batches uses a variant of the built-in lit shader that is derived from
`lit.wgsl` by replacing only its lighting tail, so the normal mapping and
material logic cannot drift from the shaded image. It writes three
`Rgba16Float` targets (`FrameTargets::surface_material`,
`ScenePipeline::read_surface_material`, `FeatureContext::surface_material`):

| Target | Contents |
| --- | --- |
| `normal_roughness` | xyz: world-space unit **shaded** normal (vertex normal, normal map, facing the viewer); w: perceptual roughness (`MeshMaterial.roughness` times the metallic-roughness map's green) |
| `f0_coverage` | rgb: specular F0 = `mix(0.04, albedo, metallic)`; a: 1 where a reflective built-in lit surface drew, otherwise 0 |
| `fallback_specular` | rgb: the specular term `engine_material_indirect_lighting` added to the opaque colour (probe / environment specular; zero without native indirect lighting) |

Targets are cleared to 0 every frame and read with the final scene depth, so a
pixel is described only if the built-in lit surface is the visible one. The
pass uses the same programs, material uniforms and Hi-Z compacted instances as
`engine.opaque`.

**Non-reflective materials (explicit).** Custom-shader materials
(`MeshMaterial.shader` or a `SceneMesh::shader`) have no surface-material
output: the opaque pass draws them, but they keep coverage 0, so they receive
no reflection (and they can still be *reflected*, because the trace reads the
scene colour). Unlit built-in materials write coverage 0 as well. Transparent
meshes are not in the opaque pass. There is no optional output for custom
shaders in this release; adding one would need a second colour attachment in
their opaque pipelines.

## Trace and composite

Per pixel with coverage and `roughness < roughness_cutoff`:

1. Reconstruct the view-space position from depth (the jittered projection that
   rasterized it) and the view direction (constant for orthographic views).
2. Reflect the view direction about the shaded normal. The start is lifted off
   the surface by `0.01 + 0.0005 z` and the ray is clipped to the camera's
   front half space and `max_distance`.
3. Project the ray to the screen and march it with perspective-correct depth.
   The segment is first clipped to the render-size rectangle, so rays that leave
   the viewport spend no samples off-screen. The number of samples is
   `min(max_steps, ceil(on-screen pixels covered))`; beyond that the stride grows (thin occluders can then be missed). A sample hits when the
   surface depth at its pixel lies within `[near - thickness, far]` of the ray
   depth interval of the step. A hit is refined by five bisections and
   re-checked against the thickness.
4. Radiance at the hit is the opaque scene colour of the hit pixel; rougher
   surfaces average a five-tap cross whose spread grows with roughness and the
   screen distance travelled (a cone approximation; there is no colour mip chain).
5. The traced radiance is scaled by the analytic split-sum environment BRDF
   (Karis) of the pixel's F0, roughness and `N.V`, times `intensity`.
6. `result = scene + w * (traced - fallback_specular)`, floored at 0, where the
   weight `w` is the product of
   * the roughness fade `1 - smoothstep(cutoff / 2, cutoff, roughness)`,
   * the screen-edge fade of the **hit** position over `edge_fade` of the screen,
   * the distance fade `1 - smoothstep(0.75, 1, travelled / max_distance)`,
   * the facing fade `1 - smoothstep(0, 0.5, dir.z)` (rays that head back towards
     the camera cannot be validated on screen).

A miss, an off-screen exit, a faded-out hit, a pixel above the cutoff and any
pixel without coverage return the opaque colour **unchanged** (a bit-identical
copy), so the existing image-based or ambient specular is the fallback and no
black holes appear. A hit with `w = 1` *replaces* the image-based specular; it
is not added to it.

## Settings

`ViewData::ssr: Option<SsrSettings>` (`engine_render_pipeline::SsrSettings`,
validated by `ViewData::validate` and before authored extraction):

| Field | Range | Default | Meaning |
| --- | --- | --- | --- |
| `max_steps` | 1..=256 | 64 | samples per ray |
| `max_distance` | (0, 10000] | 30 | longest ray, view units |
| `thickness` | (0, 100] | 0.5 | depth a ray may sit behind a surface and still hit it |
| `roughness_cutoff` | (0, 1] | 0.4 | roughness above which a pixel is not reflected |
| `edge_fade` | [0, 0.5] | 0.1 | screen-fraction fade of the hit position at the borders |
| `intensity` | [0, 4] | 1 | traced radiance multiplier; 0 skips both passes |

`Some` enables the feature unless the view overrides it; `None` leaves it off
unless the view enables `engine.ssr` through `FeatureOverrides`, which then uses
the defaults. An explicit `false` always wins.

Authored on `Camera3D.screen_space_reflections` (same fields, snake_case): see
[project-format.md](project-format.md). Omitted or `null` is off and serializes
without the key, so existing projects render byte-identically. A present object
(`{}` for the defaults) sets `render_features["engine.ssr"]` to true unless the
camera sets it explicitly. `engine-project` validates the bounds at
`Camera3D::validate`; the desktop renderer revalidates and maps them to
`ViewData::ssr` on every frame, so removing the settings or switching cameras
leaves nothing behind.

## RT reflections versus SSR

`engine.rt.reflections` (hardware tier, [rendering-raytracing.md](rendering-raytracing.md))
and `engine.ssr` are mutually exclusive per view, resolved in
`FeatureRegistry::resolve`: when both are enabled, **the RT feature wins** and
`engine.ssr` is reported `enabled: true`, `activity: superseded` (in the
pipeline `Activity::Superseded("engine.rt.reflections")`). A superseded SSR
requests no surface material, allocates nothing and leaves the frame untouched.
Other views are unaffected. The registry keys on the *enabled* RT feature, so
the desktop host (`RtHost::prepare`) only leaves it enabled when RT can run:
when the RT device is unavailable (the adapter lacks ray query, the device
request failed or `note_ray_tracing_request` reported an error) and SSR will
actually run for the view (settings present with a positive `intensity` and no
explicit `render_features["engine.ssr"] = false`), the host withdraws the
`engine.rt.reflections` request for that view. SSR then runs, and the
ray-tracing report shows reflections as `fallback` with `fallback: "ssr"`; its
`reason` starts with the SSR note, followed by the unavailability reason. If
SSR would not run, the request stays and reflections fall back to nothing. With an RT device, an RT
request that falls back for a frame (for example an empty scene) still keeps
the view, so SSR stays `superseded` there.

## Interaction with other features

* **Image-based lighting and probes.** Misses keep the probe/environment
  specular. Note the engine's existing image-based specular at perceptual
  roughness near 0 is attenuated by its capped GGX distribution (about 4.5% of
  the environment for a mirror); the analytic SSR factor is not, so a mirror hit
  can be brighter than the miss around it.
* **Lightmaps.** The lightmapped lit variant gets a surface-material variant
  too; baked diffuse does not change the specular fallback.
* **Hi-Z.** The surface pass draws the same compacted indirect instances as the
  opaque pass; real-adapter coverage checks identical pixels.
* **Decals and aerial perspective.** `OpaqueEffects` order is
  `engine.surface_material`, `engine.decals`, `engine.ssr`,
  `engine.atmosphere.aerial` (decals are `.before("engine.ssr")`, aerial is
  `.after("engine.ssr")`), followed by `engine.fog` (`.after` both). SSR
  therefore reflects decorated surfaces, and aerial perspective and fog then
  tint the SSR result, so a reflection is fogged by the view distance of the
  reflecting pixel (not by the reflected ray's own length); see
  [rendering-fog.md](rendering-fog.md).
  `gpu_ssr.rs::aerial_perspective_fogs_the_reflection_after_ssr` reads this back.
* **Upscalers, post, DLSS.** SSR runs at render size before `Upscale`;
  `scene_color()` is the SSR destination for everything later.
* **Resize, views, forget.** Surface targets are per view and follow its render
  size; a closed view releases them; views are independent.

## Limits

* Screen space only: off-screen, occluded and back-facing geometry cannot be
  reflected, and transparent meshes are neither reflective nor reflected.
* The surface-material pass relies on the vertex stage producing exactly the same
  clip depth as the opaque pass: both use the same vertex code and instance data
  and the depth test is `LessEqual` against the final scene depth, but the shader
  declares no `@invariant` position. Identical results are expected, not
  guaranteed by the API; only the NVIDIA adapter used for validation has been
  tested. A vendor that computed the two passes differently would lose coverage
  (pixels fall back to non-reflective) or flicker at depth ties.
* No temporal accumulation or denoise: the march follows the jittered projection
  that rasterized the depth, so a jittering view reflects correctly, but there is
  no history. Rough reflections use the five-tap cone approximation and are not
  physically filtered; thin occluders can be missed when the stride exceeds one
  pixel.
* The hit radiance is the opaque colour of the hit pixel (its direct lighting and
  image-based specular, but no reflection of its own: one bounce); rays that
  leave through the sky or the far plane are misses.
* Custom-shader and unlit materials are non-reflective; the surface-material pass
  draws the built-in lit batches a second time (extra vertex and fragment work,
  not profiled).
* Hi-Z pyramid acceleration of the march is not used.
* Vertex displacement of custom shaders is not replayed (they are not drawn
  by the pass); the built-in path uses the engine transforms.
* With an RT device, `engine.rt.reflections` keeps the view even for frames where
  it falls back; only a missing device hands the view to SSR.
* Quality and cost at high resolution have not been profiled.

## Validation

* CPU: `ssr` settings bounds, registration and stage order, the RT exclusion,
  the surface-material shader derivation (`engine-render-pipeline` unit tests);
  authored JSON to runtime, closed schema and bounds
  (`engine-project/tests/render_ssr.rs`).
* Real GPU (`#[ignore]`, `--ignored --test-threads=1`):
  `engine-render-pipeline/tests/gpu_ssr.rs` reads back a metallic mirror floor
  under a red unlit wall (the wall appears at the mirrored pixel with SSR on, the
  environment or black without it), misses and off-screen reflectors, the
  roughness cutoff and partial fade, the surface-material targets (shaded normal
  with a normal map, F0, coverage, custom shader and unlit exclusion), feature-off
  identity, zero intensity, an RT-reflection exclusion, an orthographic view,
  resize, two independent views, forget and the Hi-Z compacted path, a view with
  non-zero projection jitter (reflection within one texel of the unjittered
  one) and a ray that leaves the screen sideways with a small step budget.
  `engine-desktop/tests/gpu_authored_ssr.rs` does the same from project JSON
  through the desktop renderer, including an RT request on a device without ray
  tracing (SSR runs, the RT report shows `fallback: "ssr"`).
  `engine-desktop/tests/ray_tracing.rs` (needs ray query) checks both sides of the
  exclusion: with an RT device `engine.ssr` is `superseded`, without one it runs.
