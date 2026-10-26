# Native indirect-lighting resource preparation

Source milestone: native input validation and immutable float GPU resources.
The module is `engine_render_pipeline::indirect`. It does **not** install IBL in
materials or provide an authored project adapter. No tests in this milestone
have been executed by its implementer; GPU tests require an explicit real adapter.

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
separable Smith masking and Schlick Fresnel. It importance-samples half vectors
with a deterministic bit-reversal sequence. Texel centers are x=NdotV and
y=roughness at `(i+0.5)/size`. Size is 8..128, samples 64..4096, with at most
16,777,216 sample evaluations. This approximation is BRDF preparation, not lighting
baking. The LUT uploads as R=A, G=B, B=0, A=1 in `Rgba16Float`.

`check_capabilities` requires filterable Rgba16Float sampled/copy usages and six
array layers. Unsupported capability returns an error naming the missing resource
requirement; a future renderer may report constant-ambient fallback. No successful
material feature status is fabricated. No optional feature is requested after the
device has been created. Material-layout texture/sampler/uniform limits remain a
separate integration gate because this milestone creates no material layout.

`GpuIndirectResources` owns environment cubes, lightmaps, shared LUT and a linear
clamp sampler. They can be shared across views/resizes and dropped through normal
wgpu ownership. Preparation issues uploads and checks currently recorded errors;
it does not wait for GPU completion or confirm presented pixels. Tests use actual
readback to establish completion. There is no camera/history/scene-stage coupling.

## Verification sources and integration still required

CPU source tests in `tests/indirect.rs` cover invalid data, dimensions/mips, signed
diffuse evaluation, exposure overflow, atomic budgets, transformed deterministic
probe selection, limits and BRDF comparison against independent f64 uniform-light
hemisphere quadrature (production samples half vectors in f32).

Ignored GPU source tests in `tests/gpu_indirect.rs` copy and read every uploaded
cube face/mip, read a non-square HDR lightmap, sample axes/off-axis directions and fractional LOD through the actual Cube view in a
compute shader, verify independent resource replacement does not mutate the first
set, and read LUT texels against CPU quadrature. They require a real adapter:

```text
cargo test -p engine-render-pipeline --test indirect
cargo test -p engine-render-pipeline --test gpu_indirect -- --ignored --test-threads=1 --nocapture
```

Still required: material bind groups/uniforms and shading composition, diffuse
rotation and reflection evaluation, screen GI/reflection precedence, real UV2,
per-draw probe/lightmap batch identity, native host extraction, float asset loading,
project asset kinds/components/schema/snapshot/reload adapters, authored fixtures,
and integrated material GPU readbacks. Existing sprite materials must retain a
valid neutral implementation, and material graph template regressions must run.
These resource tests do not establish that user-facing IBL/probes/lightmaps render.
