//! Plain-data outputs for renderers, debugging and tests.

use glam::Vec3;

/// Owned per-particle data for a renderer (debug points first, a surface later). All vectors
/// have one entry per particle in the volume's particle order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderOutput {
    pub ids: Vec<u64>,
    pub positions: Vec<Vec3>,
    pub velocities: Vec<Vec3>,
    /// Density in kg/m^3 at the end of the step (including wall boundary density).
    pub densities: Vec<f32>,
    /// Outward colour-field normals (unit length, zero where the field is flat).
    pub normals: Vec<Vec3>,
    /// Particles whose scaled colour-field gradient exceeds `FluidParams::surface_threshold`.
    pub surface: Vec<bool>,
    pub particle_radius: f32,
    pub rest_density: f32,
}

/// Summary statistics of a volume at the end of the last step.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FluidStats {
    pub particle_count: usize,
    pub surface_count: usize,
    /// Mean density over all particles (kg/m^3).
    pub mean_density: f32,
    /// Mean density over particles not flagged as surface (kg/m^3); zero when there are none.
    pub mean_interior_density: f32,
    pub max_density: f32,
    /// Total kinetic energy in joules.
    pub kinetic_energy: f32,
    pub max_speed: f32,
}

/// Normalized density (`density / rest_density`, without wall boundary density) sampled on a
/// regular grid; the 0.5 iso-surface approximates the free surface for marching cubes.
#[derive(Clone, Debug, PartialEq)]
pub struct DensityGrid {
    /// Position of sample `(0, 0, 0)`.
    pub origin: Vec3,
    /// Distance between neighbouring samples per axis.
    pub cell_size: Vec3,
    /// Samples per axis (each at least 2).
    pub dims: [u32; 3],
    /// `dims[0] * dims[1] * dims[2]` values, x fastest, then y, then z.
    pub values: Vec<f32>,
}

impl DensityGrid {
    /// Value at integer sample coordinates.
    pub fn value(&self, x: u32, y: u32, z: u32) -> Option<f32> {
        let [nx, ny, nz] = self.dims;
        if x >= nx || y >= ny || z >= nz {
            return None;
        }
        let index = (z as usize * ny as usize + y as usize) * nx as usize + x as usize;
        self.values.get(index).copied()
    }
}
