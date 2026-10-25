//! The built-in node library.
//!
//! Every node is a pure function of its inputs, parameters, derived seed and the surface, so
//! a run is reproducible. Point order is preserved unless a node documents otherwise.
use crate::data::{Data, Field, Instance, InstanceKind, MathOp, Point};
use crate::noise::{Distance, Fractal, Noise, NoiseKind, NoiseParams};
use crate::registry::{
    NodeContext, NodeDefinition, NodeError, Outputs, ParamSpec, PortSpec, PortType, Registry,
};
use crate::rng::{Rng, hash_cell};
use crate::surface::Aabb;
use glam::{Quat, Vec3};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

const BIG: f64 = 1.0e6;

fn def(
    kind: &str,
    title: &str,
    doc: &str,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    params: Vec<ParamSpec>,
) -> NodeDefinition {
    NodeDefinition {
        kind: kind.into(),
        title: title.into(),
        doc: doc.into(),
        inputs,
        outputs,
        params,
    }
}

fn points_out(points: Vec<Point>) -> Outputs {
    BTreeMap::from([("points".to_owned(), Data::points(points))])
}

fn checked_points(ctx: &NodeContext<'_>, points: Vec<Point>) -> Result<Outputs, NodeError> {
    if points.len() > ctx.run.limits.max_points {
        return Err(NodeError::new(
            "pcg.limit.points",
            format!(
                "output has {} points, above the limit of {}",
                points.len(),
                ctx.run.limits.max_points
            ),
        ));
    }
    Ok(points_out(points))
}

