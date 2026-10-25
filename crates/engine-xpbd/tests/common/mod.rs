#![allow(dead_code)]
use std::collections::BTreeMap;

use engine_xpbd::{ColliderProxy, ProxyShape, XpbdWorld};
use glam::{Quat, Vec3};

pub const DT: f32 = 1.0 / 60.0;

pub fn run(world: &mut XpbdWorld, seconds: f32, proxies: &[ColliderProxy]) {
    let frames = (seconds / DT).round() as usize;
    for _ in 0..frames {
        world.step(DT, proxies).unwrap();
    }
}

pub fn proxy(
    id: u64,
    position: Vec3,
    rotation: Quat,
    shape: ProxyShape,
    friction: f32,
) -> ColliderProxy {
    ColliderProxy {
        id,
        position,
        rotation,
        linear_velocity: Vec3::ZERO,
        angular_velocity: Vec3::ZERO,
        shape,
        friction,
    }
}

/// Largest relative edge elongation `|x_a - x_b| / rest - 1` over unique triangle edges.
pub fn max_stretch(rest: &[Vec3], current: &[Vec3], triangles: &[[u32; 3]]) -> f32 {
    let mut edges = std::collections::BTreeSet::new();
    for &[a, b, c] in triangles {
        for (p, q) in [(a, b), (b, c), (c, a)] {
            edges.insert((p.min(q) as usize, p.max(q) as usize));
        }
    }
    edges
        .into_iter()
        .map(|(a, b)| current[a].distance(current[b]) / rest[a].distance(rest[b]) - 1.0)
        .fold(f32::NEG_INFINITY, f32::max)
}

/// Mean unsigned angle (radians) between the normals of triangles sharing an edge.
pub fn mean_fold_angle(positions: &[Vec3], triangles: &[[u32; 3]]) -> f32 {
    let normal = |t: [u32; 3]| {
        let [a, b, c] = t.map(|i| positions[i as usize]);
        (b - a).cross(c - a).normalize()
    };
    let mut by_edge: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
    for (index, &[a, b, c]) in triangles.iter().enumerate() {
        for (p, q) in [(a, b), (b, c), (c, a)] {
            by_edge.entry((p.min(q), p.max(q))).or_default().push(index);
        }
    }
    let angles: Vec<f32> = by_edge
        .values()
        .filter(|tris| tris.len() == 2)
        .map(|tris| {
            normal(triangles[tris[0]])
                .dot(normal(triangles[tris[1]]))
                .clamp(-1.0, 1.0)
                .acos()
        })
        .collect();
    angles.iter().sum::<f32>() / angles.len() as f32
}

pub fn max_speed(velocities: &[Vec3]) -> f32 {
    velocities.iter().map(|v| v.length()).fold(0.0, f32::max)
}

pub fn centroid(positions: &[Vec3]) -> Vec3 {
    positions.iter().copied().sum::<Vec3>() / positions.len() as f32
}

/// Total linear momentum `sum m v` for particles with positive inverse mass.
pub fn momentum(velocities: &[Vec3], inverse_masses: &[f32]) -> Vec3 {
    velocities
        .iter()
        .zip(inverse_masses)
        .filter(|(_, w)| **w > 0.0)
        .map(|(v, w)| *v / *w)
        .sum()
}

pub fn total_mass(inverse_masses: &[f32]) -> f32 {
    inverse_masses
        .iter()
        .filter(|w| **w > 0.0)
        .map(|w| 1.0 / w)
        .sum()
}
