//! Time-sliced execution of a [`QueryTemplate`].
//!
//! A [`QueryRun`] resolves the template's references once (querier, target and
//! blackboard keys become fixed points), then works through generation, per-item tests
//! and the final scoring in [`step`](QueryRun::step) calls that each spend at most a
//! given budget of **evaluation units** (a distance or dot test costs 1, a line of
//! sight 4, a navmesh projection or path 8), so a long query spreads over ticks.
//! Execution is deterministic: items keep generation order and ties in the final
//! ranking go to the earlier item.
use super::template::{
    ActorSource, Compare, Direction, Facing, Generator, Named, QueryTemplate, Ref, Test,
};
use crate::{
    blackboard::{BbValue, Blackboard},
    perception::{AffiliationFilter, Id},
};
use serde::Serialize;
use std::{cmp::Ordering, sync::Arc};

const COST_SIMPLE: usize = 1;
const COST_LINE: usize = 4;
const COST_NAVMESH: usize = 8;

/// A generated candidate before scoring.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Item {
    pub position: [f32; 3],
    /// The entity the item stands for (actors and smart objects).
    pub subject: Option<Id>,
    /// Smart object slot index, for slot items.
    pub slot: Option<u32>,
}

impl Item {
    pub fn at(position: [f32; 3]) -> Self {
        Self {
            position,
            subject: None,
            slot: None,
        }
    }
}

/// What a query needs from the game.
pub trait QueryHost {
    /// Snaps `position` onto the navmesh within `snap` distance; `None` when there is no
    /// navmesh or nothing near.
    fn project(&mut self, position: [f32; 3], snap: f32) -> Option<[f32; 3]>;
    /// Length of the navmesh path between two points; `None` when unreachable or when
    /// there is no navmesh.
    fn path_length(&mut self, from: [f32; 3], to: [f32; 3]) -> Option<f32>;
    /// Whether the segment is free of occluders.
    fn line_of_sight(&mut self, from: [f32; 3], to: [f32; 3]) -> bool;
    /// Actor items, at most `max`, ordered by subject id.
    fn actors(&mut self, source: ActorSource, filter: AffiliationFilter, max: usize) -> Vec<Item>;
    /// Smart object slot items with all `tags`, at most `max`, ordered by slot.
    fn smart_objects(&mut self, tags: &[String], free_only: bool, max: usize) -> Vec<Item>;
}

/// The situation a query runs in.
#[derive(Clone, Debug)]
pub struct QueryContext {
    pub querier: [f32; 3],
    /// World-space forward direction of the querier.
    pub forward: [f32; 3],
    pub target: Option<[f32; 3]>,
    pub blackboard: Blackboard,
}

/// A ranked result item.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Scored {
    pub item: Item,
    /// Weighted mean of the normalised score tests, 0..1 (0 without score tests).
    pub score: f32,
    /// Position in generation order.
    pub index: u32,
}

/// The outcome of a finished query.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QueryResult {
    /// Best first, at most `select.top_n`.
    pub items: Vec<Scored>,
    pub generated: usize,
    /// Items rejected by a filter, a gate or a failed projection.
    pub rejected: usize,
    /// Evaluation units spent.
    pub cost: usize,
    /// `step` calls it took.
    pub steps: u32,
}

/// Progress of a [`QueryRun`].
#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    Running,
    Done(QueryResult),
}

/// Why a query could not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryError {
    Invalid(String),
    /// The template reads `target` but the caller gave none.
    MissingTarget,
    /// A `{"key": ..}` reference names a key that is not a set `vec3`.
    MissingKey(String),
    /// The template needs a navmesh the host cannot supply (checked by the caller).
    MissingNavmesh,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::MissingTarget => f.write_str("the query reads `target` but none was given"),
            Self::MissingKey(key) => write!(f, "blackboard key {key:?} is not a set vec3"),
            Self::MissingNavmesh => f.write_str("the query needs a navmesh"),
        }
    }
}
impl std::error::Error for QueryError {}

#[derive(Clone, Debug)]
struct Candidate {
    item: Item,
    alive: bool,
    /// Raw value per score test, in template order of score tests.
    raw: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Generate,
    Project,
    Evaluate,
    Done,
}

/// Facing resolved to a unit vector at start (`None` for `forward`: querier forward).
#[derive(Clone, Copy, Debug)]
enum ResolvedFacing {
    Forward,
    Toward([f32; 3]),
}

#[derive(Clone, Debug)]
enum ResolvedTest {
    Distance {
        to: [f32; 3],
    },
    Dot {
        from: [f32; 3],
        facing: ResolvedFacing,
    },
    PathLength {
        from: [f32; 3],
    },
    Reachable {
        from: [f32; 3],
        invert: bool,
    },
    Line {
        from: [f32; 3],
        from_height: f32,
        to_height: f32,
        invert: bool,
    },
}

