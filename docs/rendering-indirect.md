# Native indirect-lighting resource preparation

Source milestone: native input validation, immutable float GPU resources and
optional per-view material IBL/probe consumption. The module is
`engine_render_pipeline::indirect`. A project loader and desktop adapter accept
externally prepared environment data for the built-in 3D mesh path. The desktop
host lifecycle hook and assembled GPU verification remain pending in this
source slice; authored rendering is not yet claimed.

## Authored project input

Declare a project asset of kind `indirect` and reference its ID from
`Camera3D.lighting.indirect`. Absence uses constant ambient. Unsupported
authored requests return an error. A version-1 descriptor contains
`environments`, optional `global`, and `probes`:

```json
{
  "version": 1,
  "environments": [{"edge": 1,
    "diffuse": [[1, 1, 1], [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0],
                [0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 0, 0]],
    "mips": [["px.rgb32f", "nx.rgb32f", "py.rgb32f", "ny.rgb32f",
              "pz.rgb32f", "nz.rgb32f"]]}],
  "global": {"environment": 0, "rotation": [0, 0, 0, 1], "intensity": 1},
  "probes": [{"id": "room", "positionAbsolute": [0, 0, 0],
              "scale": [1, 1, 1], "halfExtents": [4, 3, 4],
              "blendDistance": 1, "priority": 0,
              "lighting": {"environment": 0, "rotation": [0, 0, 0, 1],
                           "intensity": 1}}]
}
```

Each face file is exactly `edge * edge * 12` bytes of little-endian,
row-major, top-row-first RGB32F. The first mip is `edge` squared; each mip
halves the edge down to 1. All paths are project-relative and confined to the
project root. The loader rejects unknown fields, incorrect lengths, nonfinite
or negative radiance, radiance above 65504, invalid references/transforms,
duplicate probe IDs, and incomplete mips. It preflights every descriptor and
face length before bulk decoding: at most 1 MiB per descriptor, 16 MiB for all
descriptors, and 128 MiB decoded RGB32F across all indirect assets. GPU
resources are separately bounded to 128 MiB across active view sets. The
native integrator generates the BRDF LUT; supplied cube chains must already
be GGX-prefiltered. This path does not capture probes, prefilter environments,
consume lightmaps, or bake a scene.

Probe `positionAbsolute` is f64 world space. Extraction subtracts the applied
floating origin in f64 before producing local f32 probe volumes. A rebase
rebuilds probe volumes while retaining uploaded images. The shapes are static
world-space volumes; World terrain and foliage shaders do not yet sample IBL.

## Input and precision contract

`RadianceImage::new(size, pixels)` accepts top-row-first, linear RGB32 radiance.
Dimensions are 1..2048 per edge, pixel count must match exactly, and every channel
must be finite and within 0..65504. Upload converts to `Rgba16Float` (alpha 1) using the engine's repaired carry-aware half conversion. This introduces approximately 1/1024
relative quantization for normal half values; tiny values can underflow. It never
applies sRGB conversion, exposure or an implicit clamp to 0..1. `scaled` is an
explicit validated operation: unrepresentable results fail rather than saturate.
Integrators must apply camera exposure exactly once, separately from already
exposed screen GI/reflection inputs.

`EnvironmentData::new` requires square power-of-two cube faces up to 256, with a
complete mip chain down to 1x1. Face order is +X,-X,+Y,-Y,+Z,-Z. The texture is a
six-layer D2 resource with a Cube view. For direction (x,y,z), the cube's face-local
(s,t) coordinates before mapping -1..1 to UV are:

| Face | s | t |
| --- | --- | --- |
| +X | -z/abs(x) | -y/abs(x) |
| -X | z/abs(x) | -y/abs(x) |
| +Y | x/abs(y) | z/abs(y) |
| -Y | x/abs(y) | -z/abs(y) |
| +Z | x/abs(z) | -y/abs(z) |
| -Z | -x/abs(z) | -y/abs(z) |

