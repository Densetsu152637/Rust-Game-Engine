//! Deterministic seed point generation inside a closed mesh.
use crate::mesh::Prepared;
use crate::rng::Rng;
use crate::{Error, Limits, Pattern, Result};
use glam::DVec3;

/// Share of impact-pattern seeds drawn inside the impact radius.
pub const IMPACT_CLUSTER_FRACTION: f64 = 0.75;

fn accept(prep: &Prepared, seeds: &[DVec3], candidate: DVec3, separation: f64) -> bool {
    prep.contains(candidate)
        && seeds
            .iter()
            .all(|s| (*s - candidate).length_squared() > separation * separation)
}
pub(crate) fn generate(prep: &Prepared, pattern: &Pattern, limits: &Limits) -> Result<Vec<DVec3>> {
    pattern.validate(limits)?;
    let count = pattern.count();
    let mut rng = Rng::new(pattern.seed());
    let separation = 1e-5 * prep.scale();
    let mut seeds: Vec<DVec3> = Vec::with_capacity(count);
    let mut budget = 400 * count + 4000;
    if let Pattern::Impact { point, radius, .. } = *pattern {
        let centre = point.as_dvec3();
        let wanted = ((count as f64) * IMPACT_CLUSTER_FRACTION).ceil() as usize;
        let mut misses = 0;
        while seeds.len() < wanted && misses < 60 * count + 600 {
            let candidate = centre + rng.direction() * (f64::from(radius) * rng.unit());
            if accept(prep, &seeds, candidate, separation) {
                seeds.push(candidate);
            } else {
                misses += 1;
            }
        }
    }
    while seeds.len() < count {
        if budget == 0 {
            return Err(Error::NonRobust(
                "could not place seed points inside the mesh",
            ));
        }
        budget -= 1;
        let candidate = DVec3::new(
            rng.range(prep.min.x, prep.max.x),
            rng.range(prep.min.y, prep.max.y),
            rng.range(prep.min.z, prep.max.z),
        );
        if accept(prep, &seeds, candidate, separation) {
            seeds.push(candidate);
        }
    }
    Ok(seeds)
}
