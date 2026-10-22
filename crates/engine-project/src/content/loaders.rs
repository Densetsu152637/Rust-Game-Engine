use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}
pub fn load_mesh(path: &Path) -> Result<Mesh> {
    let bytes = read_bounded(path, 64 * 1024 * 1024)?;
    let source =
        std::str::from_utf8(&bytes).map_err(|e| Error::new("asset.mesh.invalid", e.to_string()))?;
    let mut tokens = source
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .flat_map(str::split_whitespace);
    if tokens.next() != Some("CGMESH") || tokens.next() != Some("1") {
        return Err(Error::new("asset.mesh.invalid", "Expected CGMESH 1"));
    }
    let mut mesh = Mesh::default();
    while let Some(token) = tokens.next() {
        if token != "vertex" || mesh.vertices.len() >= 1_000_000 {
            return Err(Error::new(
                "asset.mesh.invalid",
                "Expected bounded vertex list",
            ));
        }
        let mut n = [0.0_f32; 5];
        for v in &mut n {
            *v = tokens
                .next()
                .and_then(|v| v.parse().ok())
                .filter(|v: &f32| v.is_finite())
                .ok_or_else(|| {
                    Error::new("asset.mesh.invalid", "Vertex requires five finite numbers")
                })?;
        }
        mesh.vertices.push(Vertex {
            position: [n[0], n[1], n[2]],
            uv: [n[3], n[4]],
            color: [1.0; 4],
        });
    }
    if mesh.vertices.is_empty() || !mesh.vertices.len().is_multiple_of(3) {
        return Err(Error::new(
            "asset.mesh.invalid",
            "Mesh requires nonempty triangle list",
        ));
    }
    mesh.indices = (0..mesh.vertices.len() as u32).collect();
    Ok(mesh)
}
pub fn load_texture(path: &Path) -> Result<Texture> {
    let bytes = read_bounded(path, 128 * 1024 * 1024)?;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut reader =
            image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(256 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|e| Error::new("asset.texture.invalid", e.to_string()))?
            .into_rgba8();
        return Ok(Texture {
            width: image.width(),
            height: image.height(),
            pixels: image.into_raw(),
        });
    }
    let source = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new("asset.texture.invalid", "Expected PNG or PPM P3"))?;
    let mut tokens = source
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .flat_map(str::split_whitespace);
    if tokens.next() != Some("P3") {
        return Err(Error::new(
            "asset.texture.invalid",
            "Expected PNG or PPM P3",
        ));
    }
    let mut number = || {
        tokens
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| Error::new("asset.texture.invalid", "Expected PPM integer"))
    };
    let width = number()?;
    let height = number()?;
    let maximum = number()?;
    if width == 0 || height == 0 || width > 8192 || height > 8192 || maximum != 255 {
        return Err(Error::new(
            "asset.texture.invalid",
            "PPM requires bounded nonempty extent and max value 255",
        ));
    }
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for _ in 0..width * height {
        for _ in 0..3 {
            let v = number()?;
            if v > 255 {
                return Err(Error::new(
                    "asset.texture.invalid",
                    "PPM channel exceeds 255",
                ));
            }
            pixels.push(v as u8);
        }
        pixels.push(255);
    }
    if tokens.next().is_some() {
        return Err(Error::new("asset.texture.invalid", "Trailing PPM data"));
    }
    Ok(Texture {
        width,
        height,
        pixels,
    })
}
