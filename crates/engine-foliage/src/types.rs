//! Foliage type definitions: what one kind of plant looks like and where it may grow.
use crate::{Error, Result};
use engine_pcg::Surface;
use serde::{Deserialize, Serialize};

/// Most types one foliage set (and one asset) may define.
pub const MAX_TYPES: usize = 32;
/// Most lower-detail meshes (`lod_meshes`) one type may list.
pub const MAX_LOD_MESHES: usize = 4;
/// Largest density grid edge in cells.
pub const MAX_GRID_EDGE: u32 = 1024;
/// Largest density grid in cells.
pub const MAX_GRID_CELLS: usize = 1 << 20;
/// Largest density (instances per square metre).
pub const MAX_DENSITY: f32 = 64.0;

fn invalid(message: impl Into<String>) -> Error {
    Error::new("foliage.type.invalid", message)
}

fn id_ok(text: &str) -> bool {
    !text.is_empty() && text.len() <= 128
}

/// A camera-facing billboard drawn beyond the last mesh LOD.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Impostor {
    /// Texture asset id (RGBA; alpha below 0.5 is discarded).
    pub texture: String,
    /// Billboard width and height in metres at scale 1. The quad stands on the instance origin
    /// and turns about the vertical axis to face the viewer.
    pub size: [f32; 2],
}

/// Keeps instances of a type away from instances of an earlier type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeExclusion {
    /// Name of an earlier type in the same set.
    pub name: String,
    /// Horizontal radius in metres around each instance of that type.
    pub radius: f32,
}

/// An explicit density grid: `values` are row-major (`row * columns + column`, rows along
/// +Z), each in 0..=1, one per cell of `cell_size` metres starting at `origin` (x, z).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DensityGrid {
    pub origin: [f32; 2],
    pub cell_size: f32,
    pub columns: u32,
    pub rows: u32,
    pub values: Vec<f32>,
}

impl DensityGrid {
    /// Checks the dimensions and values.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::new("foliage.density.grid", m.to_string()));
        if self.columns == 0
            || self.rows == 0
            || self.columns > MAX_GRID_EDGE
            || self.rows > MAX_GRID_EDGE
        {
            return bad("grid columns and rows must be within 1..=1024");
        }
        let cells = self.columns as usize * self.rows as usize;
        if cells > MAX_GRID_CELLS || self.values.len() != cells {
            return bad("grid needs exactly columns * rows values (at most 1048576)");
        }
        if !(self.cell_size.is_finite() && self.cell_size > 0.0 && self.cell_size <= 100_000.0)
            || !self.origin.iter().all(|v| v.is_finite())
        {
            return bad("grid origin must be finite and cell_size within (0, 100000]");
        }
        if !self
            .values
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        {
            return bad("grid values must be within 0..=1");
        }
        Ok(())
    }

    /// Bilinear density at world `(x, z)`: cell centres carry the values; 0 outside the grid.
    pub fn sample(&self, x: f32, z: f32) -> f32 {
        let fx = (x - self.origin[0]) / self.cell_size - 0.5;
        let fz = (z - self.origin[1]) / self.cell_size - 0.5;
        let (cols, rows) = (self.columns as i64, self.rows as i64);
        if fx < -0.5 || fz < -0.5 || fx > cols as f32 - 0.5 || fz > rows as f32 - 0.5 {
            return 0.0;
        }
        let (x0, z0) = (fx.floor(), fz.floor());
        let (tx, tz) = (fx - x0, fz - z0);
        let at = |cx: i64, cz: i64| {
            let (cx, cz) = (cx.clamp(0, cols - 1), cz.clamp(0, rows - 1));
            self.values[(cz * cols + cx) as usize]
        };
        let (x0, z0) = (x0 as i64, z0 as i64);
        let top = at(x0, z0) * (1.0 - tx) + at(x0 + 1, z0) * tx;
        let bottom = at(x0, z0 + 1) * (1.0 - tx) + at(x0 + 1, z0 + 1) * tx;
        top * (1.0 - tz) + bottom * tz
    }
}

