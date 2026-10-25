//! A small deterministic CPU rasterizer for UI [`Draw`] lists, used for
//! headless previews (`docs/ui-authoring.md`).
//!
//! It follows the engine's 2D pipeline: draws sort by `(layer, id)`, triangles
//! fill by pixel centre with the top-left rule, vertex colour multiplies the
//! sampled texel (white without a texture), and the result blends with the
//! GPU `ALPHA_BLENDING` equations (`rgb = src.rgb * src.a + dst.rgb * (1 -
//! src.a)`, `a = src.a + dst.a * (1 - src.a)`) on 8-bit channels, with no
//! colour-space conversion. SDF text, post effects, world-space draws and the
//! UI render features are not part of this path.
use crate::{
    Error, Result,
    content::{Mesh, Texture},
    runtime::{Draw, DrawSpace},
};

/// Largest image side in pixels.
pub const MAX_RASTER_DIMENSION: u32 = 4096;
/// Draws accepted per frame.
pub const MAX_RASTER_DRAWS: usize = 65_536;
/// Triangles accepted per frame.
pub const MAX_RASTER_TRIANGLES: usize = 1_000_000;
/// Pixel visits (summed triangle bounding boxes) accepted per frame.
pub const MAX_RASTER_FILL: u64 = 536_870_912;

/// Texture filter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiSampling {
    /// What the engine's GPU path uses.
    #[default]
    Nearest,
    Bilinear,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterOptions {
    /// Image size in pixels.
    pub size: [u32; 2],
    /// Multiplies draw coordinates (logical pixels to image pixels).
    pub scale: f32,
    /// Straight RGBA the image starts as.
    pub clear: [f32; 4],
    pub sampling: UiSampling,
    /// Pixel visits allowed (capped at [`MAX_RASTER_FILL`]).
    pub max_fill: u64,
}

impl RasterOptions {
    pub fn new(size: [u32; 2]) -> Self {
        Self {
            size,
            scale: 1.0,
            clear: [0.0, 0.0, 0.0, 1.0],
            sampling: UiSampling::Nearest,
            max_fill: MAX_RASTER_FILL,
        }
    }
}

/// RGBA8 image, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RasterImage {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[at],
            self.pixels[at + 1],
            self.pixels[at + 2],
            self.pixels[at + 3],
        ]
    }

    /// PNG bytes (8-bit RGBA).
    pub fn to_png(&self) -> Result<Vec<u8>> {
        use image::ImageEncoder;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &self.pixels,
                self.width,
                self.height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| Error::new("ui.preview.encode", e.to_string()))?;
        Ok(bytes)
    }

    /// Hex BLAKE3 digest of the pixels, for determinism checks.
    pub fn digest(&self) -> String {
        blake3::hash(&self.pixels).to_hex().to_string()
    }
}

fn too_complex(message: impl Into<String>) -> Error {
    Error::new("ui.preview.too_complex", message)
}

fn invalid_mesh(message: impl Into<String>) -> Error {
    Error::new("ui.preview.invalid_mesh", message)
}

/// Rasterize the screen-space ([`DrawSpace::Ui`]) draws of `draws`.
pub fn rasterize(draws: &[Draw], options: &RasterOptions) -> Result<RasterImage> {
    let [width, height] = options.size;
    if width == 0
        || height == 0
        || width > MAX_RASTER_DIMENSION
        || height > MAX_RASTER_DIMENSION
        || !options.scale.is_finite()
        || options.scale <= 0.0
        || options.clear.iter().any(|c| !c.is_finite())
    {
        return Err(Error::new(
            "ui.preview.size",
            format!(
                "Image sides are 1 to {MAX_RASTER_DIMENSION} pixels with a positive finite scale"
            ),
        ));
    }
    if draws.len() > MAX_RASTER_DRAWS {
        return Err(too_complex(format!("At most {MAX_RASTER_DRAWS} draws")));
    }
    let mut order: Vec<&Draw> = draws.iter().filter(|d| d.space == DrawSpace::Ui).collect();
    order.sort_by(|a, b| a.layer.cmp(&b.layer).then_with(|| a.id.cmp(&b.id)));
    let triangles: usize = order.iter().map(|d| d.mesh.indices.len() / 3).sum();
    if triangles > MAX_RASTER_TRIANGLES {
        return Err(too_complex(format!(
            "At most {MAX_RASTER_TRIANGLES} triangles"
        )));
    }
    let mut frame = Frame {
        width,
        height,
        pixels: vec![0; (width as usize) * (height as usize) * 4],
    };
    let clear = options.clear.map(to_u8);
    for chunk in frame.pixels.as_chunks_mut::<4>().0 {
        *chunk = clear;
    }
    let mut budget = options.max_fill.min(MAX_RASTER_FILL);
    for draw in order {
        draw_mesh(&mut frame, draw, options, &mut budget)?;
    }
    Ok(RasterImage {
        width,
        height,
        pixels: frame.pixels,
    })
}

struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fn to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[derive(Clone, Copy)]
struct Corner {
    x: f64,
    y: f64,
    uv: [f32; 2],
    color: [f32; 4],
}

fn edge(a: &Corner, b: &Corner, x: f64, y: f64) -> f64 {
    (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x)
}

/// Top-left rule for a triangle with positive [`edge`] area (clockwise on
/// screen, y down): a top edge is horizontal and points right, a left edge
/// points up. Pixels centred exactly on such an edge belong to the triangle;
/// shared edges therefore draw exactly once.
fn owns_boundary(a: &Corner, b: &Corner) -> bool {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    (dy == 0.0 && dx > 0.0) || dy < 0.0
}

fn draw_mesh(
    frame: &mut Frame,
    draw: &Draw,
    options: &RasterOptions,
    budget: &mut u64,
) -> Result<()> {
    let mesh: &Mesh = &draw.mesh;
    if mesh.indices.is_empty() {
        return Ok(());
    }
    if !mesh.indices.len().is_multiple_of(3)
        || mesh
            .indices
            .iter()
            .any(|i| *i as usize >= mesh.vertices.len())
    {
        return Err(invalid_mesh(format!(
            "Draw `{}` has indices outside its vertices or a partial triangle",
            draw.id
        )));
    }
    let scale = f64::from(options.scale);
    // Pixel rectangle all writes of this draw stay inside.
    let (mut x0, mut y0, mut x1, mut y1) = (
        0.0_f64,
        0.0_f64,
        f64::from(frame.width),
        f64::from(frame.height),
    );
    if let Some([cx, cy, cw, ch]) = draw.clip {
        if ![cx, cy, cw, ch].iter().all(|v| v.is_finite()) {
            return Err(invalid_mesh(format!(
                "Draw `{}` has a non-finite clip",
                draw.id
            )));
        }
        x0 = x0.max(f64::from(cx) * scale);
        y0 = y0.max(f64::from(cy) * scale);
        x1 = x1.min(f64::from(cx + cw) * scale);
        y1 = y1.min(f64::from(cy + ch) * scale);
    }
    if x1 <= x0 || y1 <= y0 {
        return Ok(());
    }
    let corner = |index: u32| -> Result<Corner> {
        let v = &mesh.vertices[index as usize];
        if !v.position.iter().all(|p| p.is_finite()) || !v.uv.iter().all(|p| p.is_finite()) {
            return Err(invalid_mesh(format!(
                "Draw `{}` has a non-finite vertex",
                draw.id
            )));
        }
        Ok(Corner {
            x: f64::from(v.position[0]) * scale,
            y: f64::from(v.position[1]) * scale,
            uv: v.uv,
            color: v.color,
        })
    };
    let texture = draw.texture.as_deref();
    for triangle in mesh.indices.as_chunks::<3>().0 {
        let (a, mut b, mut c) = (
            corner(triangle[0])?,
            corner(triangle[1])?,
            corner(triangle[2])?,
        );
        let mut area = edge(&a, &b, c.x, c.y);
        if area == 0.0 {
            continue;
        }
        if area < 0.0 {
            std::mem::swap(&mut b, &mut c);
            area = -area;
        }
        // Pixel centres at (i + 0.5, j + 0.5) inside the bounding box and clip.
        let min_x = a.x.min(b.x).min(c.x).max(x0);
        let max_x = a.x.max(b.x).max(c.x).min(x1);
        let min_y = a.y.min(b.y).min(c.y).max(y0);
        let max_y = a.y.max(b.y).max(c.y).min(y1);
        let (ix0, ix1) = (
            (min_x - 0.5).ceil().max(0.0) as u32,
            ((max_x - 0.5).floor() + 1.0).max(0.0) as u32,
        );
        let (iy0, iy1) = (
            (min_y - 0.5).ceil().max(0.0) as u32,
            ((max_y - 0.5).floor() + 1.0).max(0.0) as u32,
        );
        let (ix1, iy1) = (ix1.min(frame.width), iy1.min(frame.height));
        if ix1 <= ix0 || iy1 <= iy0 {
            continue;
        }
        let visits = u64::from(ix1 - ix0) * u64::from(iy1 - iy0);
        if visits > *budget {
            return Err(too_complex("The draw list needs too many pixel fills"));
        }
        *budget -= visits;
        let own = [
            owns_boundary(&b, &c),
            owns_boundary(&c, &a),
            owns_boundary(&a, &b),
        ];
        for py in iy0..iy1 {
            let y = f64::from(py) + 0.5;
            for px in ix0..ix1 {
                let x = f64::from(px) + 0.5;
                let w = [edge(&b, &c, x, y), edge(&c, &a, x, y), edge(&a, &b, x, y)];
                let inside = w
                    .iter()
                    .zip(own)
                    .all(|(w, own)| *w > 0.0 || (*w == 0.0 && own));
                if !inside {
                    continue;
                }
                // Weights relative to `a`, so equal vertex values interpolate exactly.
                let (lb, lc) = (w[1] / area, w[2] / area);
                let mix = |f: &dyn Fn(&Corner) -> f32| -> f32 {
                    let (va, vb, vc) = (f64::from(f(&a)), f64::from(f(&b)), f64::from(f(&c)));
                    (va + (vb - va) * lb + (vc - va) * lc) as f32
                };
                let mut src = [
                    mix(&|v| v.color[0]),
                    mix(&|v| v.color[1]),
                    mix(&|v| v.color[2]),
                    mix(&|v| v.color[3]),
                ];
                if let Some(texture) = texture {
                    let uv = [mix(&|v| v.uv[0]), mix(&|v| v.uv[1])];
                    let texel = sample(texture, uv, options.sampling);
                    for (channel, texel) in src.iter_mut().zip(texel) {
                        *channel *= texel;
                    }
                }
                blend(frame, px, py, src);
            }
        }
    }
    Ok(())
}

