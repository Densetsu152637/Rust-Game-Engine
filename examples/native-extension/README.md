# External native extension fixture

This standalone game Cargo crate uses only public engine-core, engine-project and
engine-desktop APIs. Its configure function defines a heap-owning CustomActor
component (String/Vec), CustomState resource (Vec<String>), CustomBundle and ordered
GameSystems sets. Native systems mutate these types and the core extracts custom
components into its separate render world. No authored schema or reflection
registration is required for arbitrary native types.

CustomTriangle implements the public NativeRenderPass trait. It owns its uniform
buffer, bind group, pipeline layout and WGSL shader/pipeline. prepare creates GPU
resources for the host target format; encode reads an extracted custom component,
writes its color uniform and issues a real wgpu render pass after engine draws.
The same registration is taken by the native desktop runner at startup.

From the repository root:

    cargo test --manifest-path examples/native-extension/Cargo.toml
    cargo test --manifest-path examples/native-extension/Cargo.toml external_custom_pass_verifies_real_offscreen_pixels -- --ignored --nocapture

The first command checks heap values, native system ordering and extraction. The
explicit GPU test creates a real adapter/device and offscreen RGBA target, renders
a green WGSL triangle, reads actual pixels, changes the custom component to red,
extracts again and requires red pixels on a second real submission. An outside
pixel must differ from the triangle. Device/validation/readback failures fail the
test; no CPU mock or silent skip substitutes for GPU success.

The project.json manifest also permits compiling this game through the native
runtime compiler. Its authored scene is empty because configure spawns the custom
native entity. The fixture does not establish arbitrary third-party shader
correctness, visible-window behavior or cross-platform device acceptance.