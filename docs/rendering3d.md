# Scene rendering

The desktop scene path combines depth-tested 3D meshes and the existing 2D sprite
and UI overlay in one `engine-render` command submission. The wgpu backend owns
the color and depth targets; a second submission must not clear the world pass.
The original `RenderFrame`, `Draw`, `SpriteRenderer::render` and offscreen sprite
APIs remain available for 2D callers. Scene callers use `SceneRenderFrame`,
`Runtime::extract_scene_frame` and `render_scene`.

`Camera3D` is an authored/native component. Projection is a tagged `perspective`
or `orthographic` value; perspective uses `fov_y_radians`, `near` and `far`, and
orthographic uses `height`, `near` and `far`. Coordinates are right-handed, Y up,
camera forward negative Z, and normalized GPU depth 0 to 1. Camera transforms
must be invertible. When multiple cameras are active, the stable authored ID
selects the first deterministically. A scene without a 3D camera follows its
existing 2D extraction behavior.

`MeshMaterial` supplies `shader` and `texture` asset references, RGBA `tint`,
`unlit` and `transparent`. Missing shader selects the built-in triangle-lit WGSL
shader. Materials also apply to 2D sprites and meshes; their default material is
unlit and existing painter/depth settings remain authoritative. Lighting uses derivatives of world positions, so nonuniform scale affects
the geometric normal correctly. This is simple two-sided directional lighting,
not a physically based material, shadow system or skinned animation renderer.
Opaque draws write depth; transparent draws test depth, do not write it, and sort
back-to-front by object origin with stable IDs breaking ties.

Custom WGSL goes through the real rendering path. Vertex locations are 0 position
(`vec3<f32>`), 1 UV (`vec2<f32>`) and 2 color (`vec4<f32>`). Entry points default to
`vs_main` and `fs_main`. Supported uniform slots are:

| Group/binding | Value | Size |
| --- | --- | --- |
| 0/0 | camera projection × view × model, column-major matrix | 64 bytes |
| 0/1 | model-to-world column-major matrix | 64 bytes |
| 2/0 | tint vec4 followed by options vec4; options.x is unlit | 32 bytes |

Texture 1/0 and nearest clamp sampler 1/1 preserve the shared render contract.
Only bindings actually used by the shader are supplied. Other uniform slots,
wrong byte sizes, missing vertex inputs and invalid WGSL return diagnostics
before frame submission. Arbitrary storage buffers, texture arrays and PBR
material graphs are not part of this material interface. Native wgpu passes
remain available for advanced effects.

Run `cargo test -p engine-desktop --test scene3d` for CPU contract tests, and
append `-- --include-ignored --nocapture` for real GPU readback tests. GPU tests
must fail, rather than silently skip, when no adapter is available.
