//! Native SpriteLit commands submitted through the existing real wgpu backend.
//! No desktop adapter or Runtime World tilemap acceptance is claimed here.
use anyhow::{Result, ensure};
use engine_render::*;
use engine_render_pipeline::sprite_lighting::*;
use engine_render_pipeline::{
    EngineVertex, LIT_WGSL, Light, MaterialParams, MaterialTarget, compose_material,
};
use engine_render_wgpu::{GpuContext, WgpuBackend, WgpuOptions};
use glam::{Mat4, Vec3};

fn layout() -> VertexLayout {
    VertexLayout::new(
        48,
        [
            VertexAttribute {
                location: 0,
                format: VertexFormat::Float32x3,
                offset: 0,
            },
            VertexAttribute {
                location: 1,
                format: VertexFormat::Float32x2,
                offset: 12,
            },
            VertexAttribute {
                location: 2,
                format: VertexFormat::Float32x4,
                offset: 20,
            },
            VertexAttribute {
                location: 3,
                format: VertexFormat::Float32x3,
                offset: 36,
            },
        ],
    )
}
const UV: [[f32; 2]; 4] = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
const ORIENTED_UVS: [[usize; 4]; 8] = [
    [0, 1, 2, 3],
    [1, 0, 3, 2],
    [3, 2, 1, 0],
    [2, 3, 0, 1],
    [2, 1, 0, 3],
    [1, 2, 3, 0],
    [3, 0, 1, 2],
    [0, 3, 2, 1],
];
fn quad(uv: [[f32; 2]; 4]) -> Result<MeshResource> {
    let points = [
        [-1.0, -1.0, 0.5],
        [1.0, -1.0, 0.5],
        [1.0, 1.0, 0.5],
        [-1.0, 1.0, 0.5],
    ];
    let vertices: Vec<_> = points
        .into_iter()
        .zip(uv)
        .map(|(position, uv)| EngineVertex {
            position,
            uv,
            color: [1.0; 4],
            normal: [0.0; 3],
        })
        .collect();
    ensure!(
        std::mem::size_of::<EngineVertex>() == 48,
        "existing sprite vertex stride"
    );
    Ok(MeshResource::new(
        bytemuck::cast_slice(&vertices).to_vec(),
        layout(),
        vec![0, 1, 2, 0, 2, 3],
    )?)
}
struct Harness {
    device: RenderDevice<WgpuBackend>,
    gpu: GpuContext,
    target: TargetHandle,
    mesh: MeshHandle,
    lit: ShaderHandle,
    legacy: ShaderHandle,
}
impl Harness {
    fn new() -> Result<Self> {
        let gpu = GpuContext::headless_with_options(&WgpuOptions::default())?;
        eprintln!("SpriteLit actual adapter: {:?}", gpu.adapter.get_info());
        check_limits(&gpu.device.limits())?;
        let mut device = RenderDevice::new(
            WgpuBackend::from_context(gpu.clone()),
            DeviceConfig {
                frames_in_flight: 2,
            },
        )?;
        let lit = device.create_shader(ShaderResource::new(
            "native SpriteLit",
            ShaderSource::wgsl(compose_material(
                SPRITE_LIT_WGSL,
                MaterialTarget::SpriteLit,
            )?),
        ))?;
        let legacy = device.create_shader(ShaderResource::new(
            "legacy Sprite",
            ShaderSource::wgsl(compose_material(LIT_WGSL, MaterialTarget::Sprite)?),
        ))?;
        let target = device.create_target(64, 64)?;
        let mesh = device.create_mesh(&quad(UV)?)?;
        Ok(Self {
            device,
            gpu,
            target,
            mesh,
            lit,
            legacy,
        })
    }
    fn texture(&mut self, rgba: [u8; 4]) -> Result<TextureHandle> {
        Ok(self
            .device
            .create_texture(&Texture2D::new(1, 1, rgba.to_vec())?)?)
    }
    fn uniforms(
        &self,
        lighting: &SpriteLighting,
        offsets: &[Vec3],
    ) -> Result<Vec<SpriteLightingUniform>> {
        lighting.prepare_draws(&self.gpu.device.limits(), offsets)
    }
    fn command(
        &self,
        light: Option<&SpriteLightingUniform>,
        params: MaterialParams,
        textures: [Option<TextureHandle>; 3],
    ) -> Result<DrawCommand> {
        let material = SpriteMaterialUniforms::new(
            params,
            SpriteTextureMaps {
                metallic_roughness: textures[1].is_some(),
                normal: textures[2].is_some(),
            },
        )?;
        let mut draw = DrawCommand::new(
            if light.is_some() {
                self.lit
            } else {
                self.legacy
            },
            self.mesh,
        )
        .with_state(DrawState {
            blend: BlendMode::StraightAlpha,
            depth_test: false,
            depth_write: false,
            cull: CullMode::None,
            scissor: None,
        })
        // The matrix is UNSHIFTED; offset belongs to SpriteLit's model helper.
        .with_uniform(DrawUniform::from_pod(
            "clip",
            UniformSlot::new(0, 0),
            &Mat4::from_scale(Vec3::new(0.5, 0.5, 1.0)).to_cols_array(),
        ))
        .with_uniform(DrawUniform::from_pod(
            "material",
            UniformSlot::new(2, 0),
            material.material(),
        ))
        .with_uniform(DrawUniform::from_pod(
            "maps",
            UniformSlot::new(2, 2),
            material.maps(),
        ));
        if let Some(light) = light {
            draw.uniforms.push(DrawUniform::from_pod(
                "SpriteLit",
                UniformSlot::new(2, 1),
                light,
            ));
        }
        draw.texture = textures[0];
        draw.data_textures = [textures[1], textures[2]];
        Ok(draw)
    }
    fn render(&mut self, draws: Vec<DrawCommand>) -> Result<ReadbackImage> {
        let count = draws.len();
        let mut commands = self.device.make_frame();
        for draw in draws {
            commands.record(draw)?;
        }
        let outcome = self.device.submit(
            FrameDesc::new(RenderTarget::Offscreen(self.target)),
            commands,
        )?;
        match outcome {
            FrameOutcome::Rendered(stats) => {
                ensure!(stats.draws == count, "all native draws must submit")
            }
            FrameOutcome::Skipped(reason) => anyhow::bail!("offscreen draw skipped: {reason:?}"),
        };
        let image = self.device.read_target(self.target)?;
        self.gpu.check_errors()?;
        Ok(image)
    }
}
fn lighting(ambient: Vec3, lights: Vec<Light>) -> SpriteLighting {
    SpriteLighting::new(
        ambient,
        lights
            .into_iter()
            .enumerate()
            .map(|(i, l)| SpriteLight::new(format!("light{i}"), l).unwrap())
            .collect(),
    )
    .unwrap()
}
fn params() -> MaterialParams {
    MaterialParams {
        roughness: 1.0,
        ..Default::default()
    }
}
fn encode(value: f64) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}
fn close(pixel: [u8; 4], linear: [f64; 3]) {
    let expected = linear.map(encode);
    for c in 0..3 {
        assert!(
            (i16::from(pixel[c]) - i16::from(expected[c])).abs() <= 2,
            "{pixel:?} vs {expected:?} ({linear:?})"
        );
    }
    assert_eq!(pixel[3], 255);
}
// Independent scalar f64 evaluation of the existing engine BRDF at a point,
// including floors. GPU reference inputs are decoded from authored bytes.
fn reference(
    albedo: [f64; 3],
    metal: f64,
    rough: f64,
    n: [f64; 3],
    l: [f64; 3],
    incident: [f64; 3],
) -> [f64; 3] {
    let nv = n[2].max(1e-4);
    let nl = (n[0] * l[0] + n[1] * l[1] + n[2] * l[2]).max(0.0);
    let sum = [l[0], l[1], l[2] + 1.0];
    let len = (sum.iter().map(|v| v * v).sum::<f64>()).sqrt().max(1e-6);
    let h = sum.map(|v| v / len);
    let nh = (n[0] * h[0] + n[1] * h[1] + n[2] * h[2]).max(0.0);
    let vh = h[2].max(0.0);
    let a2 = rough.max(0.045).powi(4);
    let denom = nh * nh * (a2 - 1.0) + 1.0;
    let d = a2 / (std::f64::consts::PI * denom * denom).max(1e-7);
    let gv = 2.0 * nv / (nv + (a2 + (1.0 - a2) * nv * nv).sqrt());
    let gl = 2.0 * nl / (nl + (a2 + (1.0 - a2) * nl * nl).sqrt()).max(1e-6);
    std::array::from_fn(|c| {
        let f0 = 0.04 * (1.0 - metal) + albedo[c] * metal;
        let f = f0 + (1.0 - f0) * (1.0 - vh).clamp(0.0, 1.0).powi(5);
        let diffuse = (1.0 - f) * (1.0 - metal) * albedo[c] / std::f64::consts::PI;
        let spec = d * gv * gl * f / (4.0 * nv * nl).max(1e-6);
        (diffuse + spec) * incident[c] * nl
    })
}
fn unit(v: [f64; 3]) -> [f64; 3] {
    let len = v.iter().map(|v| v * v).sum::<f64>().sqrt();
    v.map(|v| v / len)
}
fn world_at(image: &ReadbackImage, x: u32, y: u32) -> [f64; 3] {
    [
        4.0 * (f64::from(x) + 0.5) / f64::from(image.width) - 2.0,
        2.0 - 4.0 * (f64::from(y) + 0.5) / f64::from(image.height),
        0.5,
    ]
}
fn point_reference(world: [f64; 3], position: [f64; 3], range: f64, color: [f64; 3]) -> [f64; 3] {
    let delta = std::array::from_fn::<_, 3, _>(|c| position[c] - world[c]);
    let d = delta.iter().map(|v| v * v).sum::<f64>().sqrt();
    let window = (1.0 - (d / range).powi(4)).clamp(0.0, 1.0).powi(2);
    let falloff = window / (d * d).max(1e-4);
    reference(
        [1.0; 3],
        0.0,
        1.0,
        [0.0, 0.0, 1.0],
        unit(delta),
        color.map(|c| c * falloff),
    )
}

