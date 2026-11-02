# Projected decals

`engine.decals` paints oriented-box decals onto the opaque scene. Each decal is
a box in world space; every visible surface point inside the box, reconstructed
from the scene depth, receives the decal's colour response. Decals follow the
surface they land on (floors, walls, curved meshes) without changing geometry.

The pass is a native feature of `engine-render-pipeline`
([rendering-architecture.md](rendering-architecture.md)); the authored path is
`Camera3D.decals` ([project-format.md](project-format.md)).

## Model

For every decal and every pixel in the decal's screen rectangle:

1. Read scene depth. Sky and cleared depth (1.0) are skipped.
2. Reconstruct the world position with the inverse of the jittered
   view-projection (the transform the opaque pass rasterised with).
3. Transform into the decal's box space, where the volume is the cube
   `[-1, 1]^3`; pixels outside it are skipped, so a surface in front of or
   behind the box is never decorated (**depth respect**).
4. Edge fade: `smoothstep(0, edge_fade, 1 - max(|x|, |y|, |z|))`, the distance
   to the nearest face as a fraction of the half extent. `edge_fade = 0` is a
   hard edge.
5. Angle fade: the surface normal (the world normals prepass) against the
   decal's projection axis. The decal faces its local **+Z** and projects
   toward -Z, so a surface receives it when `dot(normal, axis) > 0`. Between
   `max_angle - angle_fade` and `max_angle` the weight falls smoothly to zero;
   surfaces at or beyond `max_angle`, edge-on surfaces and surfaces facing away
   get nothing. A pixel without a normal (coverage 0) skips the angle fade.
6. Optional texture, sampled over the local XY face (u along +X, v along -Y,
   bilinear, clamped): RGB multiplies the albedo, alpha multiplies coverage.
7. Material response (below), written over the previous scene colour.

Decals apply in list order, each reading the result of the previous one, so a
later decal draws on top of an earlier one.

### Material response

The pipeline is forward-lit: there is no G-buffer, so decals cannot change the
inputs of lighting. The pass is therefore a **lit-colour modulation** applied
to the already-lit HDR scene colour:

| Response | Field | Effect |
| --- | --- | --- |
| Albedo, multiply | `blend: multiply` (default), `albedo`, `opacity` | `lit * lerp(1, albedo, opacity * coverage)`: stains, grime, tint. Cannot brighten and cannot paint onto black. |
| Albedo, paint | `blend: paint` | `lerp(lit, albedo * luma(lit), opacity * coverage)`: replaces the hue and keeps the lit brightness. Exact for a white diffuse surface; other surfaces are an approximation, because the original albedo is unknown. |
| Emissive | `emissive` | `+ emissive * coverage`. Not scaled by `opacity`, so `opacity: 0` is an emissive-only decal. |

**Not implemented: normal and roughness response.** Without a G-buffer, a
decal cannot re-shade the surface, so there are no decal normal maps, roughness
or metallic overrides, and specular highlights and reflections of the base
surface are tinted with the diffuse light. Lighting-correct decals would need
the opaque pass to write a material target; that is out of scope here.

Because decals run after lighting, they do not receive their own shadows and
are not lit by lights the base surface ignores; they inherit whatever
lighting the surface had.

## Placement

`engine.decals` runs in the `OpaqueEffects` stage (after Opaque and Sky, before
Transparent), registered disabled by default. It needs the normals prepass
(`FeatureNeeds::normals`), which the pipeline then runs. Consequences:

- Only opaque built-in meshes are decorated. Transparent geometry is drawn
  after the pass and is neither decorated nor does it hide or shift decals.
- `engine.decals` is ordered `.before("engine.ssr")`: SSR reflects decorated
  surfaces, and `engine.atmosphere.aerial` (`.after("engine.ssr")`) tints them,
  and `engine.fog` (after both, [rendering-fog.md](rendering-fog.md)) fogs them,
  so the `OpaqueEffects` order is decals, SSR, aerial perspective, fog. Later stages
  (upscaling, post-processing) see the decorated colour.
- The pass reads `ScenePipeline` depth at render size, so it works with every
  upscale route (the decorated colour is the scene colour).

A non-empty `ViewData::decals` turns the feature on unless the view disables it
(`FeatureOverrides` entry `engine.decals = false`); an empty list leaves it
off, so existing images are unchanged.

### Cost

One full-image copy to the stage destination, then per decal one scissored draw
over the decal's screen rectangle plus a copy of that rectangle back to the
source. Decals whose volume is entirely off screen or behind the camera, and
decals that change nothing (opacity 0 and emissive 0), are not drawn. A box that
straddles the camera plane covers the whole target.

## Native API

```rust
let mut view = ViewData::new(...)?;
view.decals = vec![Decal {
    center: Vec3::new(0.0, 0.0, 0.0),
    rotation: Quat::from_rotation_arc(Vec3::Z, Vec3::Y), // faces +Y
    half_extents: Vec3::splat(1.0),
    albedo: Vec3::new(1.0, 0.1, 0.1),
    ..Default::default()
}];
```