fn texel(texture: &Texture, x: i64, y: i64) -> [f32; 4] {
    let x = x.clamp(0, i64::from(texture.width) - 1) as usize;
    let y = y.clamp(0, i64::from(texture.height) - 1) as usize;
    let at = (y * texture.width as usize + x) * 4;
    match texture.pixels.get(at..at + 4) {
        Some(p) => [
            f32::from(p[0]) / 255.0,
            f32::from(p[1]) / 255.0,
            f32::from(p[2]) / 255.0,
            f32::from(p[3]) / 255.0,
        ],
        None => [1.0; 4],
    }
}

fn sample(texture: &Texture, uv: [f32; 2], sampling: UiSampling) -> [f32; 4] {
    if texture.width == 0 || texture.height == 0 {
        return [1.0; 4];
    }
    let (w, h) = (texture.width as f32, texture.height as f32);
    match sampling {
        UiSampling::Nearest => texel(
            texture,
            (uv[0] * w).floor() as i64,
            (uv[1] * h).floor() as i64,
        ),
        UiSampling::Bilinear => {
            let (fx, fy) = (uv[0] * w - 0.5, uv[1] * h - 0.5);
            let (ix, iy) = (fx.floor(), fy.floor());
            let (tx, ty) = (fx - ix, fy - iy);
            let (ix, iy) = (ix as i64, iy as i64);
            let (a, b) = (texel(texture, ix, iy), texel(texture, ix + 1, iy));
            let (c, d) = (texel(texture, ix, iy + 1), texel(texture, ix + 1, iy + 1));
            let mut out = [0.0; 4];
            for k in 0..4 {
                let top = a[k] + (b[k] - a[k]) * tx;
                let bottom = c[k] + (d[k] - c[k]) * tx;
                out[k] = top + (bottom - top) * ty;
            }
            out
        }
    }
}

fn blend(frame: &mut Frame, x: u32, y: u32, src: [f32; 4]) {
    let at = ((y * frame.width + x) * 4) as usize;
    let alpha = src[3].clamp(0.0, 1.0);
    let target = &mut frame.pixels[at..at + 4];
    for (channel, src) in target.iter_mut().zip(src).take(3) {
        let dst = f32::from(*channel) / 255.0;
        *channel = to_u8(src.clamp(0.0, 1.0) * alpha + dst * (1.0 - alpha));
    }
    let dst = f32::from(target[3]) / 255.0;
    target[3] = to_u8(alpha + dst * (1.0 - alpha));
}

/// Pixel bounds of the non-clear pixels, as `[x0, y0, x1, y1]` (end exclusive).
pub fn painted_bounds(image: &RasterImage, clear: [u8; 4]) -> Option<[u32; 4]> {
    let mut bounds: Option<[u32; 4]> = None;
    for y in 0..image.height {
        for x in 0..image.width {
            if image.pixel(x, y) != clear {
                bounds = Some(match bounds {
                    None => [x, y, x + 1, y + 1],
                    Some([a, b, c, d]) => [a.min(x), b.min(y), c.max(x + 1), d.max(y + 1)],
                });
            }
        }
    }
    bounds
}