#[test]
#[ignore = "requires a real wgpu adapter; native directional/point material pixels"]
fn directional_point_and_material_response_reach_native_sprite_pixels() -> Result<()> {
    let mut h = Harness::new()?;
    let sun = lighting(
        Vec3::ZERO,
        vec![Light::directional(
            Vec3::NEG_Z,
            Vec3::new(1.0, 0.5, 0.25),
            1.0,
        )],
    );
    let u = h.uniforms(&sun, &[Vec3::ZERO])?;
    let image = h.render(vec![h.command(Some(&u[0]), params(), [None; 3])?])?;
    close(
        image.pixel(32, 32).unwrap(),
        [1.0, 0.5, 0.25].map(|c| c * 0.97 / std::f64::consts::PI),
    );
    let point = lighting(
        Vec3::ZERO,
        vec![Light::point(
            Vec3::new(0.0, 0.0, 2.5),
            Vec3::new(0.2, 0.4, 0.8),
            1.0,
            4.0,
        )],
    );
    let u = h.uniforms(&point, &[Vec3::ZERO])?;
    let image = h.render(vec![h.command(Some(&u[0]), params(), [None; 3])?])?;
    for (x, y) in [(32, 32), (44, 32)] {
        close(
            image.pixel(x, y).unwrap(),
            point_reference(
                world_at(&image, x, y),
                [0.0, 0.0, 2.5],
                4.0,
                [0.2, 0.4, 0.8],
            ),
        );
    }
    let outside = lighting(
        Vec3::ZERO,
        vec![Light::point(Vec3::new(0.0, 0.0, 2.5), Vec3::ONE, 1.0, 1.0)],
    );
    let u = h.uniforms(&outside, &[Vec3::ZERO])?;
    let dark = h.render(vec![h.command(Some(&u[0]), params(), [None; 3])?])?;
    close(dark.pixel(32, 32).unwrap(), [0.0; 3]);
    let sun = lighting(
        Vec3::ZERO,
        vec![Light::directional(Vec3::NEG_Z, Vec3::ONE, 0.5)],
    );
    let u = h.uniforms(&sun, &[Vec3::ZERO])?;
    let surface = MaterialParams {
        tint: [0.8, 0.4, 0.2, 1.0],
        metallic: 1.0,
        ..params()
    };
    let baseline = h.render(vec![h.command(Some(&u[0]), surface, [None; 3])?])?;
    let mr = h.texture([0, 128, 255, 255])?;
    let mapped = h.render(vec![h.command(
        Some(&u[0]),
        surface,
        [None, Some(mr), None],
    )?])?;
    close(
        mapped.pixel(32, 32).unwrap(),
        reference(
            [0.8, 0.4, 0.2],
            1.0,
            128.0 / 255.0,
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            [0.5; 3],
        ),
    );
    assert_ne!(
        baseline.pixel(32, 32),
        mapped.pixel(32, 32),
        "actual linear MR texture must change shading"
    );
    Ok(())
}

