use super::*;

#[derive(Clone)]
struct Fragment {
    polygon: Vec<Vec2>,
    plane: [f32; 3],
    walkable: bool,
}
fn height(plane: [f32; 3], p: Vec2) -> f32 {
    plane[0] * p.x + plane[1] * p.y + plane[2]
}
fn area(p: &[Vec2]) -> f32 {
    let origin = p.first().copied().unwrap_or(Vec2::ZERO);
    p.iter()
        .zip(p.iter().cycle().skip(1))
        .map(|(a, b)| (*a - origin).perp_dot(*b - origin))
        .sum::<f32>()
        .abs()
        * 0.5
}
fn clip(polygon: &[Vec2], a: Vec2, b: Vec2) -> Vec<Vec2> {
    let mut result = Vec::new();
    for (&p, &q) in polygon.iter().zip(polygon.iter().cycle().skip(1)) {
        let dp = (b - a).perp_dot(p - a);
        let dq = (b - a).perp_dot(q - a);
        if dp >= 0. {
            result.push(p);
        }
        if (dp < 0.) != (dq < 0.) {
            result.push(p + (q - p) * (dp / (dp - dq)));
        }
    }
    result
}
fn intersection(mut polygon: Vec<Vec2>, boundary: &[Vec2]) -> Vec<Vec2> {
    for (&a, &b) in boundary.iter().zip(boundary.iter().cycle().skip(1)) {
        polygon = clip(&polygon, a, b);
    }
    polygon
}
fn spend(work: &mut usize, amount: usize) -> Result<()> {
    *work += amount;
    ensure!(*work <= MAX_WORK, "navigation bake work budget exceeded");
    Ok(())
}
fn same_plane(a: [f32; 3], b: [f32; 3], corners: &[Vec2]) -> bool {
    corners
        .iter()
        .all(|p| (height(a, *p) - height(b, *p)).abs() <= 0.0001)
}
fn corners(cell: [i32; 2], size: f32) -> [Vec2; 4] {
    let p = Vec2::new(cell[0] as f32, cell[1] as f32) * size;
    [
        p,
        p + Vec2::new(size, 0.),
        p + Vec2::splat(size),
        p + Vec2::new(0., size),
    ]
}
/// Bake explicit triangle floors into a conservative layered polygon mesh.
/// At most 65,536 XZ cells/polygons, eight planar layers per cell, and four
/// million raster/clearance operations are accepted. Incomplete cells are holes.
pub fn bake(input: &BakeInput) -> Result<NavMesh> {
    validate_input(input)?;
    let settings = &input.settings;
    let mut work = 0;
    let fragments = rasterize(input, &mut work)?;
    let mut polygons = Vec::new();
    let mut cells: BTreeMap<[i32; 2], Vec<usize>> = BTreeMap::new();
    let cell_area = settings.cell_size * settings.cell_size;
    for (cell, fragments) in &fragments {
        let square = corners(*cell, settings.cell_size);
        let mut groups: Vec<Vec<&Fragment>> = Vec::new();
        for fragment in fragments.iter().filter(|f| f.walkable) {
            if let Some(group) = groups
                .iter_mut()
                .find(|g| same_plane(g[0].plane, fragment.plane, &square))
            {
                group.push(fragment);
            } else {
                ensure!(groups.len() < 8, "navigation layer capacity exceeded");
                groups.push(vec![fragment]);
            }
        }
        for group in groups {
            let coverage: f32 = group.iter().map(|f| area(&f.polygon)).sum();
            // Positive interior overlap is rejected instead of double-counting coverage.
            for (i, a) in group.iter().enumerate() {
                for b in &group[i + 1..] {
                    spend(&mut work, 1)?;
                    ensure!(
                        area(&intersection(a.polygon.clone(), &b.polygon)) <= cell_area * 1e-5,
                        "overlapping navigation floor triangles"
                    );
                }
            }
            if coverage < cell_area * (1. - 1e-5) {
                continue;
            }
            let plane = group[0].plane;
            let low = square
                .iter()
                .map(|p| height(plane, *p))
                .fold(f32::INFINITY, f32::min);
            let high = square
                .iter()
                .map(|p| height(plane, *p))
                .fold(f32::NEG_INFINITY, f32::max);
            let mut blocked = false;
            for other in fragments {
                spend(&mut work, 1)?;
                if same_plane(plane, other.plane, &square) {
                    continue;
                }
                // A partial upper surface is also a ceiling, not only a complete layer.
                for p in &other.polygon {
                    let delta = height(other.plane, *p) - height(plane, *p);
                    if delta > 0.0001 && delta < settings.agent_height {
                        blocked = true;
                    }
                }
                // Crossing planes are unsuitable as overlapping floors in one cell.
                let min = other
                    .polygon
                    .iter()
                    .map(|p| height(other.plane, *p) - height(plane, *p))
                    .fold(f32::INFINITY, f32::min);
                let max = other
                    .polygon
                    .iter()
                    .map(|p| height(other.plane, *p) - height(plane, *p))
                    .fold(f32::NEG_INFINITY, f32::max);
                if min <= 0.0001 && max > 0.0001 {
                    blocked = true;
                }
            }
            for obstacle in &input.obstacles {
                spend(&mut work, 1)?;
                if obstacle.min[0] < square[2].x
                    && obstacle.max[0] > square[0].x
                    && obstacle.min[2] < square[2].y
                    && obstacle.max[2] > square[0].y
                    && obstacle.min[1] < high + settings.agent_height
                    && obstacle.max[1] > low
                {
                    blocked = true;
                }
            }
            if blocked {
                continue;
            }
            ensure!(
                polygons.len() < MAX_POLYGONS,
                "navigation polygon capacity exceeded"
            );
            cells.entry(*cell).or_default().push(polygons.len());
            polygons.push(Polygon {
                cell: *cell,
                plane,
                links: Vec::new(),
            });
        }
    }
    let mut polygons = erode(polygons, &cells, settings, &mut work)?;
    connect(&mut polygons, settings, &mut work)?;
    Ok(NavMesh {
        polygons,
        settings: settings.clone(),
    })
}

