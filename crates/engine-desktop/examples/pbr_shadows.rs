//! Authored PBR scene: cargo run -p engine-desktop --example pbr_shadows -- [--window|output.png]
use engine_desktop::{OffscreenSprites, RenderLimits, WgpuBackends};
use engine_project::{Project, Runtime};
fn main() -> anyhow::Result<()> {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/pbr-shadows/project.json");
    let runtime = Runtime::new(Project::load(project)?)?;
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "pbr-shadows.png".into());
    if output == "--window" {
        engine_desktop::run(runtime, Default::default())?;
    } else {
        let mut renderer =
            OffscreenSprites::wgpu(WgpuBackends::Primary, 960, 640, RenderLimits::default())?;
        renderer.render_scene(&runtime.extract_scene_frame()?)?;
        image::save_buffer(
            &output,
            &renderer.read_rgba()?,
            960,
            640,
            image::ColorType::Rgba8,
        )?;
        println!("{}: {output}", renderer.backend_description());
    }
    Ok(())
}
