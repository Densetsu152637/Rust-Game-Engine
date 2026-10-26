//! Query templates: the authored description of an environment query.
use crate::perception::AffiliationFilter;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Most items one query generates.
pub const MAX_ITEMS: usize = 512;
/// Most tests one template carries.
pub const MAX_TESTS: usize = 8;
/// Most items a query returns.
pub const MAX_RESULTS: u32 = 16;
/// Longest distance any bound, radius or extent may have.
pub const MAX_EXTENT: f32 = 100_000.0;

/// A point the query measures from or around.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Ref {
    /// `"querier"` or `"target"`.
    Named(Named),
    /// `{"key": "name"}`: a `vec3` blackboard key.
    Key { key: String },
    /// A fixed world position `[x, y, z]`.
    Point([f32; 3]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Named {
    /// The entity running the query.
    Querier,
    /// The position handed to the query (for example a perceived enemy).
    Target,
}

impl Default for Ref {
    fn default() -> Self {
        Self::Named(Named::Querier)
    }
}

impl Ref {
    pub(crate) fn needs_target(&self) -> bool {
        matches!(self, Self::Named(Named::Target))
    }
}

/// Where actor items come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorSource {
    /// Entities in the querier's perception memory (at their last known position).
    Perceived,
    /// Every entity with an `engine.PerceptionStimulus`.
    Stimuli,
}

fn one_ring() -> u32 {
    1
}
fn eight() -> u32 {
    8
}
fn sixty_four() -> u32 {
    64
}
fn yes() -> bool {
    true
}
fn one() -> f32 {
    1.0
}

/// How candidate items are produced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Generator {
    /// `count` points per ring on up to `rings` concentric circles (radius `radius`
    /// is the outermost), in the XZ plane at the center's height plus `height`.
    Circle {
        #[serde(default)]
        center: Ref,
        radius: f32,
        #[serde(default = "one_ring")]
        rings: u32,
        #[serde(default = "eight")]
        count: u32,
        #[serde(default)]
        height: f32,
    },
    /// A square grid of side `2 * half_extent`, `spacing` apart.
    Grid {
        #[serde(default)]
        center: Ref,
        half_extent: f32,
        spacing: f32,
        #[serde(default)]
        height: f32,
    },
    /// Actors, each an item at its position carrying its subject id.
    Actors {
        source: ActorSource,
        #[serde(default = "all_affiliations")]
        affiliations: AffiliationFilter,
        #[serde(default = "sixty_four")]
        max: u32,
    },
    /// Smart object slots that carry all `tags`, at their world positions.
    SmartObjects {
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default = "yes")]
        free_only: bool,
        #[serde(default = "sixty_four")]
        max: u32,
    },
}

fn all_affiliations() -> AffiliationFilter {
    AffiliationFilter::ALL
}

/// Which end of a measured value scores best.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    LowerBetter,
    HigherBetter,
}

/// The direction a dot test compares against.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Facing {
    /// `"forward"`: the querier's forward axis.
    Forward(ForwardTag),
    /// The direction from `from` toward this point.
    Toward(Ref),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardTag {
    Forward,
}

/// Blackboard comparison of a [`Test::Blackboard`] gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    IsSet,
    IsNotSet,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// One filter and/or score over the items.
///
/// `min` and `max` filter (an item outside is rejected); `score` ranks the survivors
/// by the measured value, min-max normalised over them, times `weight`. Tests run in
/// order and an item rejected by one skips the rest, so put cheap tests first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Test {
    /// Straight-line distance from the item to `to`.
    Distance {
        #[serde(default)]
        to: Ref,
        #[serde(default)]
        min: Option<f32>,
        #[serde(default)]
        max: Option<f32>,
        #[serde(default)]
        score: Option<Direction>,
        #[serde(default = "one")]
        weight: f32,
    },
    /// Dot product (-1..1) of the unit direction `from -> item` with `direction`.
    Dot {
        #[serde(default)]
        from: Ref,
        direction: Facing,
        #[serde(default)]
        min: Option<f32>,
        #[serde(default)]
        max: Option<f32>,
        #[serde(default)]
        score: Option<Direction>,
        #[serde(default = "one")]
        weight: f32,
    },
    /// Length of the navmesh path from `from` to the item; unreachable items are
    /// rejected. Needs a navmesh.
    PathLength {
        #[serde(default)]
        from: Ref,
        #[serde(default)]
        min: Option<f32>,
        #[serde(default)]
        max: Option<f32>,
        #[serde(default)]
        score: Option<Direction>,
        #[serde(default = "one")]
        weight: f32,
    },
    /// Keeps items reachable on the navmesh from `from` (`invert`: unreachable ones).
    Reachable {
        #[serde(default)]
        from: Ref,
        #[serde(default)]
        invert: bool,
    },
    /// Keeps items with a clear line from `from` (`invert`: blocked ones). Heights lift
    /// both ends above their positions.
    LineOfSight {
        #[serde(default)]
        from: Ref,
        #[serde(default)]
        from_height: f32,
        #[serde(default)]
        to_height: f32,
        #[serde(default)]
        invert: bool,
    },
    /// Gate on the querier's blackboard: when the comparison fails, no item passes.
    Blackboard {
        key: String,
        #[serde(default = "is_set")]
        operator: Compare,
        #[serde(default)]
        value: Option<Value>,
    },
}