pub(crate) fn register_builtin(registry: &mut Registry) {
    let points = |name: &str| PortSpec::required(name, PortType::Points);
    let out_points = || vec![PortSpec::required("points", PortType::Points)];
    let mut add = |d: NodeDefinition, f: fn(&NodeContext<'_>) -> Result<Outputs, NodeError>| {
        registry
            .register(d, f)
            .expect("built-in node definitions are valid");
    };

    // Samplers.
    add(
        def(
            "grid_sampler",
            "Grid sampler",
            "Regular grid of points over the generation bounds, snapped to the surface.",
            vec![],
            out_points(),
            vec![
                ParamSpec::float("spacing", 2.0, 0.001, BIG),
                ParamSpec::float("jitter", 0.0, 0.0, 1.0),
            ],
        ),
        grid_sampler,
    );
    add(
        def(
            "random_sampler",
            "Random sampler",
            "Uniformly distributed random points over the generation bounds.",
            vec![],
            out_points(),
            vec![ParamSpec::int("count", 100, 0, 1_000_000)],
        ),
        random_sampler,
    );
    add(
        def(
            "poisson_sampler",
            "Poisson-disk sampler",
            "Blue-noise points with a guaranteed minimum spacing (Bridson).",
            vec![],
            out_points(),
            vec![
                ParamSpec::float("min_distance", 2.0, 0.01, BIG),
                ParamSpec::int("attempts", 30, 1, 200),
                ParamSpec::int("max_points", 100_000, 1, 1_000_000),
            ],
        ),
        poisson_sampler,
    );

    // Fields.
    add(
        def(
            "noise_field",
            "Noise field",
            "Seeded noise remapped to [min, max], sampled at absolute world coordinates.",
            vec![],
            vec![PortSpec::required("field", PortType::Field)],
            vec![
                ParamSpec::choice(
                    "noise",
                    &[
                        "perlin",
                        "simplex",
                        "worley_f1",
                        "worley_f2",
                        "worley_f2_minus_f1",
                    ],
                    "perlin",
                ),
                ParamSpec::choice("fractal", &["none", "fbm", "ridged"], "none"),
                ParamSpec::float("frequency", 0.05, 1.0e-6, BIG),
                ParamSpec::int("octaves", 4, 1, 16),
                ParamSpec::float("lacunarity", 2.0, 1.0, 16.0),
                ParamSpec::float("gain", 0.5, 0.0, 1.0),
                ParamSpec::choice("distance", &["euclidean", "manhattan"], "euclidean"),
                ParamSpec::float("jitter", 1.0, 0.0, 1.0),
                ParamSpec::float("min", 0.0, -BIG, BIG),
                ParamSpec::float("max", 1.0, -BIG, BIG),
            ],
        ),
        noise_field,
    );
    add(
        def(
            "surface_field",
            "Surface field",
            "Field from the ground: height (remapped over low..high), slope (degrees/90) or paint layer weight.",
            vec![],
            vec![PortSpec::required("field", PortType::Field)],
            vec![
                ParamSpec::choice("source", &["height", "slope", "layer"], "height"),
                ParamSpec::string("layer", ""),
                ParamSpec::float("low", 0.0, -BIG, BIG),
                ParamSpec::float("high", 100.0, -BIG, BIG),
            ],
        ),
        surface_field,
    );
    add(
        def(
            "field_math",
            "Field math",
            "Combines field `a` with field `b` (or the constant when `b` is not connected).",
            vec![
                PortSpec::required("a", PortType::Field),
                PortSpec::optional("b", PortType::Field),
            ],
            vec![PortSpec::required("field", PortType::Field)],
            vec![
                ParamSpec::choice(
                    "op",
                    &["add", "subtract", "multiply", "min", "max"],
                    "multiply",
                ),
                ParamSpec::float("constant", 1.0, -BIG, BIG),
            ],
        ),
        field_math,
    );

    // Filters.
    add(
        def(
            "density_filter",
            "Density filter",
            "Evaluates a field at each point, stores it as the density and keeps points by threshold or probability.",
            vec![
                points("points"),
                PortSpec::required("field", PortType::Field),
            ],
            out_points(),
            vec![
                ParamSpec::choice("mode", &["threshold", "probabilistic"], "threshold"),
                ParamSpec::float("threshold", 0.5, -BIG, BIG),
                ParamSpec::boolean("invert", false),
            ],
        ),
        density_filter,
    );
    add(
        def(
            "height_filter",
            "Height filter",
            "Keeps points whose height lies in [min, max].",
            vec![points("points")],
            out_points(),
            vec![
                ParamSpec::float("min", -BIG, -BIG, BIG),
                ParamSpec::float("max", BIG, -BIG, BIG),
            ],
        ),
        height_filter,
    );
    add(
        def(
            "slope_filter",
            "Slope filter",
            "Keeps points whose ground slope in degrees lies in [min_degrees, max_degrees].",
            vec![points("points")],
            out_points(),
            vec![
                ParamSpec::float("min_degrees", 0.0, 0.0, 90.0),
                ParamSpec::float("max_degrees", 45.0, 0.0, 90.0),
            ],
        ),
        slope_filter,
    );
    add(
        def(
            "bounds_filter",
            "Bounds filter",
            "Keeps points inside the box [min, max] (or outside it when inverted).",
            vec![points("points")],
            out_points(),
            vec![
                ParamSpec::vec3("min", [-BIG; 3], -BIG, BIG),
                ParamSpec::vec3("max", [BIG; 3], -BIG, BIG),
                ParamSpec::boolean("invert", false),
            ],
        ),
        bounds_filter,
    );
    add(
        def(
            "self_prune",
            "Self pruning",
            "Removes points closer than min_distance to an already kept point; higher density wins, then earlier index.",
            vec![points("points")],
            out_points(),
            vec![ParamSpec::float("min_distance", 1.0, 0.001, BIG)],
        ),
        self_prune,
    );

    // Modifiers.
    add(
        def(
            "transform_points",
            "Transform points",
            "Randomizes position, yaw, pitch/roll and scale per point and optionally aligns to the ground normal.",
            vec![points("points")],
            out_points(),
            vec![
                ParamSpec::vec3("position_jitter", [0.0; 3], 0.0, BIG),
                ParamSpec::vec3("offset", [0.0; 3], -BIG, BIG),
                ParamSpec::boolean("snap_to_surface", true),
                ParamSpec::float("yaw_min_degrees", 0.0, -3600.0, 3600.0),
                ParamSpec::float("yaw_max_degrees", 0.0, -3600.0, 3600.0),
                ParamSpec::float("tilt_max_degrees", 0.0, 0.0, 90.0),
                ParamSpec::float("scale_min", 1.0, 0.0001, 10_000.0),
                ParamSpec::float("scale_max", 1.0, 0.0001, 10_000.0),
                ParamSpec::boolean("uniform_scale", true),
                ParamSpec::boolean("align_to_normal", false),
            ],
        ),
        transform_points,
    );
    add(
        def(
            "merge",
            "Merge",
            "Union of two point sets (a then b); points of b within dedupe_distance of a point of a are dropped.",
            vec![points("a"), points("b")],
            out_points(),
            vec![ParamSpec::float("dedupe_distance", 0.0, 0.0, BIG)],
        ),
        merge,
    );
    add(
        def(
            "difference",
            "Difference",
            "Points of a that are not within radius of any point of b.",
            vec![points("a"), points("b")],
            out_points(),
            vec![ParamSpec::float("radius", 1.0, 0.0, BIG)],
        ),
        difference,
    );

    // Spawner.
    add(
        def(
            "spawner",
            "Spawner",
            "Turns points into instances, choosing a prefab or mesh asset per point by weight.",
            vec![points("points")],
            vec![PortSpec::required("instances", PortType::Instances)],
            vec![ParamSpec::entries("entries")],
        ),
        spawner,
    );
}

