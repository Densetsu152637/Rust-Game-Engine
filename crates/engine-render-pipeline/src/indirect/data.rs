use anyhow::{Result, ensure};
use glam::Vec3;

pub const MAX_HALF: f32 = 65_504.0;
pub const MAX_RADIANCE_EDGE: u32 = 2048;
pub const MAX_CUBE_SIZE: u32 = 256;
pub const MAX_PROBES: usize = 16;
pub const MAX_ENVIRONMENTS: usize = MAX_PROBES + 1;
pub const MAX_LIGHTMAPS: usize = 8;
/// Separate ceilings for decoded RGB32 data and resident RGBA16 textures.
pub const MAX_RADIANCE_BYTES: u64 = 128 * 1024 * 1024;

/// Linear, unexposed RGB radiance, top row first. Alpha is not represented.
/// Values must survive conversion to finite half floats; there is no clamping.
#[derive(Clone, Debug, PartialEq)]
pub struct RadianceImage {
    size: [u32; 2],
    pixels: Vec<[f32; 3]>,
}

impl RadianceImage {
    pub fn new(size: [u32; 2], pixels: Vec<[f32; 3]>) -> Result<Self> {
        ensure!(
            size.iter().all(|&n| n > 0 && n <= MAX_RADIANCE_EDGE),
            "indirect image dimensions must be 1..={MAX_RADIANCE_EDGE}"
        );
        let count = u64::from(size[0]) * u64::from(size[1]);
        ensure!(
            pixels.len() as u64 == count,
            "indirect image texel count does not match its dimensions"
        );
        ensure!(
            pixels
                .iter()
                .flatten()
                .all(|v| v.is_finite() && (0.0..=MAX_HALF).contains(v)),
            "indirect radiance must be finite, nonnegative and <= {MAX_HALF}"
        );
        Ok(Self { size, pixels })
    }

    pub fn size(&self) -> [u32; 2] {
        self.size
    }
    pub fn pixels(&self) -> &[[f32; 3]] {
        &self.pixels
    }
    pub fn decoded_bytes(&self) -> u64 {
        self.pixels.len() as u64 * 12
    }
    pub fn texture_bytes(&self) -> u64 {
        self.pixels.len() as u64 * 8
    }

    /// Explicit scale for callers that need exposed input data. GPU upload never
    /// applies an implicit camera exposure. Overflow is an error, not saturation.
    pub fn scaled(&self, scale: f32) -> Result<Self> {
        ensure!(
            scale.is_finite() && scale >= 0.0,
            "radiance scale must be finite and nonnegative"
        );
        Self::new(
            self.size,
            self.pixels.iter().map(|p| p.map(|v| v * scale)).collect(),
        )
    }
}

/// Preconvolved incident diffuse radiance (irradiance / pi) in the exact basis
/// [1, x, y, z, xy, yz, 3z*z-1, xz, x*x-y*y]. Normalization is in the coefficients.
/// Negative coefficients are valid. Coordinates are the environment's local axes.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffusePolynomial([[f32; 3]; 9]);

impl DiffusePolynomial {
    pub fn new(coefficients: [[f32; 3]; 9]) -> Result<Self> {
        ensure!(
            coefficients
                .iter()
                .flatten()
                .all(|v| v.is_finite() && v.abs() <= MAX_HALF),
            "diffuse polynomial coefficients must be finite and within half-float magnitude"
        );
        Ok(Self(coefficients))
    }

    pub fn coefficients(&self) -> &[[f32; 3]; 9] {
        &self.0
    }

    /// Unit direction required; clamping applies to the reconstructed signal,
    /// never to its signed coefficients. Calculation stays in f32.
    pub fn evaluate(&self, direction: Vec3) -> Result<Vec3> {
        ensure!(
            direction.is_finite() && (direction.length_squared() - 1.0).abs() <= 1e-3,
            "diffuse polynomial requires a finite unit direction"
        );
        let Vec3 { x, y, z } = direction;
        let basis = [
            1.0,
            x,
            y,
            z,
            x * y,
            y * z,
            3.0 * z * z - 1.0,
            x * z,
            x * x - y * y,
        ];
        let result = self
            .0
            .iter()
            .zip(basis)
            .fold(Vec3::ZERO, |sum, (c, b)| sum + Vec3::from_array(*c) * b);
        Ok(result.max(Vec3::ZERO))
    }
}