fn is_set() -> Compare {
    Compare::IsSet
}

/// How many results to return.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Select {
    /// Best `top_n` items (1 selects the best one).
    pub top_n: u32,
    /// Items scoring below this are dropped (scores are 0..1).
    pub min_score: Option<f32>,
}

impl Default for Select {
    fn default() -> Self {
        Self {
            top_n: 1,
            min_score: None,
        }
    }
}

/// An environment query.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryTemplate {
    pub name: String,
    pub generator: Generator,
    #[serde(default)]
    pub tests: Vec<Test>,
    #[serde(default)]
    pub select: Select,
    /// Snap generated points onto the navmesh (points that cannot be snapped are
    /// dropped). Needs a navmesh.
    #[serde(default)]
    pub project_to_navmesh: bool,
    /// Farthest a point may move when snapped.
    #[serde(default = "default_snap")]
    pub snap_distance: f32,
    /// `navigation` asset for projection and path tests; without one the querier's
    /// `engine.NavigationAgent` navmesh is used.
    #[serde(default)]
    pub navmesh: Option<String>,
}

fn default_snap() -> f32 {
    2.0
}

fn finite(name: &str, value: f32, min: f32, max: f32) -> Result<(), String> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name} must be {min} to {max}"))
    }
}

fn check_ref(what: &str, reference: &Ref) -> Result<(), String> {
    match reference {
        Ref::Key { key } if key.is_empty() || key.len() > 64 => {
            Err(format!("{what}: key names are 1-64 characters"))
        }
        Ref::Point(p) if p.iter().any(|v| !v.is_finite() || v.abs() > 1.0e6) => {
            Err(format!("{what}: point must be finite"))
        }
        _ => Ok(()),
    }
}

fn check_bounds(what: &str, min: Option<f32>, max: Option<f32>) -> Result<(), String> {
    for (name, bound) in [("min", min), ("max", max)] {
        if let Some(v) = bound {
            finite(&format!("{what}.{name}"), v, -MAX_EXTENT, MAX_EXTENT)?;
        }
    }
    if let (Some(a), Some(b)) = (min, max)
        && a > b
    {
        return Err(format!("{what}: min is above max"));
    }
    Ok(())
}

impl Test {
    /// Whether the test needs the navmesh.
    pub fn needs_navmesh(&self) -> bool {
        matches!(self, Self::PathLength { .. } | Self::Reachable { .. })
    }
    fn refs(&self) -> Vec<&Ref> {
        match self {
            Self::Distance { to, .. } => vec![to],
            Self::Dot {
                from, direction, ..
            } => match direction {
                Facing::Toward(r) => vec![from, r],
                Facing::Forward(_) => vec![from],
            },
            Self::PathLength { from, .. }
            | Self::Reachable { from, .. }
            | Self::LineOfSight { from, .. } => vec![from],
            Self::Blackboard { .. } => vec![],
        }
    }
}

impl QueryTemplate {
    /// Whether the template reads a `target` position.
    pub fn needs_target(&self) -> bool {
        let generator = match &self.generator {
            Generator::Circle { center, .. } | Generator::Grid { center, .. } => {
                center.needs_target()
            }
            _ => false,
        };
        generator
            || self
                .tests
                .iter()
                .any(|t| t.refs().iter().any(|r| r.needs_target()))
    }

    /// Whether projection or a test needs a navmesh.
    pub fn needs_navmesh(&self) -> bool {
        self.project_to_navmesh || self.tests.iter().any(Test::needs_navmesh)
    }