/// Where a type grows: a 0..=1 multiplier of its `density`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DensityMap {
    /// 1 everywhere.
    #[default]
    Uniform,
    /// The weight of a terrain paint (splat) layer, `Surface::layer_weight`.
    Layer {
        layer: String,
        /// Use `1 - weight`.
        #[serde(default)]
        invert: bool,
        /// Weights below this count as 0 and the rest is rescaled to 0..=1.
        #[serde(default)]
        threshold: f32,
    },
    /// An explicit grid.
    Grid(DensityGrid),
    /// A texture asset's red channel mapped onto the world rectangle `origin`, `size` (x, z).
    /// Hosts resolve it into [`DensityMap::Grid`] before scattering; scatter rejects it.
    Texture {
        asset: String,
        origin: [f32; 2],
        size: [f32; 2],
    },
}

impl DensityMap {
    /// Checks the variant's parameters.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Uniform => Ok(()),
            Self::Layer {
                layer, threshold, ..
            } => {
                if !id_ok(layer) {
                    return Err(Error::new(
                        "foliage.density.layer",
                        "layer names are 1..=128 bytes",
                    ));
                }
                if !(threshold.is_finite() && (0.0..1.0).contains(threshold)) {
                    return Err(Error::new(
                        "foliage.density.layer",
                        "layer threshold must be within 0..1",
                    ));
                }
                Ok(())
            }
            Self::Grid(grid) => grid.validate(),
            Self::Texture {
                asset,
                origin,
                size,
            } => {
                if !id_ok(asset)
                    || !origin.iter().all(|v| v.is_finite())
                    || !size
                        .iter()
                        .all(|v| v.is_finite() && *v > 0.0 && *v <= 100_000.0)
                {
                    return Err(Error::new(
                        "foliage.density.texture",
                        "texture density needs an asset id, a finite origin and a size in (0, 100000]",
                    ));
                }
                Ok(())
            }
        }
    }

    /// The multiplier at world `(x, z)`, 0..=1.
    pub fn sample(&self, surface: &dyn Surface, x: f32, z: f32) -> f32 {
        match self {
            Self::Uniform | Self::Texture { .. } => 1.0,
            Self::Layer {
                layer,
                invert,
                threshold,
            } => {
                let w = surface
                    .layer_weight(layer, x, z)
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
                let w = if *invert { 1.0 - w } else { w };
                if w <= *threshold {
                    0.0
                } else {
                    ((w - threshold) / (1.0 - threshold)).clamp(0.0, 1.0)
                }
            }
            Self::Grid(grid) => grid.sample(x, z),
        }
    }
}

fn default_range_90() -> [f32; 2] {
    [0.0, 90.0]
}
fn default_height() -> [f32; 2] {
    [-100_000.0, 100_000.0]
}
fn default_scale() -> [f32; 2] {
    [1.0, 1.0]
}
fn default_yaw() -> [f32; 2] {
    [0.0, 360.0]
}
fn default_tint() -> [f32; 4] {
    [1.0; 4]
}
fn default_density() -> f32 {
    1.0
}
fn default_wind() -> f32 {
    1.0
}