/// A query in progress.
#[derive(Debug)]
pub struct QueryRun {
    template: Arc<QueryTemplate>,
    center: [f32; 3],
    ctx: QueryContext,
    tests: Vec<ResolvedTest>,
    stage: Stage,
    items: Vec<Candidate>,
    cursor: usize,
    cost: usize,
    steps: u32,
    generated: usize,
    rejected: usize,
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn length(a: [f32; 3]) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn compare(op: Compare, current: Option<&BbValue>, value: Option<&BbValue>) -> bool {
    match op {
        Compare::IsSet => current.is_some(),
        Compare::IsNotSet => current.is_none(),
        op => {
            let (Some(left), Some(right)) = (current, value) else {
                return false;
            };
            match op {
                Compare::Eq => left.loosely_equals(right),
                Compare::Ne => !left.loosely_equals(right),
                Compare::Lt => left.partial_compare(right) == Some(Ordering::Less),
                Compare::Le => matches!(
                    left.partial_compare(right),
                    Some(Ordering::Less | Ordering::Equal)
                ),
                Compare::Gt => left.partial_compare(right) == Some(Ordering::Greater),
                _ => matches!(
                    left.partial_compare(right),
                    Some(Ordering::Greater | Ordering::Equal)
                ),
            }
        }
    }
}

impl QueryRun {
    /// Starts a query: validates the template, resolves references and evaluates the
    /// blackboard gates. A failed gate makes the run finish empty on its first step.
    ///
    /// # Errors
    /// An invalid template, a missing target, or a reference to an unset key.
    pub fn new(template: Arc<QueryTemplate>, ctx: QueryContext) -> Result<Self, QueryError> {
        template.validate().map_err(QueryError::Invalid)?;
        let resolve = |reference: &Ref| -> Result<[f32; 3], QueryError> {
            match reference {
                Ref::Named(Named::Querier) => Ok(ctx.querier),
                Ref::Named(Named::Target) => ctx.target.ok_or(QueryError::MissingTarget),
                Ref::Point(p) => Ok(*p),
                Ref::Key { key } => match ctx.blackboard.get(key) {
                    Some(BbValue::Vec3(p)) => Ok(*p),
                    _ => Err(QueryError::MissingKey(key.clone())),
                },
            }
        };
        let center = match &template.generator {
            Generator::Circle { center, .. } | Generator::Grid { center, .. } => resolve(center)?,
            _ => ctx.querier,
        };
        let mut tests = Vec::new();
        let mut gate_open = true;
        for test in &template.tests {
            match test {
                Test::Distance { to, .. } => {
                    tests.push(ResolvedTest::Distance { to: resolve(to)? })
                }
                Test::Dot {
                    from, direction, ..
                } => {
                    let from_point = resolve(from)?;
                    let facing = match direction {
                        Facing::Forward(_) => ResolvedFacing::Forward,
                        Facing::Toward(r) => {
                            let d = sub(resolve(r)?, from_point);
                            let len = length(d);
                            if len < 1e-6 {
                                ResolvedFacing::Toward([0.0; 3])
                            } else {
                                ResolvedFacing::Toward([d[0] / len, d[1] / len, d[2] / len])
                            }
                        }
                    };
                    tests.push(ResolvedTest::Dot {
                        from: from_point,
                        facing,
                    });
                }
                Test::PathLength { from, .. } => {
                    tests.push(ResolvedTest::PathLength {
                        from: resolve(from)?,
                    });
                }
                Test::Reachable { from, invert } => tests.push(ResolvedTest::Reachable {
                    from: resolve(from)?,
                    invert: *invert,
                }),
                Test::LineOfSight {
                    from,
                    from_height,
                    to_height,
                    invert,
                } => tests.push(ResolvedTest::Line {
                    from: resolve(from)?,
                    from_height: *from_height,
                    to_height: *to_height,
                    invert: *invert,
                }),
                Test::Blackboard {
                    key,
                    operator,
                    value,
                } => {
                    let expected = match (value, ctx.blackboard.key_type(key)) {
                        (Some(v), Some(ty)) => BbValue::from_json(ty, v),
                        _ => None,
                    };
                    gate_open &= compare(*operator, ctx.blackboard.get(key), expected.as_ref());
                }
            }
        }
        Ok(Self {
            template,
            center,
            ctx,
            tests,
            stage: if gate_open {
                Stage::Generate
            } else {
                Stage::Done
            },
            items: Vec::new(),
            cursor: 0,
            cost: 0,
            steps: 0,
            generated: 0,
            rejected: 0,
        })
    }