    /// Checks names and bounds.
    ///
    /// # Errors
    /// The first violated rule, with a message naming the field.
    pub fn validate(&self) -> Result<(), String> {
        let name = &self.name;
        if name.is_empty()
            || name.len() > 64
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err("query names are 1-64 characters of letters, digits, '_', '-', '.'".into());
        }
        match &self.generator {
            Generator::Circle {
                center,
                radius,
                rings,
                count,
                height,
            } => {
                check_ref("generator.center", center)?;
                finite("generator.radius", *radius, f32::MIN_POSITIVE, MAX_EXTENT)?;
                finite("generator.height", *height, -MAX_EXTENT, MAX_EXTENT)?;
                if *rings == 0 || *count == 0 || (*rings as usize) * (*count as usize) > MAX_ITEMS {
                    return Err(format!(
                        "generator: rings and count must be at least 1 with rings * count <= {MAX_ITEMS}"
                    ));
                }
            }
            Generator::Grid {
                center,
                half_extent,
                spacing,
                height,
            } => {
                check_ref("generator.center", center)?;
                finite("generator.half_extent", *half_extent, 0.0, MAX_EXTENT)?;
                finite("generator.spacing", *spacing, 1.0e-3, MAX_EXTENT)?;
                finite("generator.height", *height, -MAX_EXTENT, MAX_EXTENT)?;
                let side = (2.0 * half_extent / spacing).floor() as usize + 1;
                if side * side > MAX_ITEMS {
                    return Err(format!(
                        "generator: the grid has {} points, at most {MAX_ITEMS}",
                        side * side
                    ));
                }
            }
            Generator::Actors { max, .. } | Generator::SmartObjects { max, .. } => {
                if *max == 0 || *max as usize > MAX_ITEMS {
                    return Err(format!("generator.max must be 1 to {MAX_ITEMS}"));
                }
                if let Generator::SmartObjects { tags, .. } = &self.generator
                    && (tags.len() > 8 || tags.iter().any(|t| t.is_empty() || t.len() > 64))
                {
                    return Err("generator.tags: at most 8 tags of 1-64 characters".into());
                }
            }
        }
        if self.tests.len() > MAX_TESTS {
            return Err(format!("more than {MAX_TESTS} tests"));
        }
        for (index, test) in self.tests.iter().enumerate() {
            let what = format!("tests[{index}]");
            for reference in test.refs() {
                check_ref(&what, reference)?;
            }
            let (min, max, score, weight) = match test {
                Test::Distance {
                    min,
                    max,
                    score,
                    weight,
                    ..
                }
                | Test::PathLength {
                    min,
                    max,
                    score,
                    weight,
                    ..
                } => {
                    if min.is_none() && max.is_none() && score.is_none() {
                        return Err(format!("{what}: set min, max or score, or it does nothing"));
                    }
                    (*min, *max, *score, *weight)
                }
                Test::Dot {
                    min,
                    max,
                    score,
                    weight,
                    ..
                } => {
                    if min.is_none() && max.is_none() && score.is_none() {
                        return Err(format!("{what}: set min, max or score, or it does nothing"));
                    }
                    for bound in [min, max].into_iter().flatten() {
                        finite(&format!("{what} bound"), *bound, -1.0, 1.0)?;
                    }
                    (*min, *max, *score, *weight)
                }
                Test::LineOfSight {
                    from_height,
                    to_height,
                    ..
                } => {
                    finite(
                        &format!("{what}.from_height"),
                        *from_height,
                        -1000.0,
                        1000.0,
                    )?;
                    finite(&format!("{what}.to_height"), *to_height, -1000.0, 1000.0)?;
                    continue;
                }
                Test::Reachable { .. } => continue,
                Test::Blackboard {
                    key,
                    operator,
                    value,
                } => {
                    if key.is_empty() || key.len() > 64 {
                        return Err(format!("{what}: key names are 1-64 characters"));
                    }
                    let compares = !matches!(operator, Compare::IsSet | Compare::IsNotSet);
                    if compares && value.is_none() {
                        return Err(format!("{what}: this operator compares against a value"));
                    }
                    continue;
                }
            };
            check_bounds(&what, min, max)?;
            if score.is_some() {
                finite(&format!("{what}.weight"), weight, 0.0, 1000.0)?;
            }
        }
        if self.select.top_n == 0 || self.select.top_n > MAX_RESULTS {
            return Err(format!("select.top_n must be 1 to {MAX_RESULTS}"));
        }
        if let Some(min) = self.select.min_score {
            finite("select.min_score", min, 0.0, 1.0)?;
        }
        finite("snap_distance", self.snap_distance, 0.0, 100.0)?;
        if let Some(navmesh) = &self.navmesh
            && (navmesh.is_empty() || navmesh.len() > 128)
        {
            return Err("navmesh is an asset id of 1-128 characters".into());
        }
        Ok(())
    }
}