// ---------------------------------------------------------------------------------------
// Sampling helpers
// ---------------------------------------------------------------------------------------

fn area(ctx: &NodeContext<'_>) -> Aabb {
    ctx.run.bounds.intersect_xz(&ctx.surface.bounds())
}

fn place(ctx: &NodeContext<'_>, x: f32, z: f32, rng: &mut Rng) -> Point {
    let mut p = Point::new(Vec3::new(x, ctx.surface.height(x, z), z));
    p.normal = ctx.surface.normal(x, z);
    p.seed = rng.next_u32();
    p
}

fn grid_sampler(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let spacing = ctx.params.float("spacing") as f32;
    let jitter = ctx.params.float("jitter") as f32;
    let area = area(ctx);
    if area.is_empty_xz() {
        return Ok(points_out(vec![]));
    }
    let size = area.size();
    let nx = (f64::from(size.x) / f64::from(spacing) + 1e-4).floor() as u64;
    let nz = (f64::from(size.z) / f64::from(spacing) + 1e-4).floor() as u64;
    if nx.saturating_mul(nz) > ctx.run.limits.max_points as u64 {
        return Err(NodeError::new(
            "pcg.limit.points",
            format!(
                "grid of {nx} x {nz} points exceeds the limit of {}",
                ctx.run.limits.max_points
            ),
        ));
    }
    let mut rng = Rng::new(ctx.seed);
    let mut out = Vec::with_capacity((nx * nz) as usize);
    for iz in 0..nz {
        for ix in 0..nx {
            let mut x = area.min.x + (ix as f32 + 0.5) * spacing;
            let mut z = area.min.z + (iz as f32 + 0.5) * spacing;
            if jitter > 0.0 {
                x += (rng.next_f32() - 0.5) * spacing * jitter;
                z += (rng.next_f32() - 0.5) * spacing * jitter;
            }
            out.push(place(ctx, x, z, &mut rng));
        }
    }
    checked_points(ctx, out)
}

fn random_sampler(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let count = ctx.params.int("count").max(0) as usize;
    if count > ctx.run.limits.max_points {
        return Err(NodeError::new(
            "pcg.limit.points",
            format!(
                "count {count} exceeds the limit of {}",
                ctx.run.limits.max_points
            ),
        ));
    }
    let area = area(ctx);
    if area.is_empty_xz() {
        return Ok(points_out(vec![]));
    }
    let mut rng = Rng::new(ctx.seed);
    let out = (0..count)
        .map(|_| {
            let x = rng.range_f32(area.min.x, area.max.x);
            let z = rng.range_f32(area.min.z, area.max.z);
            place(ctx, x, z, &mut rng)
        })
        .collect();
    checked_points(ctx, out)
}