    pub fn template(&self) -> &QueryTemplate {
        &self.template
    }

    /// Evaluation units spent so far.
    pub fn cost(&self) -> usize {
        self.cost
    }

    /// Runs until `budget` evaluation units are spent (at least one item is always
    /// processed) or the query finishes.
    pub fn step(&mut self, budget: usize, host: &mut dyn QueryHost) -> Progress {
        self.steps += 1;
        let mut spent = 0usize;
        loop {
            match self.stage {
                Stage::Generate => {
                    self.generate(host);
                    spent += self.items.len().max(1);
                    self.generated = self.items.len();
                    self.stage = if self.template.project_to_navmesh && !self.items.is_empty() {
                        Stage::Project
                    } else {
                        Stage::Evaluate
                    };
                    self.cursor = 0;
                }
                Stage::Project => {
                    let snap = self.template.snap_distance;
                    while self.cursor < self.items.len() {
                        if spent >= budget && spent > 0 {
                            self.cost += spent;
                            return Progress::Running;
                        }
                        let candidate = &mut self.items[self.cursor];
                        match host.project(candidate.item.position, snap) {
                            Some(p) => candidate.item.position = p,
                            None => {
                                candidate.alive = false;
                                self.rejected += 1;
                            }
                        }
                        spent += COST_NAVMESH;
                        self.cursor += 1;
                    }
                    self.cursor = 0;
                    self.stage = Stage::Evaluate;
                }
                Stage::Evaluate => {
                    while self.cursor < self.items.len() {
                        if spent >= budget && spent > 0 {
                            self.cost += spent;
                            return Progress::Running;
                        }
                        spent += self.evaluate(self.cursor, host);
                        self.cursor += 1;
                    }
                    self.stage = Stage::Done;
                }
                Stage::Done => {
                    self.cost += spent;
                    return Progress::Done(self.finish());
                }
            }
        }
    }

    /// Runs to completion with an unlimited budget.
    pub fn run(&mut self, host: &mut dyn QueryHost) -> QueryResult {
        loop {
            if let Progress::Done(result) = self.step(usize::MAX, host) {
                return result;
            }
        }
    }

    fn generate(&mut self, host: &mut dyn QueryHost) {
        let center = self.center;
        let items: Vec<Item> = match &self.template.generator {
            Generator::Circle {
                radius,
                rings,
                count,
                height,
                ..
            } => {
                let mut out = Vec::new();
                for ring in 1..=*rings {
                    let r = radius * ring as f32 / *rings as f32;
                    for i in 0..*count {
                        let angle = std::f32::consts::TAU * i as f32 / *count as f32;
                        out.push(Item::at([
                            center[0] + r * angle.cos(),
                            center[1] + height,
                            center[2] + r * angle.sin(),
                        ]));
                    }
                }
                out
            }
            Generator::Grid {
                half_extent,
                spacing,
                height,
                ..
            } => {
                let side = (2.0 * half_extent / spacing).floor() as usize + 1;
                let mut out = Vec::new();
                for ix in 0..side {
                    for iz in 0..side {
                        out.push(Item::at([
                            center[0] - half_extent + ix as f32 * spacing,
                            center[1] + height,
                            center[2] - half_extent + iz as f32 * spacing,
                        ]));
                    }
                }
                out
            }
            Generator::Actors {
                source,
                affiliations,
                max,
            } => host.actors(*source, *affiliations, *max as usize),
            Generator::SmartObjects {
                tags,
                free_only,
                max,
            } => host.smart_objects(tags, *free_only, *max as usize),
        };
        self.items = items
            .into_iter()
            .take(super::template::MAX_ITEMS)
            .map(|item| Candidate {
                item,
                alive: true,
                raw: Vec::new(),
            })
            .collect();
    }