fn validate_input(input: &BakeInput) -> Result<()> {
    let settings = &input.settings;
    ensure!(
        settings.cell_size.is_finite()
            && (0.01..=100.).contains(&settings.cell_size)
            && settings.agent_radius.is_finite()
            && (0.0..=100.).contains(&settings.agent_radius)
            && settings.agent_height.is_finite()
            && (0.01..=100.).contains(&settings.agent_height)
            && settings.max_slope_degrees.is_finite()
            && (0.0..=80.).contains(&settings.max_slope_degrees)
            && settings.max_step.is_finite()
            && (0.0..=100.).contains(&settings.max_step),
        "invalid navigation bake settings"
    );
    ensure!(
        !input.vertices.is_empty()
            && input.vertices.len() <= 65_536
            && !input.triangles.is_empty()
            && input.triangles.len() <= 32_768
            && input.obstacles.len() <= 4096,
        "navigation geometry capacity exceeded"
    );
    ensure!(
        input.vertices.iter().all(|p| valid_point(*p)),
        "invalid navigation vertex"
    );
    for obstacle in &input.obstacles {
        ensure!(
            valid_point(obstacle.min)
                && valid_point(obstacle.max)
                && obstacle.min.iter().zip(obstacle.max).all(|(a, b)| *a < b),
            "invalid navigation obstacle"
        );
    }
    Ok(())
}

fn erode(
    polygons: Vec<Polygon>,
    cells: &BTreeMap<[i32; 2], Vec<usize>>,
    settings: &BakeSettings,
    work: &mut usize,
) -> Result<Vec<Polygon>> {
    // Conservative square erosion in XZ: every retained polygon and its complete
    // radius neighborhood must have a compatible floor. This deliberately rounds
    // radius up to a cell and can remove narrow traversable passages.
    let radius = (settings.agent_radius / settings.cell_size).ceil() as i32;
    ensure!(radius <= 128, "navigation radius/cell ratio exceeds limit");
    let mut keep = vec![true; polygons.len()];
    for (index, polygon) in polygons.iter().enumerate() {
        'neighbors: for dz in -radius..=radius {
            for dx in -radius..=radius {
                spend(work, 1)?;
                let key = [polygon.cell[0] + dx, polygon.cell[1] + dz];
                let sample = corners(key, settings.cell_size);
                if !cells.get(&key).is_some_and(|candidates| {
                    candidates.iter().any(|&n| {
                        sample.iter().all(|p| {
                            (height(polygons[n].plane, *p) - height(polygon.plane, *p)).abs()
                                <= settings.max_step + 0.0001
                        })
                    })
                }) {
                    keep[index] = false;
                    break 'neighbors;
                }
            }
        }
    }
    let polygons: Vec<_> = polygons
        .into_iter()
        .zip(keep)
        .filter_map(|(p, keep)| keep.then_some(p))
        .collect();
    Ok(polygons)
}

