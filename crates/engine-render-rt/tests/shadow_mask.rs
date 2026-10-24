//! RT shadow mask against an analytic shadow: a 2 x 2 x 2 m box on a ground
//! plane under a 45-degree directional light. Ignored by default; run with
//! `cargo test -p engine-render-rt --test shadow_mask -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_render_rt::{
    RtDevice, ShadowMaskFormat, ShadowMaskKernel, ShadowMaskParams, ShadowMaskTargets,
};

const SIZE: u32 = 256;
/// The view covers x, z in [-8, 8]: 16 pixels per metre.
const PX: f32 = 16.0 / SIZE as f32;

fn world_x(i: u32) -> f32 {
    -8.0 + (i as f32 + 0.5) * PX
}

/// Visibility of every pixel, row-major (row = z, column = x).
fn render(rt: &RtDevice, format: ShadowMaskFormat) -> Vec<f32> {
    let mut scene = scene(rt);
    let plane = ground(8.0, 0.0);
    let block = cuboid([-1.0, 0.0, -1.0], [1.0, 2.0, 1.0]);
    let plane_id = scene.add_mesh(&plane.data()).unwrap();
    let block_id = scene.add_mesh(&block.data()).unwrap();
    scene
        .add_instance(&instance(plane_id, translation(0.0, 0.0, 0.0)))
        .unwrap();
    scene
        .add_instance(&instance(block_id, translation(0.0, 0.0, 0.0)))
        .unwrap();
    scene.commit().unwrap();

    // A top-down G-buffer of the ground plane. Row 0 is background (w = 0);
    // row 1 has normals facing away from the light.
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    for j in 0..SIZE {
        for i in 0..SIZE {
            positions.push([world_x(i), 0.0, world_x(j), if j == 0 { 0.0 } else { 1.0 }]);
            normals.push(if j == 1 {
                [0.0, -1.0, 0.0, 0.0]
            } else {
                [0.0, 1.0, 0.0, 0.0]
            });
        }
    }
    let position_texture = rgba32f_texture(rt, SIZE, SIZE, &positions);
    let normal_texture = rgba32f_texture(rt, SIZE, SIZE, &normals);
    let output = texture(
        rt,
        format.texture_format(),
        SIZE,
        SIZE,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
    );
    let kernel = ShadowMaskKernel::new(rt, format).expect("kernel");
    let mut encoder = rt.device().create_command_encoder(&Default::default());
    kernel
        .record(
            &mut encoder,
            &scene,
            &ShadowMaskTargets {
                world_position: &position_texture.create_view(&Default::default()),
                world_normal: &normal_texture.create_view(&Default::default()),
                output: &output.create_view(&Default::default()),
                width: SIZE,
                height: SIZE,
            },
            &ShadowMaskParams {
                direction_to_light: [1.0, 1.0, 0.0],
                normal_bias: 1e-3,
                ..Default::default()
            },
        )
        .unwrap();
    let start = std::time::Instant::now();
    submit(rt, encoder);
    println!("{format:?}: dispatch + wait {:?}", start.elapsed());
    match format {
        ShadowMaskFormat::R8Unorm => read_texture(rt, &output, 1)
            .iter()
            .map(|&v| f32::from(v) / 255.0)
            .collect(),
        ShadowMaskFormat::R16Float => {
            bytemuck::cast_slice::<u8, u16>(&read_texture(rt, &output, 2))
                .iter()
                .map(|&h| half_to_f32(h))
                .collect()
        }
        ShadowMaskFormat::R32Float => bytemuck::cast_slice(&read_texture(rt, &output, 4)).to_vec(),
    }
}

/// Analytic shadow of the box for the light direction (1, 1, 0): x in [-3, 1],
/// z in [-1, 1].
const SHADOW_X: (f32, f32) = (-3.0, 1.0);
const SHADOW_Z: (f32, f32) = (-1.0, 1.0);