#[test]
#[ignore = "requires a real wgpu adapter; normal-map linear binding and atlas orientation"]
fn normal_data_texture_and_oriented_uvs_change_lighting_without_geometry_changes() -> Result<()> {
    let mut h = Harness::new()?;
    let toward = unit([0.5, 0.3, 1.0]);
    let sun = lighting(
        Vec3::ZERO,
        vec![Light::directional(
            -Vec3::new(0.5, 0.3, 1.0).normalize(),
            Vec3::ONE,
            0.5,
        )],
    );
    let u = h.uniforms(&sun, &[Vec3::ZERO])?;
    let normal = h.texture([255, 128, 255, 255])?;
    // Explicit fixture permutations in World flag order (X,Y,transpose bits).
    // The native renderer consumes UVs, never flags or a second tile format.
    let tangents = [
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    let bitangents = [
        [0.0, -1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
    ];
    for i in 0..8 {
        let mesh = h
            .device
            .create_mesh(&quad(ORIENTED_UVS[i].map(|v| UV[v]))?)?;
        let mut draw = h.command(Some(&u[0]), params(), [None, None, Some(normal)])?;
        draw.mesh = mesh;
        let image = h.render(vec![draw])?;
        let n = unit(std::array::from_fn(|c| {
            tangents[i][c] + bitangents[i][c] * (1.0 / 255.0) + if c == 2 { 1.0 } else { 0.0 }
        }));
        close(
            image.pixel(32, 32).unwrap(),
            reference([1.0; 3], 0.0, 1.0, n, toward, [0.5; 3]),
        );
        h.device.destroy(mesh)?;
    }
    let opposite = h.texture([0, 128, 255, 255])?;
    let a = h.render(vec![h.command(
        Some(&u[0]),
        params(),
        [None, None, Some(normal)],
    )?])?;
    let b = h.render(vec![h.command(
        Some(&u[0]),
        params(),
        [None, None, Some(opposite)],
    )?])?;
    assert_ne!(
        a.pixel(32, 32),
        b.pixel(32, 32),
        "data-texture identity belongs in the backend binding cache"
    );
    Ok(())
}

#[test]
#[ignore = "requires a real wgpu adapter; base atlas orientation through native lit draw submission"]
fn native_atlas_uvs_remain_v_down_for_all_eight_orientations() -> Result<()> {
    let mut h = Harness::new()?;
    let atlas = h.device.create_texture(&Texture2D::new(
        2,
        2,
        vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ],
    )?)?;
    let lighting = lighting(Vec3::ONE, vec![]);
    let u = h.uniforms(&lighting, &[Vec3::ZERO])?;
    let expected = [
        [0, 1, 3, 2],
        [1, 0, 2, 3],
        [2, 3, 1, 0],
        [3, 2, 0, 1],
        [0, 2, 3, 1],
        [2, 0, 1, 3],
        [1, 3, 2, 0],
        [3, 1, 0, 2],
    ];
    let colors = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255; 4],
    ];
    for i in 0..8 {
        let mesh = h
            .device
            .create_mesh(&quad(ORIENTED_UVS[i].map(|v| UV[v]))?)?;
        let mut draw = h.command(Some(&u[0]), params(), [Some(atlas), None, None])?;
        draw.mesh = mesh;
        let image = h.render(vec![draw])?;
        for (corner, (x, y)) in [(20, 20), (44, 20), (44, 44), (20, 44)]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                image.pixel(x, y).unwrap(),
                colors[expected[i][corner]],
                "orientation {i},corner {corner}"
            );
        }
        h.device.destroy(mesh)?;
    }
    Ok(())
}

