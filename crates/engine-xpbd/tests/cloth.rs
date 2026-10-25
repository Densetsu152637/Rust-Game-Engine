mod common;

use common::{DT, centroid, max_speed, max_stretch, mean_fold_angle, proxy, run};
use engine_xpbd::{ClothDesc, ProxyShape, XpbdSettings, XpbdWorld};
use glam::{Quat, Vec3};

const SEGMENTS: u32 = 16;

struct Hanging {
    world: XpbdWorld,
    rest: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
}

/// A 1 m x 1 m horizontal cloth pinned at two adjacent corners. With `span < 1` the pin
/// targets move to `span` meters apart so the 1 m top edge has slack and must fold.
fn hanging_from_two_corners(span: f32, bend_compliance: f32, tether_slack: Option<f32>) -> Hanging {
    let mut desc = ClothDesc::grid(Vec3::ZERO, Vec3::X, Vec3::Z, SEGMENTS, SEGMENTS).unwrap();
    desc.params.bend_compliance = bend_compliance;
    desc.params.tether_slack = tether_slack;
    desc.params.damping = 2.0;
    desc.pinned = vec![0, SEGMENTS];
    let rest = desc.positions.clone();
    let triangles = desc.triangles.clone();
    let mut world = XpbdWorld::new(XpbdSettings::default(), 10_000).unwrap();
    world.insert_cloth(1, desc).unwrap();
    let cloth = world.cloth_mut(1).unwrap();
    let inset = 0.5 * (1.0 - span);
    cloth.set_pin_target(0, Vec3::new(inset, 0.0, 0.0)).unwrap();
    cloth
        .set_pin_target(SEGMENTS as usize, Vec3::new(1.0 - inset, 0.0, 0.0))
        .unwrap();
    run(&mut world, 6.0, &[]);
    Hanging {
        world,
        rest,
        triangles,
    }
}

#[test]
fn two_corner_pinned_cloth_reaches_steady_state_with_bounded_stretch_and_sag() {
    let untethered = hanging_from_two_corners(1.0, 1.0e3, None);
    let untethered_stretch = max_stretch(
        &untethered.rest,
        untethered.world.cloth(1).unwrap().positions(),
        &untethered.triangles,
    );
    let hanging = hanging_from_two_corners(1.0, 1.0e3, Some(1.0));
    let cloth = hanging.world.cloth(1).unwrap();
    let positions = cloth.positions();
    let stretch = max_stretch(&hanging.rest, positions, &hanging.triangles);
    let top_sag = -positions[SEGMENTS as usize / 2].y;
    // The side columns hang straight down from the pins, so the lowest point sits one
    // cloth length (1 m, stretched by at most `stretch`) below them.
    let lowest = -positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let speed = max_speed(cloth.velocities());
    println!(
        "two-corner cloth (compliance 0, 10 substeps): max stretch {stretch:.5} with tethers, {untethered_stretch:.5} without; top-edge sag {top_sag:.5} m, lowest {lowest:.5} m, max speed {speed:.5} m/s"
    );
    assert!(stretch < 0.02, "max stretch {stretch}");
    assert!(
        untethered_stretch > stretch,
        "tethers should reduce stretch"
    );
    assert!(
        (0.995..=1.0 + stretch + 2.0e-3).contains(&lowest),
        "lowest {lowest}"
    );
    assert!((0.0..0.05).contains(&top_sag), "top sag {top_sag}");
    assert!(speed < 0.02, "not at rest: {speed}");
    // Steady: one more second changes nothing measurable.
    let before = positions.to_vec();
    let mut world = hanging.world;
    run(&mut world, 1.0, &[]);
    let drift = before
        .iter()
        .zip(world.cloth(1).unwrap().positions())
        .map(|(a, b)| a.distance(*b))
        .fold(0.0, f32::max);
    println!("two-corner cloth: drift over 1 s {drift:.6} m");
    assert!(drift < 2.0e-3, "drift {drift}");
}

#[test]
fn stiffer_bending_gives_less_curvature() {
    // Pins 0.8 m apart force the 1 m top edge to fold; bending stiffness decides how.
    let stiff = hanging_from_two_corners(0.8, 0.0, Some(1.0));
    let soft = hanging_from_two_corners(0.8, 1.0e4, Some(1.0));
    let stiff_fold = mean_fold_angle(stiff.world.cloth(1).unwrap().positions(), &stiff.triangles);
    let soft_fold = mean_fold_angle(soft.world.cloth(1).unwrap().positions(), &soft.triangles);
    println!(
        "mean fold angle: bend compliance 0 -> {stiff_fold:.5} rad, 1e4 -> {soft_fold:.5} rad"
    );
    assert!(
        stiff_fold < 0.5 * soft_fold,
        "stiff {stiff_fold} soft {soft_fold}"
    );
}