fn poisson_sampler(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let r = ctx.params.float("min_distance") as f32;
    let attempts = ctx.params.int("attempts").max(1);
    let cap = (ctx.params.int("max_points").max(1) as usize).min(ctx.run.limits.max_points);
    let area = area(ctx);
    if area.is_empty_xz() {
        return Ok(points_out(vec![]));
    }
    let cell = r / std::f32::consts::SQRT_2;
    let (w, h) = (area.size().x, area.size().z);
    let gw = (w / cell).ceil().max(1.0) as u64;
    let gh = (h / cell).ceil().max(1.0) as u64;
    if gw.saturating_mul(gh) > 16_000_000 {
        return Err(NodeError::new(
            "pcg.limit.points",
            "min_distance is too small for the generation bounds (acceleration grid above 16M cells)",
        ));
    }
    let (gw, gh) = (gw as usize, gh as usize);
    let mut grid = vec![u32::MAX; gw * gh];
    let mut rng = Rng::new(ctx.seed);
    let mut samples: Vec<(f32, f32)> = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    let cell_of = |x: f32, z: f32| {
        (
            (((x - area.min.x) / cell) as usize).min(gw - 1),
            (((z - area.min.z) / cell) as usize).min(gh - 1),
        )
    };
    let first = (
        rng.range_f32(area.min.x, area.max.x),
        rng.range_f32(area.min.z, area.max.z),
    );
    let (cx, cz) = cell_of(first.0, first.1);
    grid[cz * gw + cx] = 0;
    samples.push(first);
    active.push(0);
    while !active.is_empty() && samples.len() < cap {
        let slot = rng.below(active.len() as u32) as usize;
        let (bx, bz) = samples[active[slot]];
        let mut accepted = false;
        for _ in 0..attempts {
            let angle = rng.next_f32() * std::f32::consts::TAU;
            let radius = r * (1.0 + rng.next_f32());
            let (x, z) = (bx + angle.cos() * radius, bz + angle.sin() * radius);
            if x < area.min.x || x >= area.max.x || z < area.min.z || z >= area.max.z {
                continue;
            }
            let (cx, cz) = cell_of(x, z);
            let mut clear = true;
            'scan: for nz in cz.saturating_sub(2)..=(cz + 2).min(gh - 1) {
                for nx in cx.saturating_sub(2)..=(cx + 2).min(gw - 1) {
                    let index = grid[nz * gw + nx];
                    if index != u32::MAX {
                        let (sx, sz) = samples[index as usize];
                        if (sx - x).powi(2) + (sz - z).powi(2) < r * r {
                            clear = false;
                            break 'scan;
                        }
                    }
                }
            }
            if clear {
                grid[cz * gw + cx] = samples.len() as u32;
                active.push(samples.len());
                samples.push((x, z));
                accepted = true;
                break;
            }
        }
        if !accepted {
            active.swap_remove(slot);
        }
    }
    let mut rng = Rng::new(ctx.seed ^ 0xa5a5_a5a5);
    let out = samples
        .iter()
        .map(|&(x, z)| place(ctx, x, z, &mut rng))
        .collect();
    checked_points(ctx, out)
}

// ---------------------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------------------

fn field_out(field: Field) -> Outputs {
    BTreeMap::from([("field".to_owned(), Data::field(field))])
}

fn noise_field(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let p = &ctx.params;
    let kind = match p.string("noise") {
        "simplex" => NoiseKind::Simplex,
        "worley_f1" => NoiseKind::WorleyF1,
        "worley_f2" => NoiseKind::WorleyF2,
        "worley_f2_minus_f1" => NoiseKind::WorleyF2MinusF1,
        _ => NoiseKind::Perlin,
    };
    let fractal = match p.string("fractal") {
        "fbm" => Fractal::Fbm,
        "ridged" => Fractal::Ridged,
        _ => Fractal::None,
    };
    let params = NoiseParams {
        kind,
        fractal,
        seed: ctx.seed,
        frequency: p.float("frequency"),
        octaves: p.int("octaves") as u32,
        lacunarity: p.float("lacunarity"),
        gain: p.float("gain"),
        distance: if p.string("distance") == "manhattan" {
            Distance::Manhattan
        } else {
            Distance::Euclidean
        },
        jitter: p.float("jitter"),
    };
    Ok(field_out(Field::Noise {
        noise: Box::new(Noise::new(params)),
        min: p.float("min") as f32,
        max: p.float("max") as f32,
    }))
}

fn surface_field(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let p = &ctx.params;
    Ok(field_out(match p.string("source") {
        "slope" => Field::Slope,
        "layer" => {
            if p.string("layer").is_empty() {
                return Err(NodeError::failed("source `layer` needs a layer name"));
            }
            Field::Layer(p.string("layer").to_owned())
        }
        _ => Field::Height {
            low: p.float("low") as f32,
            high: p.float("high") as f32,
        },
    }))
}

fn field_math(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let op = MathOp::parse(ctx.params.string("op")).unwrap_or(MathOp::Multiply);
    let a = ctx.field("a")?.clone();
    let b = ctx
        .optional_field("b")
        .cloned()
        .unwrap_or_else(|| Arc::new(Field::Constant(ctx.params.float("constant") as f32)));
    Ok(field_out(Field::Math { op, a, b }))
}

// ---------------------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------------------

fn point_rng(ctx: &NodeContext<'_>, point: &Point, index: usize) -> Rng {
    Rng::new(hash_cell(ctx.seed, i64::from(point.seed), index as i64, 7))
}