/// One kind of plant.
///
/// Placement limits (`slope_degrees`, `height`, `altitude`) are tested on the ground under
/// the candidate and are exact: an instance is never placed outside them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FoliageType {
    /// Unique name inside a set; assets and exclusions refer to it.
    pub name: String,
    /// Mesh asset id of the full-detail model (LOD 0).
    pub mesh: String,
    /// Mesh asset ids of lower-detail models, LOD 1 and up.
    pub lod_meshes: Vec<String>,
    /// Texture asset id of the meshes' albedo.
    pub texture: Option<String>,
    /// Linear RGBA multiplier of the albedo.
    pub tint: [f32; 4],
    /// Billboard drawn beyond the last mesh band.
    pub impostor: Option<Impostor>,
    /// Instances per square metre before the density map (0..=64).
    #[serde(default = "default_density")]
    pub density: f32,
    pub density_map: DensityMap,
    /// Uniform scale range (min, max).
    #[serde(default = "default_scale")]
    pub scale: [f32; 2],
    /// Yaw range in degrees (min, max).
    #[serde(default = "default_yaw")]
    pub yaw_degrees: [f32; 2],
    /// Largest random lean from vertical, degrees (0..=45).
    pub tilt_degrees: f32,
    /// Ground slope limits in degrees (angle between the ground normal and up).
    #[serde(default = "default_range_90")]
    pub slope_degrees: [f32; 2],
    /// Ground height limits (world Y).
    #[serde(default = "default_height")]
    pub height: [f32; 2],
    /// Ground height limits relative to the floor of the placement bounds (`bounds.min.y`).
    #[serde(default = "default_height")]
    pub altitude: [f32; 2],
    /// 0 keeps the instance upright, 1 turns its up axis onto the ground normal.
    pub align_to_normal: f32,
    /// Vertical offset of the instance origin from the ground (negative sinks it).
    pub ground_offset: f32,
    /// Horizontal distance below which two instances of this type never coexist.
    pub min_spacing: f32,
    /// Earlier types this one keeps clear of.
    pub excludes: Vec<TypeExclusion>,
    /// Distances at which LOD `i` hands over to LOD `i + 1`; one per entry of `lod_meshes`.
    pub lod_distances: Vec<f32>,
    /// Distance beyond which the impostor replaces the last mesh (needs `impostor`).
    pub impostor_distance: Option<f32>,
    /// Distance beyond which instances are not drawn; `None` uses the set's culling distance.
    pub cull_distance: Option<f32>,
    /// 0..=1: how strongly wind bends this type (0 = rigid, such as rocks).
    #[serde(default = "default_wind")]
    pub wind_weight: f32,
}

impl Default for FoliageType {
    fn default() -> Self {
        Self {
            name: String::new(),
            mesh: String::new(),
            lod_meshes: Vec::new(),
            texture: None,
            tint: default_tint(),
            impostor: None,
            density: default_density(),
            density_map: DensityMap::Uniform,
            scale: default_scale(),
            yaw_degrees: default_yaw(),
            tilt_degrees: 0.0,
            slope_degrees: default_range_90(),
            height: default_height(),
            altitude: default_height(),
            align_to_normal: 0.0,
            ground_offset: 0.0,
            min_spacing: 0.0,
            excludes: Vec::new(),
            lod_distances: Vec::new(),
            impostor_distance: None,
            cull_distance: None,
            wind_weight: default_wind(),
        }
    }
}

fn range_ok(r: [f32; 2], lo: f32, hi: f32) -> bool {
    r.iter().all(|v| v.is_finite() && (lo..=hi).contains(v)) && r[0] <= r[1]
}