#[test]
fn cloth_drapes_on_sphere_without_penetration() {
    let mut desc = ClothDesc::grid(
        Vec3::new(-0.6, 0.4, -0.6),
        Vec3::X * 1.2,
        Vec3::Z * 1.2,
        20,
        20,
    )
    .unwrap();
    desc.params.thickness = 0.01;
    desc.params.damping = 1.0;
    let mut world = XpbdWorld::new(XpbdSettings::default(), 10_000).unwrap();
    world.insert_cloth(3, desc).unwrap();
    let radius = 0.3;
    let sphere = proxy(
        9,
        Vec3::ZERO,
        Quat::IDENTITY,
        ProxyShape::Sphere { radius },
        0.5,
    );
    let mut deepest = f32::INFINITY;
    for _ in 0..(3.0 / DT) as usize {
        world.step(DT, &[sphere]).unwrap();
        let positions = world.cloth(3).unwrap().positions();
        let clearance = positions
            .iter()
            .map(|p| p.length() - radius)
            .fold(f32::INFINITY, f32::min);
        deepest = deepest.min(clearance);
    }
    let positions = world.cloth(3).unwrap().positions();
    let center = positions[10 * 21 + 10];
    let corner_y = positions[0].y;
    println!(
        "sphere drape: min clearance over run {deepest:.5} m (thickness 0.01), center y {:.4}, corner y {corner_y:.4}",
        center.y
    );
    // Penetration below the sphere surface stays under 1 mm at every frame.
    assert!(deepest > -1.0e-3, "penetration {deepest}");
    // The cloth rests on top of the sphere and its corners hang below the equator.
    assert!(
        (center.y - (radius + 0.01)).abs() < 5.0e-3,
        "center {center}"
    );
    assert!(corner_y < 0.0, "corner {corner_y}");
    assert!(!world.proxy_impulses().is_empty());
}

/// Slides a small cloth patch on a box tilted by 20 degrees and returns the downhill
/// displacement of its centroid after 0.5 s.
fn slide_on_tilted_box(friction: f32) -> f32 {
    let rotation = Quat::from_rotation_z(-20f32.to_radians());
    let normal = rotation * Vec3::Y;
    let tangent = rotation * Vec3::X;
    let half_thickness = 0.1;
    let center = normal * (half_thickness + 0.012);
    let mut desc = ClothDesc::grid(
        center - tangent * 0.15 - Vec3::Z * 0.15,
        tangent * 0.3,
        Vec3::Z * 0.3,
        8,
        8,
    )
    .unwrap();
    desc.params.friction = friction;
    desc.params.damping = 0.0;
    let settings = XpbdSettings {
        air_density: 0.0,
        ..XpbdSettings::default()
    };
    let mut world = XpbdWorld::new(settings, 1_000).unwrap();
    world.insert_cloth(1, desc).unwrap();
    let start = centroid(world.cloth(1).unwrap().positions());
    let slab = proxy(
        2,
        Vec3::ZERO,
        rotation,
        ProxyShape::Box {
            half_extents: Vec3::new(1.0, half_thickness, 1.0),
        },
        friction,
    );
    run(&mut world, 0.5, &[slab]);
    let gravity = settings.gravity;
    let downhill = (gravity - normal * normal.dot(gravity)).normalize();
    (centroid(world.cloth(1).unwrap().positions()) - start).dot(downhill)
}

#[test]
fn friction_holds_cloth_on_tilted_box_and_zero_friction_slides() {
    let held = slide_on_tilted_box(0.8);
    let slid = slide_on_tilted_box(0.0);
    // Frictionless sliding: 0.5 * g * sin(20 deg) * 0.5^2 = 0.42 m.
    println!("tilted box: friction 0.8 moved {held:.5} m, friction 0 moved {slid:.4} m");
    assert!(held.abs() < 5.0e-3, "held cloth slid {held}");
    assert!(slid > 0.3, "frictionless cloth slid only {slid}");
}

fn hanging_vertical(wind: Vec3) -> (XpbdWorld, Vec<Vec3>) {
    let mut desc = ClothDesc::grid(Vec3::ZERO, Vec3::X, Vec3::NEG_Y, SEGMENTS, SEGMENTS).unwrap();
    desc.pinned = vec![0, SEGMENTS];
    let rest = desc.positions.clone();
    let settings = XpbdSettings {
        wind,
        ..XpbdSettings::default()
    };
    let mut world = XpbdWorld::new(settings, 10_000).unwrap();
    world.insert_cloth(1, desc).unwrap();
    run(&mut world, 3.0, &[]);
    (world, rest)
}

