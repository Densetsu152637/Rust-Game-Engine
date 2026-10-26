# Material node graphs

The `engine-material-graph` crate defines the `material` graph kind. A graph is a
version 1 `engine-graph::GraphDocument` with exactly one `material_output` node.
Edges flow from producer outputs to consumer inputs. Every node must contribute
to the output. Material graphs have at most 64 nodes, 128 edges, no variables,
and a 64 KiB generated WGSL source limit. The shared graph validator also
enforces unique IDs, typed ports, required inputs, acyclic edges, and bounded
parameter values.

`material_kind()` supplies the editor palette and validation schema.
`compile_material(&document)` returns a `CompiledMaterial { source }` or
stable-path diagnostics. Pass `&source` to
`engine_render_pipeline::compose_material(source, MaterialTarget::Mesh)` or
`MaterialTarget::Sprite` before use. The compiler owns no GPU resources or
binding declarations. The source imports `engine::material` and uses the
existing texture, tint, metallic, roughness, normal map, lighting, and shadow
contract. Meshes retain the built-in lighting path. Both unlit meshes and sprites use the graph base color directly. Sprites remain unlit.

| Node | Inputs | Outputs | Parameters |
| --- | --- | --- | --- |
| `scalar` | — | `value: Float` | required finite `value` |
| `vec3` | — | `value: Vec3` | required finite three-component `value` |
| `uv` | — | `value: material_vec2` | — |
| `tint` | — | `color: Vec3`, `alpha: Float` | — |
| `metallic`, `roughness` | — | `value: Float` | — |
| `texture_color` | optional `uv: material_vec2` | `color: Vec3`, `alpha: Float` | — |
| `add_float`, `multiply_float` | required `a, b: Float` | `value: Float` | — |
| `add_vec3`, `multiply_vec3` | required `a, b: Vec3` | `value: Vec3` | — |
| `mix_vec3` | required `a, b: Vec3`, `factor: Float` | `value: Vec3` | — |
| `material_output` | optional `base_color: Vec3`, `alpha: Float`, `metallic: Float`, `roughness: Float` | — | — |

An unconnected output input uses the built-in material's matching value:
sampled image times vertex color and tint for base color and alpha, and
material options times the metallic-roughness map channels where enabled for
metallic and roughness. An unconnected `texture_color.uv` uses mesh/sprite UVs.
The material output sanitizes non-finite arithmetic results and clamps alpha,
metallic, and roughness to 0–1. Base color is nonnegative HDR and capped per
channel at 65504. Arithmetic nodes cap intermediate values to finite f32
range; `mix_vec3.factor` is clamped to 0–1. These guards bound generated
expression results; lighting and user-supplied GPU uniforms still follow the
existing material contract.

The node vocabulary is fixed. Authored documents cannot supply raw WGSL,
resource bindings, custom uniforms, displacement, or arbitrary texture slots.
The compiler emits expressions in node-ID topological order so JSON node and
edge array permutations produce identical source. Generated shader compilation
still uses `compose_material` and its WGSL validation on each target.