#[test]
#[ignore = "requires a real wgpu adapter; straight alpha/painter order and legacy coexistence"]
fn lit_and_legacy_draws_keep_serial_order_blending_and_scissor() -> Result<()> {
    let mut h = Harness::new()?;
    let lighting = lighting(Vec3::ONE, vec![]);
    let u = h.uniforms(&lighting, &[Vec3::ZERO])?;
    let red = MaterialParams {
        tint: [1.0, 0.0, 0.0, 0.5],
        ..params()
    };
    let green = MaterialParams {
        tint: [0.0, 1.0, 0.0, 0.5],
        unlit: true,
        ..params()
    };
    let blue = MaterialParams {
        tint: [0.0, 0.0, 1.0, 0.5],
        ..params()
    };
    let a = h.command(Some(&u[0]), red, [None; 3])?;
    let ui = h.command(None, green, [None; 3])?;
    let mut b = h.command(Some(&u[0]), blue, [None; 3])?;
    b.state.scissor = Some(PixelRect::new(32, 0, 32, 64));
    let image = h.render(vec![a, ui, b])?;
    close(image.pixel(20, 32).unwrap(), [0.25, 0.5, 0.0]);
    close(image.pixel(40, 32).unwrap(), [0.125, 0.25, 0.5]);
    Ok(())
}