#[test]
fn wind_displaces_hanging_cloth_and_calm_air_keeps_it_symmetric() {
    let (windy, _) = hanging_vertical(Vec3::new(0.0, 0.0, 5.0));
    let windy_mean_z = centroid(windy.cloth(1).unwrap().positions()).z;
    let (calm, _) = hanging_vertical(Vec3::ZERO);
    let positions = calm.cloth(1).unwrap().positions();
    let out_of_plane = positions.iter().map(|p| p.z.abs()).fold(0.0, f32::max);
    let columns = SEGMENTS as usize + 1;
    let mut asymmetry = 0.0f32;
    for j in 0..columns {
        for i in 0..columns {
            let a = positions[j * columns + i];
            let b = positions[j * columns + (columns - 1 - i)];
            asymmetry = asymmetry
                .max((a.x + b.x - 1.0).abs())
                .max((a.y - b.y).abs());
        }
    }
    println!(
        "wind: mean z with 5 m/s wind {windy_mean_z:.4} m; calm max |z| {out_of_plane:e} m, mirror asymmetry {asymmetry:e} m"
    );
    assert!(windy_mean_z > 0.1, "wind displacement {windy_mean_z}");
    assert!(
        out_of_plane < 1.0e-6,
        "calm cloth left its plane: {out_of_plane}"
    );
    // Gauss-Seidel ordering is not mirror symmetric; it leaves well under 0.5% of the width.
    assert!(asymmetry < 5.0e-3, "calm cloth asymmetric: {asymmetry}");
}

#[test]
fn moving_pin_target_moves_particle_exactly_and_drags_neighbours() {
    let mut desc = ClothDesc::grid(Vec3::ZERO, Vec3::X, Vec3::Z, 8, 8).unwrap();
    desc.pinned = vec![0];
    let settings = XpbdSettings {
        gravity: Vec3::ZERO,
        ..XpbdSettings::default()
    };
    let mut world = XpbdWorld::new(settings, 1_000).unwrap();
    world.insert_cloth(1, desc).unwrap();
    let before = world.cloth(1).unwrap().positions().to_vec();
    let target = Vec3::new(-0.3, 0.05, 0.0);
    world
        .cloth_mut(1)
        .unwrap()
        .set_pin_target(0, target)
        .unwrap();
    world.step(DT, &[]).unwrap();
    let after = world.cloth(1).unwrap().positions();
    assert_eq!(after[0], target);
    let right = after[1] - before[1];
    let down = after[9] - before[9];
    println!("pin drag: neighbour displacements {right} and {down}");
    assert!(
        right.x < -0.1 && down.x < -0.1,
        "neighbours not dragged: {right} {down}"
    );
    // Follow a moving bone: the pinned particle lands on every target exactly.
    for frame in 0..60 {
        let t = frame as f32 * DT;
        let target = Vec3::new(0.2 * (3.0 * t).cos(), 0.1 * (2.0 * t).sin(), 0.05 * t);
        let cloth = world.cloth_mut(1).unwrap();
        let previous = cloth.positions()[0];
        cloth.set_pin_target(0, target).unwrap();
        world.step(DT, &[]).unwrap();
        let cloth = world.cloth(1).unwrap();
        assert_eq!(cloth.positions()[0], target);
        let expected_velocity = (target - previous) / DT;
        assert!(cloth.velocities()[0].abs_diff_eq(
            expected_velocity,
            1.0e-2 * (1.0 + expected_velocity.length())
        ));
    }
    // Unpinning releases the particle.
    world.cloth_mut(1).unwrap().unpin(0).unwrap();
    assert!(world.cloth(1).unwrap().pin_target(0).is_none());
}

#[test]
fn tethers_bound_stretch_of_a_stretchy_cloth() {
    let corner_drop = |tether: Option<f32>| {
        let mut desc =
            ClothDesc::grid(Vec3::ZERO, Vec3::X, Vec3::NEG_Y, SEGMENTS, SEGMENTS).unwrap();
        desc.pinned = vec![0, SEGMENTS];
        desc.params.stretch_compliance = 2.0e-2;
        desc.params.tether_slack = tether;
        desc.params.damping = 2.0;
        let mut world = XpbdWorld::new(XpbdSettings::default(), 10_000).unwrap();
        world.insert_cloth(1, desc).unwrap();
        run(&mut world, 3.0, &[]);
        let cloth = world.cloth(1).unwrap();
        assert!(cloth.constraint_counts().2 > 0 || tether.is_none());
        let bottom_left = SEGMENTS as usize * (SEGMENTS as usize + 1);
        // Rest geodesic distance from the bottom-left corner to its pin is exactly 1 m.
        cloth.positions()[bottom_left].distance(cloth.positions()[0])
    };
    let free = corner_drop(None);
    let tethered = corner_drop(Some(1.0));
    println!("tethers: bottom corner distance to pin without {free:.4} m, with {tethered:.4} m");
    assert!(free > 1.03, "stretchy cloth should stretch: {free}");
    assert!(tethered <= 1.0 + 1.0e-3, "tether violated: {tethered}");
}

#[test]
fn flat_cloth_normals_follow_triangle_winding() {
    let desc = ClothDesc::grid(Vec3::ZERO, Vec3::X, Vec3::Z, 4, 4).unwrap();
    let mut world = XpbdWorld::new(XpbdSettings::default(), 100).unwrap();
    world.insert_cloth(1, desc).unwrap();
    let cloth = world.cloth(1).unwrap();
    assert_eq!(cloth.normals().len(), cloth.positions().len());
    assert!(
        cloth
            .normals()
            .iter()
            .all(|n| n.abs_diff_eq(Vec3::NEG_Y, 1.0e-6))
    );
}
