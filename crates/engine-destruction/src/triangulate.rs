//! Cap construction: boundary edges -> closed loops -> triangles (ear clipping with holes).
use crate::{Error, Result};
use glam::{DVec2, DVec3};
use std::collections::BTreeMap;

/// Splits directed `edges` into closed loops. A vertex visited twice splits its walk into
/// separate loops, so loops touching at a vertex still come out simple.
pub(crate) fn loops(edges: &[(u32, u32)]) -> Result<Vec<Vec<u32>>> {
    let mut out: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &(a, b) in edges {
        out.entry(a).or_default().push(b);
    }
    for ends in out.values_mut() {
        ends.sort_unstable();
        ends.reverse();
    }
    let mut result = Vec::new();
    while let Some(&start) = out.iter().find(|(_, v)| !v.is_empty()).map(|(k, _)| k) {
        let mut path = vec![start];
        let mut current = start;
        loop {
            let Some(next) = out.get_mut(&current).and_then(Vec::pop) else {
                return Err(Error::NonRobust("cut boundary does not close"));
            };
            if let Some(position) = path.iter().position(|&v| v == next) {
                result.push(path.split_off(position));
                if path.is_empty() {
                    break;
                }
            }
            path.push(next);
            current = next;
        }
    }
    Ok(result)
}
fn cross2(a: DVec2, b: DVec2) -> f64 {
    a.x * b.y - a.y * b.x
}
fn signed_area(poly: &[u32], p: &dyn Fn(u32) -> DVec2) -> f64 {
    let mut sum = 0.;
    for k in 0..poly.len() {
        sum += cross2(p(poly[k]), p(poly[(k + 1) % poly.len()]));
    }
    sum / 2.
}
fn inside_polygon(poly: &[u32], point: DVec2, p: &dyn Fn(u32) -> DVec2) -> bool {
    let mut inside = false;
    for k in 0..poly.len() {
        let (a, b) = (p(poly[k]), p(poly[(k + 1) % poly.len()]));
        if (a.y > point.y) != (b.y > point.y)
            && point.x < (b.x - a.x) * (point.y - a.y) / (b.y - a.y) + a.x
        {
            inside = !inside;
        }
    }
    inside
}
fn segments_cross(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> bool {
    let d1 = cross2(b - a, c - a);
    let d2 = cross2(b - a, d - a);
    let d3 = cross2(d - c, a - c);
    let d4 = cross2(d - c, b - c);
    d1 * d2 < 0. && d3 * d4 < 0.
}
/// Joins `hole` (clockwise) into `outer` (counter-clockwise) with a zero-width bridge.
fn bridge(
    outer: &[u32],
    hole: &[u32],
    others: &[Vec<u32>],
    p: &dyn Fn(u32) -> DVec2,
) -> Result<Vec<u32>> {
    let m = (0..hole.len())
        .max_by(|&a, &b| p(hole[a]).x.total_cmp(&p(hole[b]).x))
        .ok_or(Error::NonRobust("empty hole"))?;
    let mp = p(hole[m]);
    let mut order: Vec<usize> = (0..outer.len()).collect();
    order.sort_by(|&a, &b| {
        (p(outer[a]) - mp)
            .length_squared()
            .total_cmp(&(p(outer[b]) - mp).length_squared())
            .then(a.cmp(&b))
    });
    let blocked = |vp: DVec2| {
        let rings = [outer, hole]
            .into_iter()
            .chain(others.iter().map(|o| o.as_slice()));
        for ring in rings {
            for k in 0..ring.len() {
                let (a, b) = (p(ring[k]), p(ring[(k + 1) % ring.len()]));
                if segments_cross(mp, vp, a, b) {
                    return true;
                }
            }
        }
        false
    };
    let target = order
        .into_iter()
        .find(|&i| !blocked(p(outer[i])))
        .ok_or(Error::NonRobust("no visible bridge for a cap hole"))?;
    let mut merged = outer[..=target].to_vec();
    merged.extend(hole[m..].iter().chain(&hole[..=m]));
    merged.extend(&outer[target..]);
    Ok(merged)
}
fn ear_clip(mut poly: Vec<u32>, p: &dyn Fn(u32) -> DVec2, area_eps: f64) -> Vec<[u32; 3]> {
    let mut out = Vec::new();
    let in_triangle = |q: DVec2, a: DVec2, b: DVec2, c: DVec2| {
        cross2(b - a, q - a) >= -area_eps
            && cross2(c - b, q - b) >= -area_eps
            && cross2(a - c, q - c) >= -area_eps
    };
    while poly.len() > 3 {
        let n = poly.len();
        let mut chosen = None;
        for pass in 0..2 {
            for i in 0..n {
                let (a, b, c) = (poly[(i + n - 1) % n], poly[i], poly[(i + 1) % n]);
                let (pa, pb, pc) = (p(a), p(b), p(c));
                let turn = cross2(pb - pa, pc - pb);
                if pass == 0 && turn <= area_eps || pass == 1 && turn < -area_eps {
                    continue;
                }
                let blocked = pass == 0
                    && poly.iter().any(|&q| {
                        let pq = p(q);
                        q != a
                            && q != b
                            && q != c
                            && pq != pa
                            && pq != pb
                            && pq != pc
                            && in_triangle(pq, pa, pb, pc)
                    });
                if !blocked {
                    chosen = Some(i);
                    break;
                }
            }
            if chosen.is_some() {
                break;
            }
        }
        let i = chosen.unwrap_or(0);
        out.push([poly[(i + n - 1) % n], poly[i], poly[(i + 1) % n]]);
        poly.remove(i);
    }
    if poly.len() == 3 {
        out.push([poly[0], poly[1], poly[2]]);
    }
    out
}
/// Triangulates the planar `loops` of a cap whose outward normal is `normal`. Loops with
/// positive area (counter-clockwise about `normal`) are outlines; negative ones are holes.
/// Every loop vertex is kept so the cap stays welded to the walls (T-junction free).
pub(crate) fn triangulate(
    loops: &[Vec<u32>],
    verts: &[DVec3],
    normal: DVec3,
    scale: f64,
) -> Result<Vec<[u32; 3]>> {
    let helper = if normal.x.abs() < 0.6 {
        DVec3::X
    } else {
        DVec3::Y
    };
    let u = normal.cross(helper).normalize();
    let v = normal.cross(u);
    let p = |i: u32| DVec2::new(u.dot(verts[i as usize]), v.dot(verts[i as usize]));
    let area_eps = 1e-14 * scale * scale;
    let mut outlines: Vec<Vec<u32>> = Vec::new();
    let mut holes: Vec<Vec<u32>> = Vec::new();
    let mut tris = Vec::new();
    for ring in loops {
        let area = signed_area(ring, &p);
        if area.abs() <= area_eps {
            // Zero-area ring (collinear points): fan it so its edges stay welded.
            for k in 1..ring.len().saturating_sub(1) {
                tris.push([ring[0], ring[k], ring[k + 1]]);
            }
        } else if area > 0. {
            outlines.push(ring.clone());
        } else {
            holes.push(ring.clone());
        }
    }
    let mut assigned: Vec<Vec<Vec<u32>>> = vec![Vec::new(); outlines.len()];
    for hole in holes {
        let probe = (p(hole[0]) + p(hole[1 % hole.len()])) / 2.;
        let owner = outlines
            .iter()
            .enumerate()
            .filter(|(_, o)| inside_polygon(o, probe, &p))
            .min_by(|(_, a), (_, b)| signed_area(a, &p).total_cmp(&signed_area(b, &p)))
            .map(|(i, _)| i)
            .ok_or(Error::NonRobust("cap hole outside every outline"))?;
        assigned[owner].push(hole);
    }
    for (outline, mut rings) in outlines.into_iter().zip(assigned) {
        let right = |r: &Vec<u32>| r.iter().map(|&i| p(i).x).fold(f64::MIN, f64::max);
        rings.sort_by(|a, b| right(b).total_cmp(&right(a)));
        let mut polygon = outline;
        for k in 0..rings.len() {
            polygon = bridge(&polygon, &rings[k], &rings[k + 1..], &p)?;
        }
        tris.extend(ear_clip(polygon, &p, area_eps));
    }
    Ok(tris)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(start: u32, n: u32) -> Vec<u32> {
        (start..start + n).collect()
    }
    fn area(tris: &[[u32; 3]], verts: &[DVec3]) -> f64 {
        tris.iter()
            .map(|t| {
                let (a, b, c) = (
                    verts[t[0] as usize],
                    verts[t[1] as usize],
                    verts[t[2] as usize],
                );
                (b - a).cross(c - a).z / 2.
            })
            .sum()
    }
    #[test]
    fn a_square_with_a_square_hole_triangulates_to_the_ring_area() {
        let verts: Vec<DVec3> = [
            (0., 0.),
            (4., 0.),
            (4., 4.),
            (0., 4.),
            (1., 1.),
            (1., 3.),
            (3., 3.),
            (3., 1.),
        ]
        .iter()
        .map(|p| DVec3::new(p.0, p.1, 0.))
        .collect();
        // Outline counter-clockwise, hole clockwise, as `Solid::clip` produces them.
        let tris = triangulate(&[ring(0, 4), ring(4, 4)], &verts, DVec3::Z, 6.).unwrap();
        assert!(
            (area(&tris, &verts) - 12.).abs() < 1e-9,
            "{}",
            area(&tris, &verts)
        );
        assert!(tris.iter().all(|t| {
            let (a, b, c) = (
                verts[t[0] as usize],
                verts[t[1] as usize],
                verts[t[2] as usize],
            );
            (b - a).cross(c - a).z > 0.
        }));
    }
    #[test]
    fn loops_split_at_a_pinch_vertex() {
        // Two triangles sharing vertex 0, listed as one closed walk.
        let edges = [(0, 1), (1, 2), (2, 0), (0, 3), (3, 4), (4, 0)];
        let mut found = loops(&edges).unwrap();
        found.sort();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|l| l.len() == 3 && l.contains(&0)));
        assert!(loops(&[(0, 1), (1, 2)]).is_err());
    }
    #[test]
    fn collinear_boundary_points_do_not_create_zero_area_triangles() {
        let verts: Vec<DVec3> = [(0., 0.), (1., 0.), (2., 0.), (2., 2.), (0., 2.)]
            .iter()
            .map(|p| DVec3::new(p.0, p.1, 0.))
            .collect();
        let tris = triangulate(&[ring(0, 5)], &verts, DVec3::Z, 3.).unwrap();
        assert_eq!(tris.len(), 3);
        assert!((area(&tris, &verts) - 4.).abs() < 1e-12);
        assert!(tris.iter().all(|t| {
            let (a, b, c) = (
                verts[t[0] as usize],
                verts[t[1] as usize],
                verts[t[2] as usize],
            );
            (b - a).cross(c - a).z > 1e-9
        }));
    }
}
