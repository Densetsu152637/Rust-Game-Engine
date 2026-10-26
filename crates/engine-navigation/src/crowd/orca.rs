//! ORCA half-plane construction and the RVO2 incremental 2D linear programs (f32, branch-stable).
use glam::Vec2;

const EPSILON: f32 = 1e-5;

#[derive(Clone, Copy, Debug)]
pub(super) struct Line {
    pub point: Vec2,
    pub direction: Vec2,
}

/// ORCA constraint of `me` against a neighbour (or a static point when `responsibility == 1`).
/// `relative_position` points from me to the other; `relative_velocity = v_me - v_other`.
pub(super) fn pair_line(
    relative_position: Vec2,
    relative_velocity: Vec2,
    velocity: Vec2,
    combined_radius: f32,
    inv_horizon: f32,
    inv_dt: f32,
    responsibility: f32,
) -> Line {
    let dist_sq = relative_position.length_squared();
    let radius_sq = combined_radius * combined_radius;
    let direction;
    let u;
    if dist_sq > radius_sq {
        let w = relative_velocity - relative_position * inv_horizon;
        let w_sq = w.length_squared();
        let dot1 = w.dot(relative_position);
        if dot1 < 0. && dot1 * dot1 > radius_sq * w_sq {
            let w_len = w_sq.sqrt();
            let unit_w = w / w_len;
            direction = Vec2::new(unit_w.y, -unit_w.x);
            u = unit_w * (combined_radius * inv_horizon - w_len);
        } else {
            let leg = (dist_sq - radius_sq).sqrt();
            let rp = relative_position;
            direction = if rp.perp_dot(w) > 0. {
                Vec2::new(
                    rp.x * leg - rp.y * combined_radius,
                    rp.x * combined_radius + rp.y * leg,
                ) / dist_sq
            } else {
                -Vec2::new(
                    rp.x * leg + rp.y * combined_radius,
                    -rp.x * combined_radius + rp.y * leg,
                ) / dist_sq
            };
            u = direction * relative_velocity.dot(direction) - relative_velocity;
        }
    } else {
        // Already overlapping: resolve within a single step.
        let w = relative_velocity - relative_position * inv_dt;
        let w_len = w.length();
        let unit_w = if w_len > EPSILON {
            w / w_len
        } else {
            -relative_position.normalize_or_zero()
        };
        direction = Vec2::new(unit_w.y, -unit_w.x);
        u = unit_w * (combined_radius * inv_dt - w_len);
    }
    Line {
        point: velocity + u * responsibility,
        direction,
    }
}

fn linear_program1(
    lines: &[Line],
    line_no: usize,
    radius: f32,
    opt: Vec2,
    direction_opt: bool,
) -> Option<Vec2> {
    let l = lines[line_no];
    let dot = l.point.dot(l.direction);
    let discriminant = dot * dot + radius * radius - l.point.length_squared();
    if discriminant < 0. {
        return None;
    }
    let sq = discriminant.sqrt();
    let mut t_left = -dot - sq;
    let mut t_right = -dot + sq;
    for other in &lines[..line_no] {
        let denominator = l.direction.perp_dot(other.direction);
        let numerator = other.direction.perp_dot(l.point - other.point);
        if denominator.abs() <= EPSILON {
            if numerator < 0. {
                return None;
            }
            continue;
        }
        let t = numerator / denominator;
        if denominator >= 0. {
            t_right = t_right.min(t);
        } else {
            t_left = t_left.max(t);
        }
        if t_left > t_right {
            return None;
        }
    }
    let t = if direction_opt {
        if opt.dot(l.direction) > 0. {
            t_right
        } else {
            t_left
        }
    } else {
        l.direction.dot(opt - l.point).clamp(t_left, t_right)
    };
    Some(l.point + l.direction * t)
}

/// Returns the new velocity and the index of the first infeasible line (`lines.len()` if none).
fn linear_program2(lines: &[Line], radius: f32, opt: Vec2, direction_opt: bool) -> (Vec2, usize) {
    let mut result = if direction_opt {
        opt * radius
    } else if opt.length_squared() > radius * radius {
        opt.normalize() * radius
    } else {
        opt
    };
    for (i, l) in lines.iter().enumerate() {
        if l.direction.perp_dot(l.point - result) > 0. {
            match linear_program1(lines, i, radius, opt, direction_opt) {
                Some(r) => result = r,
                None => return (result, i),
            }
        }
    }
    (result, lines.len())
}

fn linear_program3(
    lines: &[Line],
    num_obst: usize,
    begin: usize,
    radius: f32,
    mut result: Vec2,
) -> Vec2 {
    let mut distance = 0.;
    for i in begin..lines.len() {
        if lines[i].direction.perp_dot(lines[i].point - result) > distance {
            let mut proj: Vec<Line> = lines[..num_obst].to_vec();
            for j in num_obst..i {
                let determinant = lines[i].direction.perp_dot(lines[j].direction);
                let point = if determinant.abs() <= EPSILON {
                    if lines[i].direction.dot(lines[j].direction) > 0. {
                        continue;
                    }
                    (lines[i].point + lines[j].point) * 0.5
                } else {
                    lines[i].point
                        + lines[i].direction
                            * (lines[j].direction.perp_dot(lines[i].point - lines[j].point)
                                / determinant)
                };
                proj.push(Line {
                    point,
                    direction: (lines[j].direction - lines[i].direction).normalize_or_zero(),
                });
            }
            let opt = Vec2::new(-lines[i].direction.y, lines[i].direction.x);
            let (candidate, fail) = linear_program2(&proj, radius, opt, true);
            if fail == proj.len() {
                result = candidate;
            }
            distance = lines[i].direction.perp_dot(lines[i].point - result);
        }
    }
    result
}

/// Picks the velocity closest to `preferred` inside every ORCA half-plane (RVO2 `computeNewVelocity`).
pub(super) fn solve(lines: &[Line], num_obst: usize, max_speed: f32, preferred: Vec2) -> Vec2 {
    let (velocity, fail) = linear_program2(lines, max_speed, preferred, false);
    if fail < lines.len() {
        linear_program3(lines, num_obst, fail, max_speed, velocity)
    } else {
        velocity
    }
}
