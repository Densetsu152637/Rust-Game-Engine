//! Run with `cargo run -p engine-navigation --example navmesh`.
use engine_navigation::{BakeInput, BakeSettings, NavPath, Obstacle, QuerySettings, bake};

fn main() -> anyhow::Result<()> {
    let mesh = bake(&BakeInput {
        vertices: vec![[0., 0., 0.], [10., 0., 0.], [10., 0., 10.], [0., 0., 10.]],
        triangles: vec![[0, 1, 2], [0, 2, 3]],
        obstacles: vec![Obstacle {
            min: [4., -1., 3.],
            max: [6., 3., 7.],
        }],
        settings: BakeSettings::default(),
    })?;
    match mesh.find_path([2., 0., 5.], [8., 0., 5.], QuerySettings::default())? {
        NavPath::Reached { points, polygons } => {
            println!(
                "Baked {} polygons; route crosses {polygons}:",
                mesh.polygon_count()
            );
            for point in points {
                println!("  {point:?}");
            }
        }
        result => anyhow::bail!("route failed: {result:?}"),
    }
    Ok(())
}