impl FoliageType {
    /// A type with a name and a mesh and the defaults of every other field.
    pub fn new(name: impl Into<String>, mesh: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mesh: mesh.into(),
            ..Self::default()
        }
    }

    /// Number of mesh levels of detail (the base mesh plus `lod_meshes`).
    pub fn mesh_levels(&self) -> usize {
        1 + self.lod_meshes.len()
    }

    /// Checks every field; the error names the type.
    pub fn validate(&self) -> Result<()> {
        let n = &self.name;
        let bad = |m: &str| Err(invalid(format!("type `{n}`: {m}")));
        if self.name.is_empty()
            || self.name.len() > 64
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(invalid(
                "type names are 1..=64 bytes of [A-Za-z0-9_.-]".to_string(),
            ));
        }
        if !id_ok(&self.mesh) {
            return bad("mesh needs an asset id of 1..=128 bytes");
        }
        if self.lod_meshes.len() > MAX_LOD_MESHES || self.lod_meshes.iter().any(|m| !id_ok(m)) {
            return bad("at most 4 lod_meshes with asset ids of 1..=128 bytes");
        }
        if self.texture.as_deref().is_some_and(|t| !id_ok(t)) {
            return bad("texture ids are 1..=128 bytes");
        }
        if !self
            .tint
            .iter()
            .all(|v| v.is_finite() && (0.0..=8.0).contains(v))
        {
            return bad("tint components must be within 0..=8");
        }
        if let Some(i) = &self.impostor
            && (!id_ok(&i.texture)
                || !i
                    .size
                    .iter()
                    .all(|v| v.is_finite() && *v > 0.0 && *v <= 1000.0))
        {
            return bad("impostor needs a texture id and a size within (0, 1000]");
        }
        if !(self.density.is_finite() && (0.0..=MAX_DENSITY).contains(&self.density)) {
            return bad("density must be within 0..=64 per square metre");
        }
        self.density_map.validate()?;
        if !range_ok(self.scale, 0.001, 1000.0) {
            return bad("scale range must be ordered and within 0.001..=1000");
        }
        if !range_ok(self.yaw_degrees, -3600.0, 3600.0) {
            return bad("yaw_degrees must be an ordered range within -3600..=3600");
        }
        if !(self.tilt_degrees.is_finite() && (0.0..=45.0).contains(&self.tilt_degrees)) {
            return bad("tilt_degrees must be within 0..=45");
        }
        if !range_ok(self.slope_degrees, 0.0, 90.0) {
            return bad("slope_degrees must be an ordered range within 0..=90");
        }
        if !range_ok(self.height, -100_000.0, 100_000.0)
            || !range_ok(self.altitude, -200_000.0, 200_000.0)
        {
            return bad("height and altitude must be ordered ranges within +-100000");
        }
        if !(self.align_to_normal.is_finite() && (0.0..=1.0).contains(&self.align_to_normal)) {
            return bad("align_to_normal must be within 0..=1");
        }
        if !(self.ground_offset.is_finite() && self.ground_offset.abs() <= 1000.0) {
            return bad("ground_offset must be within +-1000");
        }
        if !(self.min_spacing.is_finite() && (0.0..=1000.0).contains(&self.min_spacing)) {
            return bad("min_spacing must be within 0..=1000");
        }
        if self.excludes.len() > MAX_TYPES
            || self.excludes.iter().any(|e| {
                !id_ok(&e.name)
                    || e.name == self.name
                    || !(e.radius.is_finite() && e.radius > 0.0 && e.radius <= 1000.0)
            })
        {
            return bad("excludes name other types and need a radius within (0, 1000]");
        }
        if self.lod_distances.len() != self.lod_meshes.len() {
            return bad("lod_distances needs exactly one distance per lod_mesh");
        }
        let mut last = 0.0f32;
        for d in &self.lod_distances {
            if !(d.is_finite() && *d > last && *d <= 1.0e6) {
                return bad("lod_distances must be positive and strictly increasing");
            }
            last = *d;
        }
        match (self.impostor.is_some(), self.impostor_distance) {
            (false, Some(_)) => return bad("impostor_distance needs an impostor"),
            (true, Some(d)) if !(d.is_finite() && d > last && d <= 1.0e6) => {
                return bad("impostor_distance must exceed the last lod distance");
            }
            _ => {}
        }
        let visible_edge = self.impostor_distance.unwrap_or(last);
        if let Some(c) = self.cull_distance
            && !(c.is_finite() && c > visible_edge && c <= 1.0e6)
        {
            return bad("cull_distance must exceed the last lod and impostor distance");
        }
        if !(self.wind_weight.is_finite() && (0.0..=1.0).contains(&self.wind_weight)) {
            return bad("wind_weight must be within 0..=1");
        }
        Ok(())
    }
}

