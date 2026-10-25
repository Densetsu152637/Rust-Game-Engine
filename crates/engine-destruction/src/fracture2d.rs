//! 2D Voronoi fracture of convex polygons.
use crate::rng::Rng;
use crate::{Error, Limits, Result};
use glam::{DVec2, Vec2};

/// Seed pattern for [`fracture_polygon`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pattern2 {
    Uniform {
        count: usize,
        seed: u64,
    },
    /// 75% of the seeds fall within `radius` of `point`.
    Impact {
        count: usize,
        seed: u64,
        point: Vec2,
        radius: f32,
    },
}
impl Pattern2 {
    pub fn count(&self) -> usize {
        match *self {
            Self::Uniform { count, .. } | Self::Impact { count, .. } => count,
        }
    }
    fn validate(&self, limits: &Limits) -> Result<()> {
        if !(1..=limits.max_fragments).contains(&self.count()) {
            return Err(Error::InvalidPattern("fragment count out of range"));
        }
        if let Self::Impact { point, radius, .. } = *self
            && (!point.is_finite() || !radius.is_finite() || radius <= 0.)
        {
            return Err(Error::InvalidPattern(
                "impact point must be finite and radius positive",
            ));
        }
        Ok(())
    }
}
/// One convex piece of a polygon.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment2 {
    pub seed_index: usize,
    pub seed: Vec2,
    /// Counter-clockwise vertices.
    pub points: Vec<Vec2>,
    /// `fracture_edge[i]` marks the edge `points[i] -> points[i + 1]` as a cut edge.
    pub fracture_edge: Vec<bool>,
    pub area: f32,
    pub centroid: Vec2,
    /// Polar moment of area about `centroid` at unit density.
    pub inertia: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Fracture2 {
    pub fragments: Vec<Fragment2>,
    pub seeds: Vec<Vec2>,
    pub source_area: f32,
}
impl Fracture2 {
    pub fn total_area(&self) -> f32 {
        self.fragments.iter().map(|f| f.area).sum()
    }
}
fn cross(a: DVec2, b: DVec2) -> f64 {
    a.x * b.y - a.y * b.x
}
fn area(points: &[DVec2]) -> f64 {
    (0..points.len())
        .map(|k| cross(points[k], points[(k + 1) % points.len()]))
        .sum::<f64>()
        / 2.
}
fn inside(points: &[DVec2], p: DVec2) -> bool {
    (0..points.len())
        .all(|k| cross(points[(k + 1) % points.len()] - points[k], p - points[k]) >= 0.)
}
/// Keeps `normal . p <= offset`; edges created along the cut are flagged.
fn clip(poly: &[(DVec2, bool)], normal: DVec2, offset: f64, eps: f64) -> Vec<(DVec2, bool)> {
    let dist: Vec<f64> = poly
        .iter()
        .map(|(p, _)| {
            let d = normal.dot(*p) - offset;
            if d.abs() <= eps { 0. } else { d }
        })
        .collect();
    let mut out: Vec<(DVec2, bool)> = Vec::with_capacity(poly.len() + 2);
    for k in 0..poly.len() {
        let next = (k + 1) % poly.len();
        let (a, flag) = poly[k];
        let (da, db) = (dist[k], dist[next]);
        if da < 0. && db > 0. {
            out.push((a, flag));
            out.push((a + (poly[next].0 - a) * (da / (da - db)), true));
        } else if da == 0. && db > 0. {
            out.push((a, true));
        } else if da <= 0. {
            out.push((a, flag));
        } else if db < 0. {
            out.push((a + (poly[next].0 - a) * (da / (da - db)), flag));
        }
    }
    let mut merged: Vec<(DVec2, bool)> = Vec::with_capacity(out.len());
    for item in out {
        match merged.last_mut() {
            Some(last) if (last.0 - item.0).length_squared() <= eps * eps => last.1 = item.1,
            _ => merged.push(item),
        }
    }
    while merged.len() > 1
        && (merged[0].0 - merged[merged.len() - 1].0).length_squared() <= eps * eps
    {
        let last = merged.pop().expect("length checked");
        merged[0].1 = last.1;
    }
    merged
}
fn build(index: usize, seed: DVec2, poly: &[(DVec2, bool)]) -> Fragment2 {
    let points: Vec<DVec2> = poly.iter().map(|(p, _)| *p).collect();
    let n = points.len();
    let a = area(&points);
    let mut centroid = DVec2::ZERO;
    let mut polar = 0.;
    for k in 0..n {
        let (p, q) = (points[k], points[(k + 1) % n]);
        let c = cross(p, q);
        centroid += (p + q) * c;
        polar += c * (p.length_squared() + p.dot(q) + q.length_squared()) / 12.;
    }
    centroid /= 6. * a;
    Fragment2 {
        seed_index: index,
        seed: seed.as_vec2(),
        points: points.iter().map(|p| p.as_vec2()).collect(),
        fracture_edge: poly.iter().map(|(_, f)| *f).collect(),
        area: a as f32,
        centroid: centroid.as_vec2(),
        inertia: (polar - a * centroid.length_squared()) as f32,
    }
}
/// Fractures a convex polygon (either winding) along Voronoi cells of seeds generated from
/// `pattern`. Deterministic for a given input.
pub fn fracture_polygon(
    polygon: &[Vec2],
    pattern: &Pattern2,
    limits: &Limits,
) -> Result<Fracture2> {
    pattern.validate(limits)?;
    if polygon.len() < 3 || polygon.len() > limits.max_vertices {
        return Err(Error::InvalidMesh("polygon needs 3..=max_vertices points"));
    }
    if polygon
        .iter()
        .any(|p| !p.is_finite() || p.abs().max_element() > 1e7)
    {
        return Err(Error::InvalidMesh("polygon coordinates must be finite"));
    }
    let mut points: Vec<DVec2> = polygon.iter().map(|p| p.as_dvec2()).collect();
    if area(&points) < 0. {
        points.reverse();
    }
    let total = area(&points);
    let (min, max) = points
        .iter()
        .fold((DVec2::MAX, DVec2::MIN), |(l, h), p| (l.min(*p), h.max(*p)));
    let scale = (max - min).length();
    if total <= 1e-12 * scale * scale {
        return Err(Error::InvalidMesh("polygon encloses no area"));
    }
    let convex = (0..points.len()).all(|k| {
        cross(
            points[(k + 1) % points.len()] - points[k],
            points[(k + 2) % points.len()] - points[(k + 1) % points.len()],
        ) >= -1e-9 * scale * scale
    });
    if !convex {
        return Err(Error::InvalidMesh("2D fracture requires a convex polygon"));
    }
    let count = pattern.count();
    let mut rng = Rng::new(match *pattern {
        Pattern2::Uniform { seed, .. } | Pattern2::Impact { seed, .. } => seed,
    });
    let separation = 1e-5 * scale;
    let mut seeds: Vec<DVec2> = Vec::with_capacity(count);
    let fresh = |seeds: &[DVec2], c: DVec2| {
        inside(&points, c) && seeds.iter().all(|s| (*s - c).length() > separation)
    };
    if let Pattern2::Impact { point, radius, .. } = *pattern {
        let wanted = ((count as f64) * crate::IMPACT_CLUSTER_FRACTION).ceil() as usize;
        let mut misses = 0;
        while seeds.len() < wanted && misses < 60 * count + 600 {
            let angle = rng.range(0., std::f64::consts::TAU);
            let r = f64::from(radius) * rng.unit();
            let c = point.as_dvec2() + DVec2::new(angle.cos(), angle.sin()) * r;
            if fresh(&seeds, c) {
                seeds.push(c);
            } else {
                misses += 1;
            }
        }
    }
    let mut budget = 400 * count + 4000;
    while seeds.len() < count {
        if budget == 0 {
            return Err(Error::NonRobust("could not place seed points in polygon"));
        }
        budget -= 1;
        let c = DVec2::new(rng.range(min.x, max.x), rng.range(min.y, max.y));
        if fresh(&seeds, c) {
            seeds.push(c);
        }
    }
    let source: Vec<(DVec2, bool)> = points.iter().map(|p| (*p, false)).collect();
    let eps = 1e-10 * scale;
    let mut fragments = Vec::new();
    for (index, &seed) in seeds.iter().enumerate() {
        let mut cell = source.clone();
        let mut order: Vec<usize> = (0..seeds.len()).filter(|&j| j != index).collect();
        order.sort_by(|&a, &b| {
            (seeds[a] - seed)
                .length_squared()
                .total_cmp(&(seeds[b] - seed).length_squared())
                .then(a.cmp(&b))
        });
        for other in order {
            let delta = seeds[other] - seed;
            let length = delta.length();
            let reach = cell
                .iter()
                .map(|(p, _)| (*p - seed).length())
                .fold(0., f64::max);
            if length / 2. > reach + eps {
                break;
            }
            let normal = delta / length;
            let offset = (seeds[other].length_squared() - seed.length_squared()) / (2. * length);
            cell = clip(&cell, normal, offset, eps);
            if cell.len() < 3 {
                break;
            }
        }
        if cell.len() >= 3 {
            let pts: Vec<DVec2> = cell.iter().map(|(p, _)| *p).collect();
            if area(&pts) > 1e-9 * total {
                fragments.push(build(index, seed, &cell));
            }
        }
    }
    Ok(Fracture2 {
        fragments,
        seeds: seeds.iter().map(|s| s.as_vec2()).collect(),
        source_area: total as f32,
    })
}