#[test]
#[ignore = "requires a real wgpu adapter; one offset, per-draw uniforms and replacement"]
fn offset_is_applied_once_and_native_snapshots_do_not_leak_between_draws() -> Result<()> {
    let mut h = Harness::new()?;
    let point = lighting(
        Vec3::ZERO,
        vec![Light::point(Vec3::new(1.0, 0.0, 2.5), Vec3::ONE, 1.0, 4.0)],
    );
    let u = h.uniforms(&point, &[Vec3::X])?;
    let image = h.render(vec![h.command(Some(&u[0]), params(), [None; 3])?])?;
    close(image.pixel(20, 32).unwrap(), [0.0; 3]);
    for x in [36, 48] {
        close(
            image.pixel(x, 32).unwrap(),
            point_reference(world_at(&image, x, 32), [1.0, 0.0, 2.5], 4.0, [1.0; 3]),
        );
    }
    let red = lighting(Vec3::new(0.5, 0.0, 0.0), vec![]);
    let blue = lighting(Vec3::new(0.0, 0.0, 0.5), vec![]);
    let r = h.uniforms(&red, &[Vec3::NEG_X])?;
    let b = h.uniforms(&blue, &[Vec3::X])?;
    let image = h.render(vec![
        h.command(Some(&r[0]), params(), [None; 3])?,
        h.command(Some(&b[0]), params(), [None; 3])?,
    ])?;
    close(image.pixel(16, 32).unwrap(), [0.5, 0.0, 0.0]);
    close(image.pixel(48, 32).unwrap(), [0.0, 0.0, 0.5]);
    let old_target = h.target;
    h.target = h.device.create_target(96, 48)?;
    h.device.destroy(old_target)?;
    let green = lighting(Vec3::new(0.0, 0.5, 0.0), vec![]);
    let u = h.uniforms(&green, &[Vec3::ZERO])?;
    let image = h.render(vec![h.command(Some(&u[0]), params(), [None; 3])?])?;
    close(image.pixel(48, 24).unwrap(), [0.0, 0.5, 0.0]);
    let legacy = MaterialParams {
        unlit: true,
        tint: [0.25; 4],
        ..params()
    };
    let image = h.render(vec![h.command(None, legacy, [None; 3])?])?;
    close(image.pixel(48, 24).unwrap(), [0.0625; 3]);
    Ok(())
}

#[test]
#[ignore = "requires a real wgpu adapter; all admitted lights contribute, no silent truncation"]
fn sixty_four_admitted_lights_reach_the_shader_count() -> Result<()> {
    let mut h = Harness::new()?;
    let lights = (0..MAX_SPRITE_LIGHTS)
        .map(|_| Light::directional(Vec3::NEG_Z, Vec3::ONE, 1.0 / MAX_SPRITE_LIGHTS as f32))
        .collect();
    let lighting = lighting(Vec3::ZERO, lights);
    let u = h.uniforms(&lighting, &[Vec3::ZERO])?;
    let image = h.render(vec![h.command(Some(&u[0]), params(), [None; 3])?])?;
    close(
        image.pixel(32, 32).unwrap(),
        [0.97 / std::f64::consts::PI; 3],
    );
    Ok(())
}