fn check(format: ShadowMaskFormat, mask: &[f32]) {
    let at = |i: u32, j: u32| mask[(j * SIZE + i) as usize];
    let (mut max_shadowed, mut min_lit) = (0.0f32, 1.0f32);
    let margin = 2.0 * PX;
    for j in 2..SIZE {
        for i in 0..SIZE {
            let (x, z) = (world_x(i), world_x(j));
            let v = at(i, j);
            assert!(v.is_finite());
            let inside_x = x > SHADOW_X.0 + margin && x < SHADOW_X.1 - margin;
            let inside_z = z > SHADOW_Z.0 + margin && z < SHADOW_Z.1 - margin;
            let outside = x < SHADOW_X.0 - margin
                || x > SHADOW_X.1 + margin
                || z < SHADOW_Z.0 - margin
                || z > SHADOW_Z.1 + margin;
            if inside_x && inside_z {
                max_shadowed = max_shadowed.max(v);
            } else if outside {
                min_lit = min_lit.min(v);
            }
        }
    }
    // Edge positions: the lit/shadow boundary (midpoint between the last lit
    // and the first shadowed pixel) along the x edge at x = -3 for the rows of
    // z in [-0.75, 0.75], and along the z edges for x = -1.
    let mut max_edge_error_px = 0.0f32;
    for j in 0..SIZE {
        let z = world_x(j);
        if z.abs() > 0.75 {
            continue;
        }
        let first = (0..SIZE).find(|&i| at(i, j) < 0.5).expect("shadow in row");
        let edge = world_x(first) - 0.5 * PX;
        max_edge_error_px = max_edge_error_px.max((edge - SHADOW_X.0).abs() / PX);
    }
    let column = (0..SIZE).find(|&i| world_x(i) > -1.0).unwrap();
    let rows: Vec<u32> = (2..SIZE).filter(|&j| at(column, j) < 0.5).collect();
    let z_low = world_x(rows[0]) - 0.5 * PX;
    let z_high = world_x(*rows.last().unwrap()) + 0.5 * PX;
    max_edge_error_px = max_edge_error_px
        .max((z_low - SHADOW_Z.0).abs() / PX)
        .max((z_high - SHADOW_Z.1).abs() / PX);
    let shadowed_pixels = mask.iter().filter(|&&v| v < 0.5).count();
    // 4 m x 2 m of shadow at 256 px/m^2, plus the facing-away row.
    let analytic = (4.0 * 2.0 / (PX * PX)) as usize + SIZE as usize;
    println!(
        "{format:?}: max visibility in shadow {max_shadowed}, min visibility in light {min_lit}, \
         max edge error {max_edge_error_px:.3} px, shadowed pixels {shadowed_pixels} \
         (analytic {analytic}), background {} facing-away {}",
        at(10, 0),
        at(10, 1)
    );
    assert!(
        max_shadowed <= 0.01,
        "shadowed region visibility {max_shadowed}"
    );
    assert!(min_lit >= 0.99, "lit region visibility {min_lit}");
    assert!(
        max_edge_error_px <= 1.0,
        "edge error {max_edge_error_px} px"
    );
    assert_eq!(shadowed_pixels, analytic);
    assert_eq!(at(10, 0), 1.0, "background pixels are lit");
    assert_eq!(
        at(10, 1),
        0.0,
        "surfaces facing away from the light are shadowed"
    );
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn shadow_mask_matches_the_analytic_shadow() {
    let rt = open();
    for format in [
        ShadowMaskFormat::R8Unorm,
        ShadowMaskFormat::R16Float,
        ShadowMaskFormat::R32Float,
    ] {
        if !rt.supports_storage_format(format.texture_format()) {
            println!("{format:?}: not a storage format on this device; skipped");
            assert!(ShadowMaskKernel::new(&rt, format).is_err());
            continue;
        }
        check(format, &render(&rt, format));
    }
    assert!(rt.status().is_hardware());
}
