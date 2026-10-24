#![allow(dead_code)]

use engine_mesh_lod::MeshData;
use glam::{Vec2, Vec3};

/// `n` x `n` cell plane in XZ (y = 0), side length `size`, normals +Y, CCW from +Y.
pub fn plane(n: usize, size: f32) -> MeshData {
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    for j in 0..=n {
        for i in 0..=n {
            let (u, v) = (i as f32 / n as f32, j as f32 / n as f32);
            positions.push(Vec3::new(u * size, 0.0, v * size));
            uvs.push(Vec2::new(u, v));
        }
    }
    let idx = |i: usize, j: usize| (j * (n + 1) + i) as u32;
    let mut indices = Vec::new();
    for j in 0..n {
        for i in 0..n {
            let (a, b, c, d) = (idx(i, j), idx(i + 1, j), idx(i, j + 1), idx(i + 1, j + 1));
            indices.extend([a, c, b, b, c, d]);
        }
    }
    let count = positions.len();
    MeshData::new(positions, indices)
        .with_normals(vec![Vec3::Y; count])
        .with_uvs(uvs)
}

/// Plane whose column `i == n / 2` is duplicated so U jumps by 0.5 across it.
pub fn plane_with_uv_seam(n: usize, size: f32) -> (MeshData, Vec<Vec3>) {
    assert!(n.is_multiple_of(2));
    let seam = n / 2;
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    // Left block columns 0..=seam, right block columns seam..=n (duplicated seam column).
    let mut index_of = vec![vec![0u32; n + 1]; (n + 1) * 2];
    let mut seam_positions = Vec::new();
    for (block, cols) in [(0usize, 0..=seam), (1usize, seam..=n)] {
        for j in 0..=n {
            for i in cols.clone() {
                let (u, v) = (i as f32 / n as f32, j as f32 / n as f32);
                index_of[block * (n + 1) + j][i] = positions.len() as u32;
                positions.push(Vec3::new(u * size, 0.0, v * size));
                uvs.push(Vec2::new(u + block as f32 * 0.5, v));
                if i == seam && block == 0 {
                    seam_positions.push(Vec3::new(u * size, 0.0, v * size));
                }
            }
        }
    }
    let mut indices = Vec::new();
    for j in 0..n {
        for i in 0..n {
            let block = usize::from(i >= seam);
            let at = |ii: usize, jj: usize| index_of[block * (n + 1) + jj][ii];
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
            indices.extend([a, c, b, b, c, d]);
        }
    }
    let count = positions.len();
    (
        MeshData::new(positions, indices)
            .with_normals(vec![Vec3::Y; count])
            .with_uvs(uvs),
        seam_positions,
    )
}

/// Welded UV sphere with single poles; `segments` x `rings`, normals = positions / radius.
pub fn uv_sphere(segments: usize, rings: usize, radius: f32, center: Vec3) -> MeshData {
    let mut positions = vec![Vec3::Y * radius];
    for i in 1..rings {
        let theta = std::f32::consts::PI * i as f32 / rings as f32;
        for j in 0..segments {
            let phi = std::f32::consts::TAU * j as f32 / segments as f32;
            positions.push(Vec3::new(
                theta.sin() * phi.cos(),
                theta.cos(),
                theta.sin() * phi.sin(),
            ) * radius);
        }
    }
    positions.push(-Vec3::Y * radius);
    let ring = |i: usize, j: usize| (1 + (i - 1) * segments + j % segments) as u32;
    let bottom = (positions.len() - 1) as u32;
    let mut indices = Vec::new();
    for j in 0..segments {
        indices.extend([0, ring(1, j + 1), ring(1, j)]);
    }
    for i in 1..rings - 1 {
        for j in 0..segments {
            let (a, b, c, d) = (ring(i, j), ring(i, j + 1), ring(i + 1, j), ring(i + 1, j + 1));
            indices.extend([a, b, c, b, d, c]);
        }
    }
    for j in 0..segments {
        indices.extend([bottom, ring(rings - 1, j), ring(rings - 1, j + 1)]);
    }
    // Orient outward using the signed volume.
    let volume: f32 = indices
        .chunks_exact(3)
        .map(|t| {
            let (a, b, c) = (positions[t[0] as usize], positions[t[1] as usize], positions[t[2] as usize]);
            a.dot(b.cross(c))
        })
        .sum();
    if volume < 0.0 {
        for t in indices.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
    }
    let normals: Vec<Vec3> = positions.iter().map(|p| p.normalize()).collect();
    let positions: Vec<Vec3> = positions.iter().map(|p| *p + center).collect();
    MeshData::new(positions, indices).with_normals(normals)
}

/// Smallest triangle double-area in the mesh.
pub fn min_double_area(mesh: &MeshData) -> f32 {
    mesh.indices
        .chunks_exact(3)
        .map(|t| {
            let (a, b, c) = (mesh.positions[t[0] as usize], mesh.positions[t[1] as usize], mesh.positions[t[2] as usize]);
            (b - a).cross(c - a).length()
        })
        .fold(f32::INFINITY, f32::min)
}

pub fn total_area(mesh: &MeshData) -> f32 {
    mesh.indices
        .chunks_exact(3)
        .map(|t| {
            let (a, b, c) = (mesh.positions[t[0] as usize], mesh.positions[t[1] as usize], mesh.positions[t[2] as usize]);
            0.5 * (b - a).cross(c - a).length()
        })
        .sum()
}
