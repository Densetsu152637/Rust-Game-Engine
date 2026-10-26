//! Rule tiles (neighbour-matching autotiling in the Unity RuleTile spirit) and the built-in
//! 47-tile blob and 16-tile four-neighbour presets.
use crate::error::{Error, Result};
use crate::orient::{self, FLIP_X, FLIP_Y, ROTATIONS};
use crate::tileset::{MAX_RULES, TileId};
use serde::{Deserialize, Serialize};

/// Neighbour offsets in rule order: NW, N, NE, W, E, SW, S, SE (y up).
pub const OFFSETS: [[i32; 2]; 8] = [
    [-1, 1],
    [0, 1],
    [1, 1],
    [-1, 0],
    [1, 0],
    [-1, -1],
    [0, -1],
    [1, -1],
];
const NW: usize = 0;
const N: usize = 1;
const NE: usize = 2;
const W: usize = 3;
const E: usize = 4;
const SW: usize = 5;
const S: usize = 6;
const SE: usize = 7;

/// What a rule demands of one neighbour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Neighbour {
    /// No requirement.
    Any,
    /// The neighbour is this rule tile (or a tile it connects to).
    This,
    /// The neighbour is empty or a different tile.
    NotThis,
}

/// How a rule is expanded into oriented variants. Variants are tried in the listed order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleTransform {
    /// Only the authored orientation.
    #[default]
    None,
    /// 0, 90, 180 and 270 degrees clockwise.
    Rotated,
    /// Authored and mirrored left-right.
    MirrorX,
    /// Authored and mirrored top-bottom.
    MirrorY,
    /// Authored, mirrored left-right, top-bottom and both.
    MirrorXy,
}
impl RuleTransform {
    fn variants(self) -> &'static [u8] {
        const NONE: [u8; 1] = [0];
        const MX: [u8; 2] = [0, FLIP_X];
        const MY: [u8; 2] = [0, FLIP_Y];
        const MXY: [u8; 4] = [0, FLIP_X, FLIP_Y, FLIP_X | FLIP_Y];
        match self {
            RuleTransform::None => &NONE,
            RuleTransform::Rotated => &ROTATIONS,
            RuleTransform::MirrorX => &MX,
            RuleTransform::MirrorY => &MY,
            RuleTransform::MirrorXy => &MXY,
        }
    }
}

/// One matching rule: eight neighbour requirements, the tile to show and its expansion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Requirements in [`OFFSETS`] order (NW, N, NE, W, E, SW, S, SE).
    pub neighbours: [Neighbour; 8],
    /// Concrete tile to display when the rule matches.
    pub output: TileId,
    /// Rotation/mirror expansion.
    #[serde(default)]
    pub transform: RuleTransform,
}

/// A logical tile that displays one of several concrete tiles depending on its neighbours.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleTile {
    /// Unique id placed in maps by painters.
    pub id: TileId,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Concrete tile shown when no rule matches.
    pub default_tile: TileId,
    /// Other tile ids that count as "this" for neighbour tests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connects: Vec<TileId>,
    /// Rules in priority order: the first match wins.
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledRule {
    neighbours: [Neighbour; 8],
    pub output: TileId,
    pub flags: u8,
}
impl CompiledRule {
    pub fn matches(&self, same: &[bool; 8]) -> bool {
        self.neighbours
            .iter()
            .zip(same)
            .all(|(rule, same)| match rule {
                Neighbour::Any => true,
                Neighbour::This => *same,
                Neighbour::NotThis => !*same,
            })
    }
}
impl RuleTile {
    /// An empty rule tile showing `default_tile` when nothing matches.
    pub fn new(id: TileId, name: &str, default_tile: TileId) -> Self {
        Self {
            id,
            name: name.into(),
            default_tile,
            connects: Vec::new(),
            rules: Vec::new(),
        }
    }
    /// Expands rules into oriented variants, dropping variants whose pattern an earlier variant
    /// of the same rule already covers.
    pub(crate) fn compile(
        &self,
        is_concrete: impl Fn(TileId) -> bool,
    ) -> Result<Vec<CompiledRule>> {
        let bad = |m: &str| Error::new("tileset.rule", format!("rule tile {}: {m}", self.id));
        if self.name.chars().count() > 64
            || self.connects.len() > 64
            || self.rules.len() > MAX_RULES
        {
            return Err(bad("name, connects or rules out of range"));
        }
        if !is_concrete(self.default_tile) {
            return Err(bad("default_tile must be a concrete tile"));
        }
        let mut out = Vec::new();
        for rule in &self.rules {
            if !is_concrete(rule.output) {
                return Err(bad("rule output must be a concrete tile"));
            }
            let mut seen: Vec<[Neighbour; 8]> = Vec::new();
            for &flags in rule.transform.variants() {
                let mut pattern = [Neighbour::Any; 8];
                for (k, offset) in OFFSETS.iter().enumerate() {
                    let target = orient::apply_int(flags, *offset);
                    let slot = OFFSETS
                        .iter()
                        .position(|o| *o == target)
                        .expect("orientation maps neighbours to neighbours");
                    pattern[slot] = rule.neighbours[k];
                }
                if seen.contains(&pattern) {
                    continue;
                }
                seen.push(pattern);
                out.push(CompiledRule {
                    neighbours: pattern,
                    output: rule.output,
                    flags,
                });
            }
        }
        Ok(out)
    }

