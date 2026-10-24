use glam::Vec2;
fn area(a: Vec2, b: Vec2, c: Vec2) -> f32 {
    (c - a).perp_dot(b - a)
}
fn equal(a: Vec2, b: Vec2) -> bool {
    a.distance_squared(b) <= 1e-12
}

/// String pulling over an ordered corridor, bounded even for adversarial restarts.
pub(super) fn pull(portals: &[(Vec2, Vec2)], limit: usize) -> Option<Vec<Vec2>> {
    let mut output = vec![portals[0].0];
    let mut apex = portals[0].0;
    let mut left = apex;
    let mut right = apex;
    let mut li = 0;
    let mut ri = 0;
    let mut i = 1;
    let mut work = 0;
    while i < portals.len() {
        work += 1;
        if work > super::MAX_WORK || output.len() >= limit {
            return None;
        }
        let (nl, nr) = portals[i];
        if area(apex, right, nr) <= 0. {
            if equal(apex, right) || area(apex, left, nr) > 0. {
                right = nr;
                ri = i;
            } else {
                output.push(left);
                apex = left;
                right = apex;
                i = li + 1;
                ri = li;
                continue;
            }
        }
        if area(apex, left, nl) >= 0. {
            if equal(apex, left) || area(apex, right, nl) < 0. {
                left = nl;
                li = i;
            } else {
                output.push(right);
                apex = right;
                left = apex;
                i = ri + 1;
                li = ri;
                continue;
            }
        }
        i += 1;
    }
    let goal = portals.last()?.0;
    if !equal(*output.last()?, goal) {
        output.push(goal);
    }
    Some(output)
}
pub(super) fn crossing(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<Vec2> {
    let ab = b - a;
    let cd = d - c;
    let det = ab.perp_dot(cd);
    if det.abs() < 1e-8 {
        for p in [c, d, a, b] {
            let within = |x: Vec2, y: Vec2| {
                (y - x).perp_dot(p - x).abs() < 1e-5 && (p - x).dot(p - y) <= 1e-5
            };
            if within(a, b) && within(c, d) {
                return Some(p);
            }
        }
        return None;
    }
    let t = (c - a).perp_dot(cd) / det;
    let u = (c - a).perp_dot(ab) / det;
    if (-1e-5..=1.00001).contains(&t) && (-1e-5..=1.00001).contains(&u) {
        Some(a + ab * t.clamp(0., 1.))
    } else {
        None
    }
}
