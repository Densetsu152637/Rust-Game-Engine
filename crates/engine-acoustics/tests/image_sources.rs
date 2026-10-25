//! Test 11: image-source counts, delays and occlusion in the shoebox.

use engine_acoustics::direct::ListenerFrame;
use engine_acoustics::image_source::{ImageSourceSettings, image_sources};
use engine_acoustics::reference::*;
use engine_acoustics::*;

const SOURCE: [f32; 3] = SHOEBOX_SOURCE;
const LISTENER: [f32; 3] = SHOEBOX_LISTENER;

fn frame() -> ListenerFrame {
    ListenerFrame::new(&ListenerPose {
        position: LISTENER,
        forward: [0.0, 0.0, -1.0],
        up: [0.0, 1.0, 0.0],
    })
    .unwrap()
}
fn settings(order: u8) -> ImageSourceSettings {
    ImageSourceSettings {
        order,
        er_rays: 256,
        max_paths: 64,
        speed_of_sound: 343.0,
        air_np_per_m: [0.0; 4],
    }
}
fn reflect(p: [f32; 3], wall: usize) -> [f32; 3] {
    let (axis, at) = (
        wall / 2,
        if wall.is_multiple_of(2) {
            0.0
        } else {
            SHOEBOX_DIMS_M[wall / 2]
        },
    );
    let mut q = p;
    q[axis] = 2.0 * at - p[axis];
    q
}
fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}
/// Analytic delays of the distinct images of exactly `order` reflections.
fn analytic_delays(order: usize) -> Vec<f32> {
    let mut images: Vec<[f32; 3]> = vec![];
    let mut push = |p: [f32; 3]| {
        if !images.iter().any(|q| dist(*q, p) < 1e-4) {
            images.push(p);
        }
    };
    for a in 0..6 {
        if order == 1 {
            push(reflect(SOURCE, a));
            continue;
        }
        for b in (0..6).filter(|&b| b != a) {
            push(reflect(reflect(SOURCE, a), b));
        }
    }
    let direct = dist(SOURCE, LISTENER);
    let mut d: Vec<f32> = images
        .iter()
        .map(|p| (dist(*p, LISTENER) - direct) / 343.0)
        .collect();
    d.sort_by(f32::total_cmp);
    d
}

#[test]
fn shoebox_yields_4n2_plus_2_paths_with_analytic_delays() {
    let world = reference_shoebox(0.0).unwrap();
    for order in 1..=2u8 {
        let paths = image_sources(&world, SOURCE, &frame(), &settings(order));
        let mut delays: Vec<f32> = paths
            .iter()
            .filter(|p| p.path.order == order)
            .map(|p| p.path.delay_s)
            .collect();
        delays.sort_by(f32::total_cmp);
        let expected = analytic_delays(usize::from(order));
        let n = usize::from(order);
        assert_eq!(expected.len(), 4 * n * n + 2);
        assert_eq!(delays.len(), 4 * n * n + 2, "order {order}");
        let worst = delays
            .iter()
            .zip(&expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        println!(
            "order {order}: {} paths, worst delay error {worst:e} s",
            delays.len()
        );
        assert!(worst < 1e-4);
        // Order 2 includes the order-1 paths.
        if order == 2 {
            assert_eq!(paths.len(), 24);
        }
        // Amplitudes: sqrt((1 - a)^order) / length.
        for p in &paths {
            let expected = (1.0f32 - 0.05).powi(i32::from(p.path.order)).sqrt() / p.path.length_m;
            assert!((p.path.gains[0] - expected).abs() < 1e-5);
        }
    }
}

#[test]
fn an_occluded_image_path_is_rejected() {
    // The first-order reflection off the floor (y = 0) from SOURCE to LISTENER hits
    // the floor where the path crosses it; a small panel just above that point
    // blocks only that path.
    let image = reflect(SOURCE, 2);
    let s = LISTENER[1] / (LISTENER[1] - image[1]);
    let r = [0, 1, 2].map(|k| LISTENER[k] + (image[k] - LISTENER[k]) * s);
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let wall = b
        .add_material(AcousticMaterial {
            absorption: SHOEBOX_ABSORPTION,
            scattering: 0.0,
            transmission: [0.0; 4],
        })
        .unwrap();
    let room = b
        .add_mesh(box_mesh([0.0; 3], SHOEBOX_DIMS_M, wall))
        .unwrap();
    b.add_instance(room, IDENTITY_AFFINE, Mobility::Static, 1)
        .unwrap();
    let panel = b
        .add_mesh(box_mesh(
            [r[0] - 0.6, 0.3, r[2] - 0.6],
            [r[0] + 0.6, 0.32, r[2] + 0.6],
            wall,
        ))
        .unwrap();
    b.add_instance(panel, IDENTITY_AFFINE, Mobility::Static, 1)
        .unwrap();
    let world = b.build().unwrap();
    let open = image_sources(
        &reference_shoebox(0.0).unwrap(),
        SOURCE,
        &frame(),
        &settings(1),
    );
    let blocked = image_sources(&world, SOURCE, &frame(), &settings(1));
    let floor_delay = (dist(image, LISTENER) - dist(SOURCE, LISTENER)) / 343.0;
    assert_eq!(open.len(), 6);
    assert!(
        open.iter()
            .any(|p| (p.path.delay_s - floor_delay).abs() < 1e-4)
    );
    assert!(
        !blocked
            .iter()
            .any(|p| p.path.order == 1 && (p.path.delay_s - floor_delay).abs() < 1e-4),
        "floor path must be rejected"
    );
    // The other five walls still reflect (the panel itself may add its own paths).
    for wall in [0, 1, 3, 4, 5] {
        let d = (dist(reflect(SOURCE, wall), LISTENER) - dist(SOURCE, LISTENER)) / 343.0;
        assert!(
            blocked
                .iter()
                .any(|p| p.path.order == 1 && (p.path.delay_s - d).abs() < 1e-4),
            "wall {wall}"
        );
    }
}
