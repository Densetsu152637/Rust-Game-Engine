use super::*;

/// Quad corners are left/bottom world units. Atlas rectangles use top-left pixels.
pub fn sprite_quad(
    position: [f32; 3],
    size: [f32; 2],
    pivot: [f32; 2],
    source: [u32; 4],
    texture: &Texture,
    tint: [f32; 4],
) -> Result<Mesh> {
    if texture.width == 0
        || texture.height == 0
        || source[2] == 0
        || source[3] == 0
        || source[0]
            .checked_add(source[2])
            .is_none_or(|n| n > texture.width)
        || source[1]
            .checked_add(source[3])
            .is_none_or(|n| n > texture.height)
    {
        return Err(Error::new(
            "asset.sprite.rectangle",
            "Sprite rectangle exceeds texture",
        ));
    }
    let x = position[0] - size[0] * pivot[0];
    let y = position[1] - size[1] * pivot[1];
    let z = position[2];
    let u0 = source[0] as f32 / texture.width as f32;
    let u1 = (source[0] + source[2]) as f32 / texture.width as f32;
    let v0 = source[1] as f32 / texture.height as f32;
    let v1 = (source[1] + source[3]) as f32 / texture.height as f32;
    Ok(Mesh {
        vertices: vec![
            Vertex {
                position: [x, y, z],
                uv: [u0, v1],
                color: tint,
            },
            Vertex {
                position: [x + size[0], y, z],
                uv: [u1, v1],
                color: tint,
            },
            Vertex {
                position: [x + size[0], y + size[1], z],
                uv: [u1, v0],
                color: tint,
            },
            Vertex {
                position: [x, y + size[1], z],
                uv: [u0, v0],
                color: tint,
            },
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    })
}
pub fn animation_frame(elapsed: f64, fps: f32, count: usize, looping: bool) -> usize {
    if !elapsed.is_finite() || elapsed < 0.0 || !fps.is_finite() || fps <= 0.0 || count == 0 {
        return 0;
    }
    let frame = (elapsed * fps as f64).floor();
    if looping {
        (frame % (count as f64)) as usize
    } else {
        frame.min((count - 1) as f64) as usize
    }
}
/// Logical-pixel text uses top-left origins. Word wrapping precedes glyph breaks.
pub fn text_mesh(
    font: &FontAtlas,
    texture: &Texture,
    text: &str,
    origin: [f32; 2],
    wrap_width: f32,
    scale: f32,
    color: [f32; 4],
) -> Result<Mesh> {
    if text.len() > 65536
        || !scale.is_finite()
        || scale <= 0.0
        || !wrap_width.is_finite()
        || wrap_width < 0.0
    {
        return Err(Error::new(
            "ui.text.invalid",
            "Invalid text geometry parameters",
        ));
    }
    let mut mesh = Mesh::default();
    let (mut x, mut y) = (origin[0], origin[1]);
    for word in text.split_inclusive(char::is_whitespace) {
        let width: f32 = word
            .chars()
            .take_while(|c| !c.is_whitespace())
            .map(|c| font.glyph(c).advance * scale)
            .sum();
        if wrap_width > 0.0 && width > 0.0 && x > origin[0] && x + width > origin[0] + wrap_width {
            x = origin[0];
            y += font.line_height * scale;
        }
        for c in word.chars() {
            if c == '\r' {
                continue;
            }
            if c == '\n' {
                x = origin[0];
                y += font.line_height * scale;
                continue;
            }
            let glyph = font.glyph(c);
            let advance = glyph.advance * scale;
            // Whitespace hangs past the wrap column; the following word wraps instead.
            if wrap_width > 0.0
                && !c.is_whitespace()
                && x > origin[0]
                && x + advance > origin[0] + wrap_width
            {
                x = origin[0];
                y += font.line_height * scale;
            }
            if glyph.source[2] > 0 && glyph.source[3] > 0 {
                // sprite_quad uses bottom-left geometry; invert Y for top-left UI.
                let mut quad = sprite_quad(
                    [
                        x + glyph.bearing[0] * scale,
                        -y - glyph.bearing[1] * scale,
                        0.0,
                    ],
                    [
                        glyph.source[2] as f32 * scale,
                        glyph.source[3] as f32 * scale,
                    ],
                    [0.0, 1.0],
                    glyph.source,
                    texture,
                    color,
                )?;
                for v in &mut quad.vertices {
                    v.position[1] = -v.position[1];
                }
                let base = mesh.vertices.len() as u32;
                mesh.vertices.extend(quad.vertices);
                mesh.indices
                    .extend(quad.indices.into_iter().map(|i| i + base));
            }
            x += advance;
        }
    }
    Ok(mesh)
}
