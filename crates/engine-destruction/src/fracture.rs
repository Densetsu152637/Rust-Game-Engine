//! 3D Voronoi fracture of closed meshes.
use crate::decompose;
use crate::hull::convex_hull;
use crate::mass::mass_properties;
use crate::mesh::{Mesh, Prepared, Welded};
use crate::seeds;
use crate::{Error, Limits, Pattern, Result};
use glam::{DMat3, DVec3, Vec3};

/// Render mesh of one fragment. `interior[i]` marks triangle `i` as fracture surface (a cap
/// created by cutting) rather than original surface, for material assignment.
#[derive(Clone, Debug, PartialEq)]
pub struct FragmentMesh {
    pub positions: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
    pub interior: Vec<bool>,
}
/// One convex piece of a fractured mesh, in the coordinates of the input mesh.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    /// Index of the Voronoi seed that owns this piece.
    pub seed_index: usize,
    pub seed: Vec3,
    /// Vertices of the convex hull of the piece (the collider for physics).
    pub hull_points: Vec<Vec3>,
    pub mesh: FragmentMesh,
    /// Exact volume of the piece.
    pub volume: f32,
    /// Volume of the hull of `hull_points`; equals `volume` up to rounding because every
    /// fragment is convex.
    pub hull_volume: f32,
    pub centroid: Vec3,
    /// Inertia tensor at unit density about `centroid`, in mesh axes (row-major).
    pub inertia: [[f32; 3]; 3],
}
impl Fragment {
    /// Hull vertices relative to the centroid.
    pub fn local_hull_points(&self) -> Vec<Vec3> {
        self.hull_points
            .iter()
            .map(|p| *p - self.centroid)
            .collect()
    }
}
/// The pieces of a fractured mesh.
#[derive(Clone, Debug, PartialEq)]
pub struct Fracture {
    pub fragments: Vec<Fragment>,
    pub seeds: Vec<Vec3>,
    pub source_volume: f32,
    /// The input mesh is convex. Otherwise it was first split into this many convex parts
    /// (see `parts`), and each Voronoi cell yields one fragment per part it touches.
    pub convex_source: bool,
    /// Convex parts the input was split into (1 when `convex_source`).
    pub parts: usize,
}
impl Fracture {
    pub fn total_volume(&self) -> f32 {
        self.fragments.iter().map(|f| f.volume).sum()
    }
}
/// Fractures `mesh` with seeds generated from `pattern`. Deterministic for a given input.
pub fn fracture(mesh: &Mesh, pattern: &Pattern, limits: &Limits) -> Result<Fracture> {
    let prep = mesh.prepare(limits)?;
    let seeds = seeds::generate(&prep, pattern, limits)?;
    run(&prep, &seeds, limits)
}
/// The seed points `pattern` would use for `mesh`.
pub fn generate_seeds(mesh: &Mesh, pattern: &Pattern, limits: &Limits) -> Result<Vec<Vec3>> {
    let prep = mesh.prepare(limits)?;
    Ok(seeds::generate(&prep, pattern, limits)?
        .into_iter()
        .map(|s| s.as_vec3())
        .collect())
}
/// Fractures `mesh` along the Voronoi cells of explicit `seeds`.
pub fn fracture_with_seeds(mesh: &Mesh, seeds: &[Vec3], limits: &Limits) -> Result<Fracture> {
    if seeds.is_empty() || seeds.len() > limits.max_fragments {
        return Err(Error::InvalidPattern("seed count out of range"));
    }
    if seeds.iter().any(|s| !s.is_finite()) {
        return Err(Error::InvalidPattern("seed points must be finite"));
    }
    let prep = mesh.prepare(limits)?;
    let wide: Vec<DVec3> = seeds.iter().map(|s| s.as_dvec3()).collect();
    run(&prep, &wide, limits)
}
fn matrix(m: DMat3) -> [[f32; 3]; 3] {
    let row = |r: usize| [m.col(0)[r] as f32, m.col(1)[r] as f32, m.col(2)[r] as f32];
    [row(0), row(1), row(2)]
}
fn build(index: usize, seed: DVec3, piece: &Welded, source_volume: f64) -> Option<Fragment> {
    let props = mass_properties(&piece.verts, &piece.tris);
    if props.volume <= 1e-9 * source_volume {
        return None;
    }
    let hull = convex_hull(&piece.verts)?;
    let hull_props = mass_properties(&hull.points, &hull.tris);
    Some(Fragment {
        seed_index: index,
        seed: seed.as_vec3(),
        hull_points: hull.points.iter().map(|p| p.as_vec3()).collect(),
        mesh: FragmentMesh {
            positions: piece.verts.iter().map(|p| p.as_vec3()).collect(),
            triangles: piece.tris.clone(),
            interior: piece.interior.clone(),
        },
        volume: props.volume as f32,
        hull_volume: hull_props.volume as f32,
        centroid: props.centroid.as_vec3(),
        inertia: matrix(props.inertia),
    })
}
fn run(prep: &Prepared, seeds: &[DVec3], limits: &Limits) -> Result<Fracture> {
    let scale = prep.scale();
    let eps = 1e-10 * scale;
    let parts = decompose::pieces(prep)?;
    let mut fragments = Vec::new();
    let mut vertices = 0;
    for (index, &seed) in seeds.iter().enumerate() {
        let mut order: Vec<usize> = (0..seeds.len()).filter(|&j| j != index).collect();
        order.sort_by(|&a, &b| {
            (seeds[a] - seed)
                .length_squared()
                .total_cmp(&(seeds[b] - seed).length_squared())
                .then(a.cmp(&b))
        });
        for part in &parts {
            let mut cell = part.clone();
            let mut reach = cell.max_distance(seed);
            for &other in &order {
                let delta = seeds[other] - seed;
                let length = delta.length();
                if length <= 0. {
                    continue;
                }
                if length / 2. > reach + eps {
                    break;
                }
                let normal = delta / length;
                let offset =
                    (seeds[other].length_squared() - seed.length_squared()) / (2. * length);
                cell.clip(normal, offset, eps);
                if cell.is_empty() {
                    break;
                }
                reach = cell.max_distance(seed);
            }
            if cell.is_empty() {
                continue;
            }
            if let Some(fragment) = build(index, seed, &cell.welded(scale), prep.volume) {
                vertices += fragment.mesh.positions.len();
                fragments.push(fragment);
            }
            if fragments.len() > limits.max_fragments || vertices > limits.max_vertices {
                return Err(Error::LimitExceeded("fragment or vertex limit exceeded"));
            }
        }
    }
    Ok(Fracture {
        fragments,
        seeds: seeds.iter().map(|s| s.as_vec3()).collect(),
        source_volume: prep.volume as f32,
        convex_source: parts.len() == 1,
        parts: parts.len(),
    })
}