The supplied specular chain must already be GGX-prefiltered. The intended material
LOD is perceptual roughness times (mip_count - 1). Validation establishes dimensions
and finite data, not whether an external producer actually used GGX convolution.
No runtime convolution, probe capture, UV unwrapping or scene baking is provided.

`DiffusePolynomial` stores preconvolved incident diffuse radiance (irradiance/pi)
in the exact local-direction basis `[1,x,y,z,xy,yz,3z*z-1,xz,x*x-y*y]`.
Normalization factors are absorbed into its nine RGB coefficient triples. Signed
finite coefficients within half magnitude are accepted; evaluation requires a
unit vector and clamps reconstructed radiance nonnegative. CPU evaluation uses
f32 and does not truncate coefficients to half. Environment rotation must rotate
the sample direction into this same local space when materials are integrated.

## Probe selection and budgets

`ProbeVolume` validates an affine invertible transform, positive half extents,
local blend distance and a stable 1..64-byte ID. Extents are at most 1,000,000;
blend distance must fit the smallest half extent. `select_probe` accepts at most
16 enabled volumes and a finite world sample position. Highest priority wins;
equal priority picks the lexicographically first ID. Duplicate IDs fail.

Selection returns at most one local index plus complementary local/global weights.
Inside the box, smoothstep of the minimum local face distance over blend distance
controls the local weight; outside it global weight is 1. Zero blend distance is
a hard boundary. Transforms can include scale: blend distance uses **local units**.
Resolve the returned volume's environment externally. The intended first material
adapter samples at a draw's transformed origin; there is no dense probe grid,
per-fragment volume interpolation or box-projected reflection correction.

`IndirectBudget` admits distinct resources atomically: at most 17 environments,
8 lightmaps, 128 MiB decoded RGB32 payload, and separately 128 MiB resident RGBA16
payload. `GpuIndirectResources::prepare` validates the whole set and device sizes
before uploading anything; its aggregate budget includes the shared BRDF LUT.
Callers sharing assets count them once in the set. This is not a global GPU
allocator, cache eviction policy or a budget for all unrelated engine textures.

## BRDF and GPU resource lifetime

`BrdfLut::prepare` computes split-sum coefficients A/B for `F0*A+B`. It follows the
existing engine BRDF's perceptual roughness floor 0.045, GGX distribution,
separable Smith masking and Schlick Fresnel, including the existing shader's
numerical regularization: NdotV >= 1e-4, distribution denominator >= 1e-7,
half-vector length and local Smith denominator >= 1e-6, and final specular
denominator >= 1e-6. Geometry retains the actual view direction before the NdotV
floor. CPU preparation uses f64 inversion/accumulation and deterministic bit-reversal
samples of the actual capped GGX distribution. Ideal GGX sampling undersamples its
remaining tail after the shader floor suppresses the narrow peak, especially at
low roughness and grazing angles. Precision promotion alone does not repair that
coverage problem. This matches the mathematical regularized shader, without
claiming bitwise agreement with its GPU f32 arithmetic.

For `u=NdotH^2`, `a=max(roughness,0.045)^4`, `k=sqrt(1e-7/pi)` and
`d=1-(1-a)*u`, the unnormalized density is `pi*D=a/max(d^2,k^2)`.
When `a>=k`, it is ordinary GGX with mass `Z=1`. Otherwise the CDF changes at
`u0=(1-k)/(1-a)`: lower mass is `T=a/(1-a)*(1/k-1)` and the constant-density
upper mass is `C=a*(k-a)/((1-a)*k^2)`, with `Z=T+C`. Inverting these two branches
spends samples on the distribution that survives the cap. The half-vector PDF is
`D_sample*NdotH/Z`, and the light-direction PDF divides by `4*VdotH`.
Integration retains both `Z` and `D_shader/D_sample`, evaluating the shader's
reconstructed, length-floored half vector separately. No shared BRDF shader is
changed by this native preparation. Independent numerical density/CDF regression
sources cover both regimes, their transition and the roughness-one endpoint;
light-direction quadrature retains its original accuracy thresholds. Bounded
standalone CPU diagnostics checked these formulas; Cargo revalidation remains a
separate gate. Texel centers are x=NdotV and
y=roughness at `(i+0.5)/size`. Size is 8..128, samples 64..4096, with at most
16,777,216 sample evaluations. This approximation is BRDF preparation, not lighting
baking. The LUT uploads as R=A, G=B, B=0, A=1 in `Rgba16Float`.