fn connect(polygons: &mut [Polygon], settings: &BakeSettings, work: &mut usize) -> Result<()> {
    let mut cells: BTreeMap<[i32; 2], Vec<usize>> = BTreeMap::new();
    for (index, polygon) in polygons.iter().enumerate() {
        cells.entry(polygon.cell).or_default().push(index);
    }
    for i in 0..polygons.len() {
        for offset in [[1, 0], [-1, 0], [0, 1], [0, -1]] {
            let key = [
                polygons[i].cell[0] + offset[0],
                polygons[i].cell[1] + offset[1],
            ];
            if let Some(candidates) = cells.get(&key) {
                let square = corners(polygons[i].cell, settings.cell_size);
                let edge = match offset {
                    [1, 0] => [square[1], square[2]],
                    [-1, 0] => [square[0], square[3]],
                    [0, 1] => [square[3], square[2]],
                    _ => [square[0], square[1]],
                };
                for &j in candidates {
                    spend(work, 1)?;
                    if edge.iter().all(|p| {
                        (polygons[i].height(*p) - polygons[j].height(*p)).abs()
                            <= settings.max_step + 0.0001
                    }) {
                        polygons[i].links.push(j);
                    }
                }
            }
        }
    }
    Ok(())
}

fn rasterize(input: &BakeInput, work: &mut usize) -> Result<BTreeMap<[i32; 2], Vec<Fragment>>> {
    let mut cells: BTreeMap<[i32; 2], Vec<Fragment>> = BTreeMap::new();
    let mut fragment_count = 0;
    let size = input.settings.cell_size;
    for triangle in &input.triangles {
        ensure!(
            triangle
                .iter()
                .all(|i| (*i as usize) < input.vertices.len()),
            "invalid navigation triangle index"
        );
        let vertices = triangle.map(|i| Vec3::from(input.vertices[i as usize]));
        let normal = (vertices[1] - vertices[0]).cross(vertices[2] - vertices[0]);
        ensure!(
            normal.length_squared() > 1e-12,
            "degenerate navigation triangle"
        );
        // Vertical triangles have no walkable footprint; walls belong in obstacles.
        if normal.y.abs() <= 1e-8 {
            continue;
        }
        let walkable = normal.y.abs() / normal.length() + 1e-6
            >= input.settings.max_slope_degrees.to_radians().cos();
        let plane = [
            -normal.x / normal.y,
            -normal.z / normal.y,
            normal.dot(vertices[0]) / normal.y,
        ];
        let mut footprint = vertices.map(|v| Vec2::new(v.x, v.z)).to_vec();
        if (footprint[1] - footprint[0]).perp_dot(footprint[2] - footprint[0]) < 0. {
            footprint.reverse();
        }
        let min = footprint
            .iter()
            .copied()
            .fold(Vec2::splat(f32::INFINITY), Vec2::min)
            / size;
        let max = footprint
            .iter()
            .copied()
            .fold(Vec2::splat(f32::NEG_INFINITY), Vec2::max)
            / size;
        let x0 = min.x.floor() as i32;
        let z0 = min.y.floor() as i32;
        let x1 = max.x.ceil() as i32;
        let z1 = max.y.ceil() as i32;
        ensure!(
            i64::from(x1 - x0) * i64::from(z1 - z0) <= MAX_POLYGONS as i64,
            "navigation raster bounds exceeded"
        );
        for z in z0..z1 {
            for x in x0..x1 {
                spend(work, 1)?;
                let cell = [x, z];
                let polygon = intersection(footprint.clone(), &corners(cell, size));
                if area(&polygon) <= size * size * 1e-7 {
                    continue;
                }
                ensure!(
                    cells.len() < MAX_POLYGONS || cells.contains_key(&cell),
                    "navigation raster cell capacity exceeded"
                );
                let fragments = cells.entry(cell).or_default();
                fragment_count += 1;
                ensure!(
                    fragment_count <= 262_144,
                    "navigation fragment storage capacity exceeded"
                );
                ensure!(
                    fragments.len() < 128,
                    "navigation cell fragment capacity exceeded"
                );
                fragments.push(Fragment {
                    polygon,
                    plane,
                    walkable,
                });
            }
        }
    }
    Ok(cells)
}