`Decal::validate` is a pure function; the pass validates every decal each frame
and a failure stops the frame naming the decal index (`decal 1: decal opacity
must be finite in 0..=1`). `ScenePipeline::decal_report(view)` returns the last
`DecalReport` (`submitted`, `drawn`, `culled`, `dropped_overflow`,
`missing_textures`); it is absent while the view has no running decal pass.

| Field | Range / default |
| --- | --- |
| `center` | finite, within 100000 of the origin |
| `rotation` | finite nonzero quaternion (normalised on use) |
| `half_extents` | each in `(0, 100000]`, default 0.5 |
| `albedo` | each channel `0..=1`, default white |
| `opacity` | `0..=1`, default 1 |
| `emissive` | each channel `0..=1000`, default 0 |
| `max_angle` | `(0, pi/2]` radians, default 70 degrees |
| `angle_fade` | `0..=max_angle` radians, default 15 degrees |
| `edge_fade` | `0..=1`, default 0.1 |
| `texture` | `None`, `Texture(Arc<GpuTexture>)` or `Missing` |

### Limits and overflow

- A view **draws at most 64 decals** (`MAX_DECALS`). Valid decals past the 64th
  in list order are dropped and counted in `DecalReport::dropped_overflow`;
  the frame still renders.
- A view **accepts at most 1024 submitted decals** (`MAX_SUBMITTED_DECALS`).
  A longer list is invalid input: `ViewData::validate` rejects it (and any
  invalid decal, naming its index) when the frame starts, before any stage is
  encoded, so the view's histories are untouched.
- Authored cameras are stricter: `Camera3D.decals` accepts at most 64 entries,
  and validation rejects more (`camera3d.decals.too_many`), so the dropped path
  only occurs for native callers.

### Missing textures

`DecalTexture::Missing` stands for a texture that was requested but could not
be resolved. The decal renders flat (albedo and opacity only), is counted in
`DecalReport::missing_textures`, and is never an error. The authored path does
not carry textures yet, so authored decals are flat.

## Authored settings

`Camera3D.decals` (default empty; absent or `[]` = off, byte-identical output)
is a list of up to 64 entries:

| Key | Default | Meaning |
| --- | --- | --- |
| `position` | `[0,0,0]` | world position of the box centre |
| `rotation` | `[0,0,0,1]` | orientation quaternion `[x,y,z,w]` |
| `half_extents` | `[0.5,0.5,0.5]` | half size along local X, Y, Z |
| `albedo` | `[1,1,1]` | linear albedo |
| `opacity` | `1` | opacity of the albedo response |
| `blend` | `"multiply"` | `"multiply"` or `"paint"` |
| `emissive` | `[0,0,0]` | added radiance (HDR scene units) |
| `max_angle` | `1.2217` | radians; surface-normal cut-off |
| `angle_fade` | `0.2618` | radians of fade below `max_angle` |
| `edge_fade` | `0.1` | fraction of the half extent faded at each face |

Unknown keys and out-of-range or non-finite values are rejected when the
project loads (`camera3d.decals.invalid`). A non-empty list enables
`engine.decals`; `render_features["engine.decals"] = false` wins. The desktop
host translates positions by the applied floating origin and validates again
before submission; the camera's list replaces the previous one every frame, so
removing the decals restores the undecorated image exactly.

Authored decals live on the camera, not on entities, so moving or animating a
decal means rewriting the camera's list.

## Validation

Pure CPU tests (`cargo test -p engine-render-pipeline decals`, `-p engine-project
--test render_decals`, `-p engine-desktop decals`) cover input validation,
overflow, culling, ordering, uniform packing and the authored schema.

Ignored GPU readback tests, run with `--ignored --test-threads=1` on a real
adapter:

- `crates/engine-render-pipeline/tests/gpu_decals.rs`: pixels change inside the
  projected volume and stay bit-identical outside; a box that does not reach the
  surface and a nearer surface outside the box leave pixels unchanged;
  angle-fade (facing, edge-on, facing away, 45 degree tilt against 30 and 60
  degree cut-offs, soft cut-off); edge fade; opacity, paint and emissive
  response; list order; texture alpha, `Missing` textures and the report;
  overflow keeping the first 64; an invalid decal failing the frame; resize,
  removal restoring the baseline and an explicit disable.
- `crates/engine-desktop/tests/gpu_authored_decals.rs`: project JSON to pixels,
  removal, the `render_features` switch, resize and a rejected invalid decal.

These tests were written without being run in the session that added them;
GPU results are not yet recorded.

## Limits

- Lit-colour modulation only: no normal, roughness or metallic response, and no
  re-lighting (see Material response).
- Opaque geometry in the scene depth only. Angle fade uses the engine normals
  prepass, which draws opaque built-in-path meshes with engine transforms
  (custom vertex displacement is not applied there), so a mesh with a custom
  shader is decorated by its depth but fades by the undisplaced normal.
- Authored decals are flat (no texture) and camera-attached.
- Each decal costs a rectangle copy; 64 large overlapping decals at high
  resolution are expensive. No performance numbers have been measured.
- The reconstruction uses the jittered inverse view-projection the opaque pass
  rasterised with, so it is consistent under temporal jitter; there is no
  decal history.
