//! CPU-only loaders and geometry. Texture pixels are top-left RGBA8.
use crate::{Error, Result, schema::read_bounded};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
mod font;
mod geometry;
mod loaders;
mod viewport;

pub use font::*;
pub use geometry::*;
pub use loaders::*;
pub use viewport::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> FontAtlas {
        let glyph = |source, advance| FontGlyph {
            source,
            advance,
            bearing: [0.0; 2],
        };
        FontAtlas {
            texture: "t".into(),
            line_height: 5.0,
            fallback: 97,
            glyphs: BTreeMap::from([
                (97, glyph([0, 0, 2, 2], 3.0)),
                (32, glyph([0, 0, 0, 0], 2.0)),
            ]),
        }
    }

    fn texture() -> Texture {
        Texture {
            width: 4,
            height: 4,
            pixels: vec![255; 64],
        }
    }

    /// Top edge of every glyph quad, in layout order.
    fn tops(mesh: &Mesh) -> Vec<f32> {
        mesh.vertices
            .chunks(4)
            .map(|quad| quad[3].position[1])
            .collect()
    }

    #[test]
    fn overflowing_trailing_space_hangs_instead_of_creating_a_blank_line() {
        // "aa" is 6 wide; the space would end at 8 > 7 but must stay on the line.
        let mesh = text_mesh(&font(), &texture(), "aa aa", [0.0; 2], 7.0, 1.0, [1.0; 4]).unwrap();
        assert_eq!(tops(&mesh), vec![0.0, 0.0, 5.0, 5.0]);
    }

    #[test]
    fn spaces_that_fit_do_not_change_wrapping() {
        let mesh = text_mesh(&font(), &texture(), "aa aa", [0.0; 2], 9.0, 1.0, [1.0; 4]).unwrap();
        assert_eq!(tops(&mesh), vec![0.0, 0.0, 5.0, 5.0]);
    }

    #[test]
    fn explicit_newlines_still_start_a_new_line_after_an_overflowing_space() {
        let mesh = text_mesh(&font(), &texture(), "aa \naa", [0.0; 2], 7.0, 1.0, [1.0; 4]).unwrap();
        assert_eq!(tops(&mesh), vec![0.0, 0.0, 5.0, 5.0]);
    }
}