/// Complete, externally GGX-prefiltered cube mip chain. Face order is
/// +X,-X,+Y,-Y,+Z,-Z; all six faces at a level have the same dimensions.
/// No convolution or scene baking is performed by this type or its uploader.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentData {
    diffuse: DiffusePolynomial,
    mips: Vec<[RadianceImage; 6]>,
}

impl EnvironmentData {
    pub fn new(diffuse: DiffusePolynomial, mips: Vec<[RadianceImage; 6]>) -> Result<Self> {
        let first = mips
            .first()
            .ok_or_else(|| anyhow::anyhow!("environment needs a cube mip chain"))?;
        let [edge, height] = first[0].size();
        ensure!(
            edge == height && edge.is_power_of_two() && edge <= MAX_CUBE_SIZE,
            "environment faces must be square power-of-two images <= {MAX_CUBE_SIZE}"
        );
        ensure!(
            mips.len() == edge.ilog2() as usize + 1,
            "environment needs every mip down to 1x1"
        );
        for (level, faces) in mips.iter().enumerate() {
            let expected = [edge >> level; 2];
            ensure!(
                faces.iter().all(|face| face.size() == expected),
                "environment mip {level} must contain six {expected:?} faces"
            );
        }
        let data = Self { diffuse, mips };
        ensure!(
            data.decoded_bytes() <= MAX_RADIANCE_BYTES,
            "environment decoded budget exceeded"
        );
        Ok(data)
    }

    pub fn diffuse(&self) -> &DiffusePolynomial {
        &self.diffuse
    }
    pub fn mips(&self) -> &[[RadianceImage; 6]] {
        &self.mips
    }
    pub fn edge(&self) -> u32 {
        self.mips[0][0].size()[0]
    }
    pub fn decoded_bytes(&self) -> u64 {
        self.mips
            .iter()
            .flatten()
            .map(RadianceImage::decoded_bytes)
            .sum()
    }
    pub fn texture_bytes(&self) -> u64 {
        self.mips
            .iter()
            .flatten()
            .map(RadianceImage::texture_bytes)
            .sum()
    }
}

/// Atomic planning budget for DISTINCT resources in a prepared set. Callers
/// sharing a resource count it once; this is not an allocator or a cache eviction
/// policy. Use GpuIndirectResources to enforce the aggregate upload gate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndirectBudget {
    environments: usize,
    lightmaps: usize,
    decoded_bytes: u64,
    texture_bytes: u64,
}

impl IndirectBudget {
    pub fn environments(&self) -> usize {
        self.environments
    }
    pub fn lightmaps(&self) -> usize {
        self.lightmaps
    }
    pub fn decoded_bytes(&self) -> u64 {
        self.decoded_bytes
    }
    pub fn texture_bytes(&self) -> u64 {
        self.texture_bytes
    }

    pub fn include_environment(&mut self, data: &EnvironmentData) -> Result<()> {
        let mut next = self.clone();
        next.environments += 1;
        ensure!(
            next.environments <= MAX_ENVIRONMENTS,
            "at most {MAX_ENVIRONMENTS} environments per indirect set"
        );
        next.add_bytes(data.decoded_bytes(), data.texture_bytes())?;
        *self = next;
        Ok(())
    }

    pub fn include_lightmap(&mut self, data: &RadianceImage) -> Result<()> {
        let mut next = self.clone();
        next.lightmaps += 1;
        ensure!(
            next.lightmaps <= MAX_LIGHTMAPS,
            "at most {MAX_LIGHTMAPS} lightmaps per indirect set"
        );
        next.add_bytes(data.decoded_bytes(), data.texture_bytes())?;
        *self = next;
        Ok(())
    }

    pub(crate) fn add_bytes(&mut self, decoded: u64, texture: u64) -> Result<()> {
        let decoded = self
            .decoded_bytes
            .checked_add(decoded)
            .ok_or_else(|| anyhow::anyhow!("decoded indirect size overflow"))?;
        let texture = self
            .texture_bytes
            .checked_add(texture)
            .ok_or_else(|| anyhow::anyhow!("GPU indirect size overflow"))?;
        ensure!(
            decoded <= MAX_RADIANCE_BYTES && texture <= MAX_RADIANCE_BYTES,
            "indirect decoded/GPU radiance budget exceeds {MAX_RADIANCE_BYTES} bytes"
        );
        self.decoded_bytes = decoded;
        self.texture_bytes = texture;
        Ok(())
    }
}