fn density_filter(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let field = ctx.field("field")?;
    let fcx = ctx.field_context();
    let threshold = ctx.params.float("threshold") as f32;
    let invert = ctx.params.boolean("invert");
    let probabilistic = ctx.params.string("mode") == "probabilistic";
    let mut out = Vec::new();
    for (index, point) in ctx.points("points")?.iter().enumerate() {
        let value = field.eval(&fcx, point.position.x, point.position.z);
        let keep = if probabilistic {
            let chance = if invert { 1.0 - value } else { value };
            point_rng(ctx, point, index).next_f32() < chance
        } else {
            (value >= threshold) != invert
        };
        if keep {
            out.push(Point {
                density: value.clamp(0.0, 1.0),
                ..*point
            });
        }
    }
    Ok(points_out(out))
}

fn filter(ctx: &NodeContext<'_>, keep: impl Fn(&Point) -> bool) -> Result<Outputs, NodeError> {
    Ok(points_out(
        ctx.points("points")?
            .iter()
            .filter(|p| keep(p))
            .copied()
            .collect(),
    ))
}

fn height_filter(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let (lo, hi) = (
        ctx.params.float("min") as f32,
        ctx.params.float("max") as f32,
    );
    filter(ctx, |p| p.position.y >= lo && p.position.y <= hi)
}

fn slope_filter(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let (lo, hi) = (
        ctx.params.float("min_degrees") as f32,
        ctx.params.float("max_degrees") as f32,
    );
    filter(ctx, |p| {
        let slope = p.normal.y.clamp(-1.0, 1.0).acos().to_degrees();
        slope >= lo - 1e-4 && slope <= hi + 1e-4
    })
}

fn bounds_filter(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let to_vec = |a: [f64; 3]| Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32);
    let bounds = Aabb {
        min: to_vec(ctx.params.vec3("min")),
        max: to_vec(ctx.params.vec3("max")),
    };
    let invert = ctx.params.boolean("invert");
    filter(ctx, |p| bounds.contains(p.position) != invert)
}

/// Uniform-grid neighbour lookup over point positions.
struct PointGrid {
    cell: f32,
    cells: BTreeMap<(i64, i64, i64), Vec<Vec3>>,
}

