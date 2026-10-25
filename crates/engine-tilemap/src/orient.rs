//! Tile orientation: the eight rotations/mirrors (the dihedral group D4) as three flag bits.
//!
//! Cell coordinates are y-up. A flagged tile is the authored art after, in order,
//! (1) transposing across the top-left/bottom-right diagonal, (2) mirroring horizontally if
//! `FLIP_X`, (3) mirroring vertically if `FLIP_Y`. A 90 degree clockwise rotation is
//! `TRANSPOSE | FLIP_X`, 180 degrees is `FLIP_X | FLIP_Y`, 270 degrees clockwise is
//! `TRANSPOSE | FLIP_Y`.

/// Mirror horizontally.
pub const FLIP_X: u8 = 1;
/// Mirror vertically.
pub const FLIP_Y: u8 = 2;
/// Transpose across the diagonal (applied first).
pub const TRANSPOSE: u8 = 4;
/// All valid flag bits.
pub const MASK: u8 = FLIP_X | FLIP_Y | TRANSPOSE;
/// Clockwise rotations by 0, 90, 180 and 270 degrees.
pub const ROTATIONS: [u8; 4] = [0, TRANSPOSE | FLIP_X, FLIP_X | FLIP_Y, TRANSPOSE | FLIP_Y];

/// Applies `flags` to an offset from the cell centre (y-up).
pub fn apply(flags: u8, mut offset: [f32; 2]) -> [f32; 2] {
    if flags & TRANSPOSE != 0 {
        offset = [-offset[1], -offset[0]];
    }
    if flags & FLIP_X != 0 {
        offset[0] = -offset[0];
    }
    if flags & FLIP_Y != 0 {
        offset[1] = -offset[1];
    }
    offset
}
/// Inverse of [`apply`]: maps a displayed offset back to the authored art.
pub fn invert(flags: u8, mut offset: [f32; 2]) -> [f32; 2] {
    if flags & FLIP_Y != 0 {
        offset[1] = -offset[1];
    }
    if flags & FLIP_X != 0 {
        offset[0] = -offset[0];
    }
    if flags & TRANSPOSE != 0 {
        offset = [-offset[1], -offset[0]];
    }
    offset
}
/// Applies `flags` to an integer neighbour offset.
pub fn apply_int(flags: u8, offset: [i32; 2]) -> [i32; 2] {
    let r = apply(flags, [offset[0] as f32, offset[1] as f32]);
    [r[0] as i32, r[1] as i32]
}