/// Checks a whole set: each type, unique names, exclusions naming earlier types.
pub fn validate_types(types: &[FoliageType]) -> Result<()> {
    if types.len() > MAX_TYPES {
        return Err(Error::new(
            "foliage.type.capacity",
            format!("at most {MAX_TYPES} foliage types"),
        ));
    }
    for (index, t) in types.iter().enumerate() {
        t.validate()?;
        if types[..index].iter().any(|o| o.name == t.name) {
            return Err(invalid(format!("duplicate type name `{}`", t.name)));
        }
        for e in &t.excludes {
            if !types[..index].iter().any(|o| o.name == e.name) {
                return Err(Error::new(
                    "foliage.exclusion.order",
                    format!(
                        "type `{}` excludes `{}`, which must be listed before it",
                        t.name, e.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_type_with_a_mesh_is_valid_and_bounds_are_enforced() {
        let t = FoliageType::new("grass", "grass_mesh");
        t.validate().unwrap();
        for mutate in [
            (|t: &mut FoliageType| t.density = 65.0) as fn(&mut FoliageType),
            |t| t.scale = [2.0, 1.0],
            |t| t.slope_degrees = [10.0, 95.0],
            |t| t.lod_distances = vec![10.0],
            |t| t.impostor_distance = Some(30.0),
            |t| t.align_to_normal = 1.5,
            |t| t.name = "bad name".into(),
            |t| t.tilt_degrees = f32::NAN,
        ] {
            let mut bad = t.clone();
            mutate(&mut bad);
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn lod_distances_must_increase_and_cull_exceeds_them() {
        let mut t = FoliageType::new("tree", "tree0");
        t.lod_meshes = vec!["tree1".into(), "tree2".into()];
        t.lod_distances = vec![20.0, 50.0];
        t.impostor = Some(Impostor {
            texture: "tree_card".into(),
            size: [4.0, 6.0],
        });
        t.impostor_distance = Some(120.0);
        t.cull_distance = Some(400.0);
        t.validate().unwrap();
        t.lod_distances = vec![50.0, 20.0];
        assert!(t.validate().is_err());
        t.lod_distances = vec![20.0, 50.0];
        t.cull_distance = Some(100.0);
        assert!(t.validate().is_err());
    }

    #[test]
    fn exclusions_must_name_earlier_types() {
        let tree = FoliageType::new("tree", "tree");
        let mut grass = FoliageType::new("grass", "grass");
        grass.excludes = vec![TypeExclusion {
            name: "tree".into(),
            radius: 2.0,
        }];
        validate_types(&[tree.clone(), grass.clone()]).unwrap();
        assert_eq!(
            validate_types(&[grass, tree]).unwrap_err().code,
            "foliage.exclusion.order"
        );
    }

    #[test]
    fn grid_density_is_bilinear_between_cell_centres_and_zero_outside() {
        let grid = DensityGrid {
            origin: [0.0, 0.0],
            cell_size: 2.0,
            columns: 2,
            rows: 1,
            values: vec![0.0, 1.0],
        };
        grid.validate().unwrap();
        // Cell centres at x = 1 (0.0) and x = 3 (1.0).
        assert!((grid.sample(1.0, 1.0) - 0.0).abs() < 1e-6);
        assert!((grid.sample(3.0, 1.0) - 1.0).abs() < 1e-6);
        assert!((grid.sample(2.0, 1.0) - 0.5).abs() < 1e-6);
        assert_eq!(grid.sample(-1.0, 1.0), 0.0);
        assert_eq!(grid.sample(2.0, 5.0), 0.0);
    }

    #[test]
    fn types_round_trip_through_json_and_reject_unknown_fields() {
        let mut t = FoliageType::new("bush", "bush");
        t.density_map = DensityMap::Layer {
            layer: "grass".into(),
            invert: false,
            threshold: 0.2,
        };
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<FoliageType>(&json).unwrap(), t);
        assert!(serde_json::from_str::<FoliageType>(r#"{"name":"a","bogus":1}"#).is_err());
    }
}