`check_capabilities` requires filterable Rgba16Float sampled/copy usages and six
array layers. Unsupported capability returns an error naming the missing resource
requirement; a future renderer may report constant-ambient fallback. No successful
material feature status is fabricated. No optional feature is requested after the
device has been created. The native material adapter additionally validates its optional layout limits;
the legacy material layouts remain available on a restricted device.

`GpuIndirectResources` owns environment cubes, lightmaps, shared LUT and a linear
clamp sampler. They can be shared across views/resizes and dropped through normal
wgpu ownership. Preparation issues uploads and checks currently recorded errors;
it does not wait for GPU completion or confirm presented pixels. Tests use actual
readback to establish completion. There is no camera/history/scene-stage coupling.

## Verification sources and integration still required

CPU source tests in `tests/indirect.rs` cover invalid data, dimensions/mips, signed
diffuse evaluation, exposure overflow, atomic budgets, transformed deterministic
probe selection, limits and BRDF comparison against independent f64 light-direction quadrature of the actual regularized
shader (production samples half vectors in f32). Low-roughness .045/.1 cases
include grazing NdotV down to 5e-5, below the shader's view floor. The oracle
clusters light-direction z and azimuth near the mirror ray using explicit
change-of-variable Jacobians; it has no ideal-GGX PDF cancellation. Its 512/1024
midpoint grids must agree within 0.001 + 1% per coefficient before comparison.
The bounded 4096-sample production comparison allows 0.002 + 4% per coefficient;
GPU-upload comparison adds 0.001 for half quantization. These are declared
acceptance tolerances, not measured errors: the new tests remain unrun. The GPU
LUT fixture uses size 15 so its first two rows test roughness floored to .045
and exactly .1, including the first (grazing) view-angle column.

Ignored GPU source tests in `tests/gpu_indirect.rs` copy and read every uploaded
cube face/mip, read a non-square HDR lightmap, sample axes/off-axis directions and fractional LOD through the actual Cube view in a
compute shader, verify independent resource replacement does not mutate the first
set, and read LUT texels against CPU quadrature. They require a real adapter:

```text
cargo test -p engine-render-pipeline --test indirect
cargo test -p engine-render-pipeline --test gpu_indirect -- --ignored --test-threads=1 --nocapture
```

## Native material adapter (source, GPU verification pending)

Configure each view independently:

```rust,ignore
let prepared = Arc::new(GpuIndirectResources::prepare(&gpu, &environments, &[], &lut)?);
let global = EnvironmentLighting::new(0, glam::Quat::IDENTITY, 1.0)?;
let scene = Arc::new(IndirectScene::new(prepared, Some(global), probes)?);
pipeline.set_indirect(ViewId::MAIN, Some(scene))?;
// Explicitly restore legacy ambient/screen lighting:
pipeline.set_indirect(ViewId::MAIN, None)?;
```

`IndirectScene::new` validates environment indices, unique probe IDs and bounded
resource/probe counts. `EnvironmentLighting` requires a finite unit quaternion
(local environment axes to world axes) and intensity in 0..1000. Intensity includes
any desired camera exposure; the renderer does not add another exposure. The
shader rotates normals and reflected directions by the inverse rotation. Samples
use supplied diffuse irradiance/pi coefficients and GGX-prefiltered specular mips,
with split-sum BRDF coefficients. Diffuse carries nonmetallic weight and roughness
Fresnel energy weight; specular uses metallic F0 and material roughness. No sky
background, automatic probe capture, convolution or scene baking is implied.