    /// Runs the tests on one item; returns the units spent.
    fn evaluate(&mut self, index: usize, host: &mut dyn QueryHost) -> usize {
        if !self.items[index].alive {
            return 0;
        }
        let position = self.items[index].item.position;
        let mut spent = 0;
        let mut raw = Vec::new();
        let mut resolved = self.tests.iter();
        let mut alive = true;
        for test in &self.template.tests {
            if matches!(test, Test::Blackboard { .. }) {
                continue;
            }
            let Some(resolved_test) = resolved.next() else {
                break;
            };
            let (value, cost, bounds, score) = match (test, resolved_test) {
                (
                    Test::Distance {
                        min, max, score, ..
                    },
                    ResolvedTest::Distance { to },
                ) => (
                    Some(length(sub(position, *to))),
                    COST_SIMPLE,
                    (*min, *max),
                    *score,
                ),
                (
                    Test::Dot {
                        min, max, score, ..
                    },
                    ResolvedTest::Dot { from, facing },
                ) => {
                    let d = sub(position, *from);
                    let len = length(d);
                    let unit = if len < 1e-6 {
                        [0.0; 3]
                    } else {
                        [d[0] / len, d[1] / len, d[2] / len]
                    };
                    let reference = match facing {
                        ResolvedFacing::Forward => {
                            let f = self.ctx.forward;
                            let fl = length(f);
                            if fl < 1e-6 {
                                [0.0; 3]
                            } else {
                                [f[0] / fl, f[1] / fl, f[2] / fl]
                            }
                        }
                        ResolvedFacing::Toward(v) => *v,
                    };
                    (
                        Some(dot(unit, reference)),
                        COST_SIMPLE,
                        (*min, *max),
                        *score,
                    )
                }
                (
                    Test::PathLength {
                        min, max, score, ..
                    },
                    ResolvedTest::PathLength { from },
                ) => (
                    host.path_length(*from, position),
                    COST_NAVMESH,
                    (*min, *max),
                    *score,
                ),
                (Test::Reachable { .. }, ResolvedTest::Reachable { from, invert }) => {
                    let reachable = host.path_length(*from, position).is_some();
                    spent += COST_NAVMESH;
                    if reachable == *invert {
                        alive = false;
                        break;
                    }
                    continue;
                }
                (
                    Test::LineOfSight { .. },
                    ResolvedTest::Line {
                        from,
                        from_height,
                        to_height,
                        invert,
                    },
                ) => {
                    let a = [from[0], from[1] + from_height, from[2]];
                    let b = [position[0], position[1] + to_height, position[2]];
                    let clear = host.line_of_sight(a, b);
                    spent += COST_LINE;
                    if clear == *invert {
                        alive = false;
                        break;
                    }
                    continue;
                }
                _ => continue,
            };
            spent += cost;
            let Some(value) = value else {
                alive = false;
                break;
            };
            if bounds.0.is_some_and(|min| value < min) || bounds.1.is_some_and(|max| value > max) {
                alive = false;
                break;
            }
            if score.is_some() {
                raw.push(value);
            }
        }
        let candidate = &mut self.items[index];
        candidate.alive = alive;
        candidate.raw = raw;
        if !alive {
            self.rejected += 1;
        }
        spent.max(COST_SIMPLE)
    }

    fn finish(&mut self) -> QueryResult {
        let survivors: Vec<usize> = (0..self.items.len())
            .filter(|i| self.items[*i].alive)
            .collect();
        // (direction, weight) of each score test, in the order `raw` was filled.
        let scoring: Vec<(Direction, f32)> = self
            .template
            .tests
            .iter()
            .filter_map(|t| match t {
                Test::Distance { score, weight, .. }
                | Test::Dot { score, weight, .. }
                | Test::PathLength { score, weight, .. } => score.map(|s| (s, *weight)),
                _ => None,
            })
            .collect();
        let total_weight: f32 = scoring.iter().map(|(_, w)| *w).sum();
        let mut ranges = Vec::with_capacity(scoring.len());
        for slot in 0..scoring.len() {
            let mut min = f32::INFINITY;
            let mut max = f32::NEG_INFINITY;
            for i in &survivors {
                let v = self.items[*i].raw[slot];
                min = min.min(v);
                max = max.max(v);
            }
            ranges.push((min, max));
        }
        let mut scored: Vec<Scored> = survivors
            .iter()
            .map(|&i| {
                let score = if scoring.is_empty() || total_weight <= 0.0 {
                    0.0
                } else {
                    let mut sum = 0.0;
                    for (slot, (direction, weight)) in scoring.iter().enumerate() {
                        let (min, max) = ranges[slot];
                        let v = self.items[i].raw[slot];
                        // Values equal up to float noise carry no preference.
                        let normal = if max - min <= 1.0e-5 * (1.0 + max.abs().max(min.abs())) {
                            1.0
                        } else {
                            let n = (v - min) / (max - min);
                            match direction {
                                Direction::HigherBetter => n,
                                Direction::LowerBetter => 1.0 - n,
                            }
                        };
                        sum += weight * normal;
                    }
                    (sum / total_weight).clamp(0.0, 1.0)
                };
                Scored {
                    item: self.items[i].item,
                    score,
                    index: i as u32,
                }
            })
            .collect();
        scored.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.index.cmp(&b.index)));
        if let Some(min) = self.template.select.min_score {
            scored.retain(|s| s.score >= min);
        }
        scored.truncate(self.template.select.top_n as usize);
        QueryResult {
            items: scored,
            generated: self.generated,
            rejected: self.rejected,
            cost: self.cost,
            steps: self.steps,
        }
    }
}