    /// The 16-tile four-neighbour preset. The output for neighbour mask
    /// `N=1, E=2, S=4, W=8` is `first_tile + mask`; corners are ignored. `default_tile` is
    /// `first_tile` (the isolated tile).
    pub fn four_neighbour16(id: TileId, name: &str, first_tile: TileId) -> Self {
        let mut tile = Self::new(id, name, first_tile);
        for mask in 0u32..16 {
            let mut n = [Neighbour::Any; 8];
            for (slot, bit) in [(N, 1), (E, 2), (S, 4), (W, 8)] {
                n[slot] = if mask & bit != 0 {
                    Neighbour::This
                } else {
                    Neighbour::NotThis
                };
            }
            tile.rules.push(Rule {
                neighbours: n,
                output: first_tile + mask,
                transform: RuleTransform::None,
            });
        }
        tile
    }

    /// The 47-tile blob preset. A neighbour mask has `N=1, NE=2, E=4, SE=8, S=16, SW=32, W=64,
    /// NW=128`; a corner only counts when both adjacent edges are present. The 47 distinct
    /// reduced masks, sorted ascending, map to `first_tile + index` (see [`blob47_masks`]).
    /// `default_tile` is `first_tile` (the isolated tile).
    pub fn blob47(id: TileId, name: &str, first_tile: TileId) -> Self {
        let mut tile = Self::new(id, name, first_tile);
        for (index, mask) in blob47_masks().into_iter().enumerate() {
            let has = |bit: u8| mask & bit != 0;
            let edge = |present: bool| {
                if present {
                    Neighbour::This
                } else {
                    Neighbour::NotThis
                }
            };
            let mut n = [Neighbour::Any; 8];
            n[N] = edge(has(1));
            n[E] = edge(has(4));
            n[S] = edge(has(16));
            n[W] = edge(has(64));
            for (slot, corner, a, b) in [
                (NE, 2, 1, 4),
                (SE, 8, 4, 16),
                (SW, 32, 16, 64),
                (NW, 128, 64, 1),
            ] {
                if has(a) && has(b) {
                    n[slot] = edge(has(corner));
                }
            }
            tile.rules.push(Rule {
                neighbours: n,
                output: first_tile + index as u32,
                transform: RuleTransform::None,
            });
        }
        tile
    }
}

/// Reduces a blob neighbour mask: corner bits survive only if both adjacent edge bits are set.
pub fn blob_reduce(mask: u8) -> u8 {
    let mut out = mask & (1 | 4 | 16 | 64);
    for (corner, a, b) in [(2, 1, 4), (8, 4, 16), (32, 16, 64), (128, 64, 1)] {
        if mask & a != 0 && mask & b != 0 && mask & corner != 0 {
            out |= corner;
        }
    }
    out
}
/// The 47 distinct reduced blob masks in ascending order.
pub fn blob47_masks() -> Vec<u8> {
    let mut masks: Vec<u8> = (0..=255u8).map(blob_reduce).collect();
    masks.sort_unstable();
    masks.dedup();
    masks
}