Probe choice is evaluated at each draw's transformed origin. One highest-priority
local box blends against global lighting using the validated smooth boundary
weight; absent global data blends against the existing constant ambient instead.
Large meshes do not interpolate across several probes, and reflection parallax is
not box-corrected. A missing environment is different from an explicit zero-intensity
one: missing data retains legacy ambient; zero intensity intentionally supplies dark
indirect lighting. Finite input validation does not guarantee arbitrary accumulated
lighting fits the final half-float HDR target; callers still choose sensible exposure.

The pipeline keeps `SceneFrame`, `SceneMesh`, `EngineInstance` (208 bytes), group 0
and group 3 unchanged. It selects an optional material pipeline variant using the
same four groups. Legacy groups 1/2 are retained for all existing feature shaders.
The native variant adds these bindings:

| Group/binding | Resource |
| --- | --- |
| 1/4, 1/5 | global/local specular cube, or a shared black cube |
| 1/6 | BRDF LUT |
| 1/7 | linear clamp environment sampler |
| 2/1 | 448-byte dynamic uniform: two diffuse coefficient sets, inverse rotations, weights, mip bounds and intensity flags |

Material limits are 11 sampled textures, 3 samplers and 4 uniform bindings per
fragment stage, plus 3 dynamic uniform bindings. A restricted device keeps the
legacy pipeline; `set_indirect` returns a named error before changing the previous
configuration. Cross-device resource sets are rejected. No fifth group, instance
ABI change or mandatory new optional GPU feature is introduced.

The built-in material calls `engine_material_indirect_lighting`. Material graphs
inherit that call through their existing built-in template, without graph registry
or compiler edits. Raw custom shaders using the old `engine_indirect_lighting`
continue to receive ambient/screen lighting only. Opt into IBL by calling the new
material helper from raw `#import engine::material` source. Already composed legacy
source contains its neutral helper and remains neutral. The sprite implementation
of the new helper delegates to its existing unlit/ambient implementation and adds
no resource bindings. World terrain/foliage shaders and their layouts are unchanged;
this first adapter does not automatically add IBL to their custom shading paths.

Screen GI replaces the selected diffuse term rather than being added again. Screen
AO multiplies that diffuse term. An eligible screen reflection blends from IBL
specular to screen radiance with the existing roughness cutoff/fade; at full weight
it replaces IBL. With no IBL, this is the previous ambient/AO/GI/reflection behavior.
Direct lighting/shadows and explicit unlit materials retain their existing roles.

Per-view caches use resource IDs plus actual resolved coefficients/rotations/weights
in material identity, so differently illuminated draws cannot share the first draw's
uniforms. At most 4096 distinct indirect materials are admitted per view; overflow
fails before encoding. Uniform offsets/stride use device alignment and buffer/dynamic
offset budgets. Resolution happens one draw at a time; retained indirect uniforms
and texture groups are bounded by distinct material count, not an unbounded copy
of SH data per instance. Immutable prepared resources are shared across views/resizes.
Replacement/toggle releases stale material groups; camera reset clears those groups
without discarding lighting configuration. `forget_view` drops its configuration
and all view caches. Setting the same immutable Arc again leaves caches intact.

Ignored `gpu_indirect_shading` test sources render native lit meshes and read actual
scene HDR pixels. They cover constant HDR irradiance, independently integrated
metallic specular/roughness response, supplied mip selection, cube/diffuse rotation,
probe priority/tie/boundary blending, per-view isolation, resize/forget, missing/empty
data and toggling, legacy custom neutrality, screen GI/AO/reflection precedence,
invalid scene references and distinct-material limits. Source-only compiler tests
compose native, legacy and sprite variants; metadata tests cover bad rotations,
intensities and restricted limits. These tests have NOT run in this milestone:

```text
cargo test -p engine-render-pipeline --test gpu_indirect_shading -- --ignored --test-threads=1 --nocapture
```

Still required for the full rendering request: executed CPU/GPU validation and
source review; authored project/desktop extraction, float asset loading and schemas;
real UV2 and lightmap material consumption; relevant World material integration;
authored fixtures and runtime resource reload/status adapters. Native shaded test
sources do not establish user-facing authored IBL/probes/lightmaps acceptance.
