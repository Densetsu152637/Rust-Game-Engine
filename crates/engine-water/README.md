# engine-water

Pure-Rust surface water model (depends on `glam` and `serde` only; no GPU, ECS or file
access). `engine-project` builds the `engine.WaterBody` / `engine.Buoyancy` runtime on it,
`engine-world-render` draws it (`world.water`), and the MCP `water_*` tools query it.

| Module | Contents |
| --- | --- |
| `gerstner` | `GerstnerWave` / `GerstnerSet`: up to 16 waves, steepness sum capped at 0.9 (no loops), displacement, Jacobian, normals, orbital velocity, `angular_frequency` (`w^2 = g k tanh(k h)`) |
| `spectrum` | `OceanSettings`: Phillips and JONSWAP directional spectra, continuum variance, peak |
| `ocean` | `OceanSpectrum` (seeded `h0`, dispersion, loop period) and `OceanFrame` (4 packed inverse FFTs: height, choppy displacement, slopes, Jacobian; periodic bilinear sampling and height queries), `WaveStats` |
| `fft` | radix-2 complex FFT, 1D and 2D, plus a reference DFT |
| `displace` | bounded fixed-point inversion of horizontal displacement (tolerance 1e-4 m, at most 16 iterations) |
| `river` | `RiverSettings` / `RiverSpline`: Catmull-Rom centreline, width/depth, constant-discharge flow field, ribbon mesh |
| `surface` | `WaterSurface` (ocean, lake, river), `WaterOptics`, `Underwater` fog, `ShorelineSettings` over a `GroundHeight` sampler |
| `buoyancy` | point-sampled buoyancy, linear/angular/flow drag impulses (unconditionally stable drag) |

```rust
use engine_water::*;
use glam::Vec2;

let waves = GerstnerSet::deep(&[GerstnerWave { wavelength: 12.0, amplitude: 0.4, ..Default::default() }])?;
let (point, inversion) = waves.sample(Vec2::new(3.0, 4.0), /* t */ 1.5, /* amplitude scale */ 1.0);
assert!(inversion.converged);
let height = point.offset.y;

let ocean = OceanSpectrum::new(&OceanSettings::default(), GRAVITY)?;
let frame = ocean.frame(1.5)?;
let (surface, _) = frame.sample(Vec2::new(10.0, 20.0));
println!("Hs = {:.2} m, height {:.2}", ocean.stats().significant_height, surface.offset.y);
# Ok::<(), engine_water::Error>(())
```

Tests (`cargo test -p engine-water`): FFT round trip and DFT agreement, dispersion relation
and crest speed, steepness bound, inverse-displacement error, Parseval and spectrum energy,
FFT vs direct synthesis, tile periodicity, determinism, mode phase evolution, loop period,
river flow continuity, shoreline attenuation, underwater fog and buoyancy balance. See
[docs/world-water.md](../../docs/world-water.md).