impl PointGrid {
    fn new(cell: f32) -> Self {
        Self {
            cell: cell.max(1e-4),
            cells: BTreeMap::new(),
        }
    }
    fn key(&self, p: Vec3) -> (i64, i64, i64) {
        (
            (p.x / self.cell).floor() as i64,
            (p.y / self.cell).floor() as i64,
            (p.z / self.cell).floor() as i64,
        )
    }
    fn insert(&mut self, p: Vec3) {
        self.cells.entry(self.key(p)).or_default().push(p);
    }
    /// `true` when any stored point is closer than `radius` (strictly) to `p`.
    fn any_closer_than(&self, p: Vec3, radius: f32) -> bool {
        let (kx, ky, kz) = self.key(p);
        let reach = (radius / self.cell).ceil() as i64;
        for dz in -reach..=reach {
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    if let Some(items) = self.cells.get(&(kx + dx, ky + dy, kz + dz))
                        && items.iter().any(|q| q.distance(p) < radius)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
}

fn self_prune(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let radius = ctx.params.float("min_distance") as f32;
    let points = ctx.points("points")?;
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by(|&a, &b| {
        points[b]
            .density
            .total_cmp(&points[a].density)
            .then(a.cmp(&b))
    });
    let mut grid = PointGrid::new(radius);
    let mut kept = Vec::new();
    for index in order {
        let p = points[index].position;
        if !grid.any_closer_than(p, radius) {
            grid.insert(p);
            kept.push(index);
        }
    }
    kept.sort_unstable();
    Ok(points_out(kept.into_iter().map(|i| points[i]).collect()))
}

// ---------------------------------------------------------------------------------------
// Modifiers
// ---------------------------------------------------------------------------------------

fn transform_points(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let p = &ctx.params;
    let v3 = |name: &str| {
        let a = p.vec3(name);
        Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32)
    };
    let (jitter, offset) = (v3("position_jitter"), v3("offset"));
    let snap = p.boolean("snap_to_surface");
    let (yaw_lo, yaw_hi) = (
        p.float("yaw_min_degrees") as f32,
        p.float("yaw_max_degrees") as f32,
    );
    let tilt = p.float("tilt_max_degrees") as f32;
    let (s_lo, s_hi) = (p.float("scale_min") as f32, p.float("scale_max") as f32);
    let uniform = p.boolean("uniform_scale");
    let align = p.boolean("align_to_normal");
    let mut out = Vec::new();
    for (index, point) in ctx.points("points")?.iter().enumerate() {
        let mut rng = point_rng(ctx, point, index);
        let mut q = *point;
        q.position.x += rng.range_f32(-jitter.x, jitter.x);
        q.position.z += rng.range_f32(-jitter.z, jitter.z);
        if snap {
            q.position.y = ctx.surface.height(q.position.x, q.position.z);
            q.normal = ctx.surface.normal(q.position.x, q.position.z);
        }
        q.position.y += rng.range_f32(-jitter.y, jitter.y);
        q.position += offset;
        let base = if align {
            Quat::from_rotation_arc(Vec3::Y, q.normal)
        } else {
            Quat::IDENTITY
        };
        let yaw = Quat::from_rotation_y(rng.range_f32(yaw_lo, yaw_hi).to_radians());
        let tilt_q = if tilt > 0.0 {
            Quat::from_euler(
                glam::EulerRot::XYZ,
                rng.range_f32(-tilt, tilt).to_radians(),
                0.0,
                rng.range_f32(-tilt, tilt).to_radians(),
            )
        } else {
            Quat::IDENTITY
        };
        q.rotation = (base * yaw * tilt_q * point.rotation).normalize();
        let factor = if uniform {
            Vec3::splat(rng.range_f32(s_lo, s_hi))
        } else {
            Vec3::new(
                rng.range_f32(s_lo, s_hi),
                rng.range_f32(s_lo, s_hi),
                rng.range_f32(s_lo, s_hi),
            )
        };
        q.scale = point.scale * factor;
        out.push(q);
    }
    Ok(points_out(out))
}

fn merge(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let (a, b) = (ctx.points("a")?, ctx.points("b")?);
    let dedupe = ctx.params.float("dedupe_distance") as f32;
    let mut out: Vec<Point> = a.as_ref().clone();
    if dedupe > 0.0 {
        let mut grid = PointGrid::new(dedupe);
        a.iter().for_each(|p| grid.insert(p.position));
        out.extend(
            b.iter()
                .filter(|p| !grid.any_closer_than(p.position, dedupe)),
        );
    } else {
        out.extend(b.iter());
    }
    checked_points(ctx, out)
}

fn difference(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let (a, b) = (ctx.points("a")?, ctx.points("b")?);
    let radius = ctx.params.float("radius") as f32;
    let mut grid = PointGrid::new(radius.max(0.01));
    b.iter().for_each(|p| grid.insert(p.position));
    Ok(points_out(
        a.iter()
            .filter(|p| !(radius > 0.0 && grid.any_closer_than(p.position, radius)))
            .copied()
            .collect(),
    ))
}

// ---------------------------------------------------------------------------------------
// Spawner
// ---------------------------------------------------------------------------------------

fn spawner(ctx: &NodeContext<'_>) -> Result<Outputs, NodeError> {
    let entries = ctx
        .params
        .value("entries")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let choices: Vec<(InstanceKind, String, f64)> = entries
        .iter()
        .filter_map(|entry| {
            let asset = entry.get("asset")?.as_str()?.to_owned();
            let kind = match entry.get("kind").and_then(Value::as_str) {
                Some("prefab") => InstanceKind::Prefab,
                _ => InstanceKind::Mesh,
            };
            Some((
                kind,
                asset,
                entry.get("weight").and_then(Value::as_f64).unwrap_or(1.0),
            ))
        })
        .collect();
    let total: f64 = choices.iter().map(|c| c.2).sum();
    if choices.is_empty() || total <= 0.0 {
        return Err(NodeError::failed("spawner needs at least one entry"));
    }
    let points = ctx.points("points")?;
    if points.len() > ctx.run.limits.max_instances {
        return Err(NodeError::new(
            "pcg.limit.instances",
            format!(
                "{} points would spawn more than the limit of {} instances",
                points.len(),
                ctx.run.limits.max_instances
            ),
        ));
    }
    let instances = points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let mut target = point_rng(ctx, point, index).next_f64() * total;
            let mut chosen = &choices[choices.len() - 1];
            for choice in &choices {
                if target < choice.2 {
                    chosen = choice;
                    break;
                }
                target -= choice.2;
            }
            Instance {
                position: point.position.to_array(),
                rotation: point.rotation.to_array(),
                scale: point.scale.to_array(),
                kind: chosen.0,
                asset: chosen.1.clone(),
            }
        })
        .collect();
    Ok(BTreeMap::from([(
        "instances".to_owned(),
        Data::instances(instances),
    )]))
}
