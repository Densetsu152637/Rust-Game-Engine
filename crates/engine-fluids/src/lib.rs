//! Headless 3D particle fluids solved with Position Based Fluids (Macklin & Mueller 2013).
//!
//! Units are meters, seconds and kilograms. The crate depends only on `glam` and `std` and has
//! no ECS, renderer or rigid-body dependency:
//!
//! - [`FluidWorld`] owns fluid volumes keyed by stable `u64` ids (the shape an ECS adapter
//!   needs: create, update, step and read by id).
//! - [`FluidVolume`] holds one particle set with an explicit capacity, a box [`Domain`],
//!   emitters, kill volumes and the solver parameters ([`FluidParams`]).
//! - Rigid bodies take part through plain-data [`ColliderProxy`] values supplied on every step;
//!   the step reports the impulse the fluid applied to each proxy ([`ProxyImpulse`], sorted by
//!   id) so the host can apply buoyancy and drag (two-way coupling).
//! - Rendering consumes plain data: [`RenderOutput`] (positions, velocities, densities,
//!   colour-field normals, surface flags) and [`DensityGrid`] samples for a later
//!   marching-cubes surface.
//!
//! See `docs/physics-fluids.md` for the solver choice, parameters, stability ranges, measured
//! performance and limits.
//!
//! ```
//! use engine_fluids::{Domain, FluidParams, FluidWorld, Region, VolumeDesc};
//! use glam::Vec3;
//!
//! let mut world = FluidWorld::new();
//! let volume = world.create_volume(1, VolumeDesc {
//!     params: FluidParams::default(),
//!     domain: Domain::new(Vec3::ZERO, Vec3::new(1.0, 1.0, 0.3)),
//!     capacity: 4096,
//! })?;
//! let region = Region::Box {
//!     center: Vec3::new(0.2, 0.25, 0.15),
//!     half_extents: Vec3::new(0.2, 0.25, 0.15),
//! };
//! assert_eq!(volume.fill(region, Vec3::ZERO, None)?, 8 * 10 * 6);
//! let report = world.step(1.0 / 60.0, &[])?;
//! assert_eq!(report.volumes[0].1.particle_count, 480);
//! # Ok::<(), engine_fluids::Error>(())
//! ```

mod emitter;
mod grid;
mod kernel;
mod output;
mod params;
mod proxy;
mod volume;
mod world;

pub use emitter::{EmitterDesc, EmitterKind, Region};
pub use output::{DensityGrid, FluidStats, RenderOutput};
pub use params::{Domain, FluidParams};
pub use proxy::{ColliderProxy, ProxyImpulse, ProxyShape};
pub use volume::{FluidVolume, StepReport, VolumeDesc};
pub use world::{FluidWorld, WorldStepReport};

/// Most particles one volume may hold (`VolumeDesc::capacity` upper bound).
pub const MAX_PARTICLES: usize = 1 << 20;
/// Most fluid volumes one [`FluidWorld`] may hold.
pub const MAX_VOLUMES: usize = 256;
/// Most collider proxies accepted by one step.
pub const MAX_PROXIES: usize = 256;
/// Most emitters per volume.
pub const MAX_EMITTERS: usize = 64;
/// Most kill volumes per volume.
pub const MAX_KILL_VOLUMES: usize = 64;
/// Upper bound for [`FluidParams::substeps`].
pub const MAX_SUBSTEPS: u32 = 16;
/// Upper bound for [`FluidParams::iterations`].
pub const MAX_ITERATIONS: u32 = 32;
/// Longest accepted step in seconds.
pub const MAX_STEP_SECONDS: f32 = 0.1;
/// Most neighbours kept per particle and substep; extra neighbours are dropped and counted in
/// [`StepReport::neighbor_overflow`].
pub const MAX_NEIGHBORS: usize = 128;
/// Most samples one [`DensityGrid`] may hold.
pub const MAX_GRID_SAMPLES: usize = 1 << 22;
/// Smallest and largest accepted particle radius in meters.
pub const PARTICLE_RADIUS_RANGE: (f32, f32) = (1.0e-3, 10.0);

/// Errors returned by the fluid API. Every rejected call leaves the volume unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// An input was non-finite, out of range or inconsistent; the text names it.
    Invalid(&'static str),
    /// An explicit spawn would exceed the volume capacity.
    CapacityExceeded { requested: usize, available: usize },
    /// No volume with this id exists.
    UnknownVolume(u64),
    /// A volume with this id already exists.
    DuplicateVolume(u64),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(what) => f.write_str(what),
            Self::CapacityExceeded {
                requested,
                available,
            } => write!(
                f,
                "fluid capacity exceeded: requested {requested}, available {available}"
            ),
            Self::UnknownVolume(id) => write!(f, "unknown fluid volume {id}"),
            Self::DuplicateVolume(id) => write!(f, "fluid volume {id} already exists"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
