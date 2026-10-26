//! Host side of the 3D path: engine scene data to `engine-render-pipeline`
//! inputs (GPU meshes/textures, view data, light list) and CPU validation.

use super::RenderStats;
use super::cache::address;
use anyhow::{Context, Result, ensure};
use engine_project::content::{Mesh, Texture};
use engine_project::lights::{
    DirectionalShadows, LightInstance3D, LightShadows, SceneLight, ShadowFilterMode,
};
use engine_project::rendering3d::{
    CameraInstance3D, MeshDraw3D, Projection3D, SceneRenderFrame, StreamFrame, TextureStreamMode,
};
use engine_render_pipeline::{
    CascadeSettings, EngineVertex, FeatureOverrides, GpuContext, GpuMesh, GpuTexture, Light,
    LightList, LightShape, MaterialParams, MaterialTarget, SceneMesh, ShadowFilter, ShadowSettings,
    TextureStreams, ViewData, ViewId, compose_material, photometric, validate_params,
};
use glam::{Mat4, Vec3};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The built-in lit material (`engine::material` contract).
pub const LIT_WGSL: &str = engine_render_pipeline::LIT_WGSL;

pub fn camera_matrix(camera: &CameraInstance3D, aspect: f32) -> Result<Mat4> {
    let world = Mat4::from_cols_array_2d(&camera.world);
    ensure!(
        world.is_finite() && world.determinant().abs() > 1e-8,
        "camera transform is singular"
    );
    let matrix = camera_projection(camera, aspect)? * world.inverse();
    ensure!(matrix.is_finite(), "camera matrix overflow");
    Ok(matrix)
}

/// The view-to-clip projection of [`camera_matrix`] (depth 0..1, not inverted).
pub(crate) fn camera_projection(camera: &CameraInstance3D, aspect: f32) -> Result<Mat4> {
    camera.camera.validate()?;
    ensure!(aspect.is_finite() && aspect > 0.0, "invalid camera aspect");
    Ok(match camera.camera.projection {
        Projection3D::Perspective {
            fov_y_radians,
            near,
            far,
        } => glam::camera::rh::proj::directx::perspective(fov_y_radians, aspect, near, far),
        Projection3D::Orthographic { height, near, far } => {
            glam::camera::rh::proj::directx::orthographic(
                -height * aspect * 0.5,
                height * aspect * 0.5,
                -height * 0.5,
                height * 0.5,
                near,
                far,
            )
        }
    })
}

/// The active camera's view: projection aspect of the output (3D viewport)
/// size, `render` size targets, and the camera's authored feature overrides.
pub(crate) fn camera_view(
    camera: &CameraInstance3D,
    id: ViewId,
    render: [u32; 2],
    output: [u32; 2],
) -> Result<ViewData> {
    let projection = camera_projection(camera, output[0] as f32 / output[1] as f32)?;
    let world = Mat4::from_cols_array_2d(&camera.world);
    let mut view = ViewData::new(id, world, projection, render, output)?;
    view.ambient = Vec3::splat(camera.camera.lighting.ambient);
    view.features = FeatureOverrides(camera.camera.effective_render_features());
    Ok(view)
}

fn filter(
    mode: ShadowFilterMode,
    pcf_radius: u32,
    source_size: f32,
    max_radius: u32,
) -> ShadowFilter {
    match mode {
        ShadowFilterMode::Hard => ShadowFilter::Hard,
        ShadowFilterMode::Pcf => ShadowFilter::Pcf { radius: pcf_radius },
        ShadowFilterMode::Pcss => ShadowFilter::Pcss {
            source_size,
            max_radius,
        },
    }
}

fn local_shadow(s: &LightShadows) -> Option<ShadowSettings> {
    s.enabled.then(|| ShadowSettings {
        resolution: s.resolution,
        depth_bias: s.depth_bias,
        normal_bias: s.normal_bias,
        filter: filter(s.filter, s.pcf_radius, s.source_radius, s.pcss_max_radius),
        near: s.near,
        cascades: CascadeSettings::default(),
    })
}

fn directional_shadow(s: &DirectionalShadows) -> Option<ShadowSettings> {
    s.enabled.then(|| ShadowSettings {
        resolution: s.resolution,
        depth_bias: s.depth_bias,
        normal_bias: s.normal_bias,
        filter: filter(
            s.filter,
            s.pcf_radius,
            s.angular_diameter_radians,
            s.pcss_max_radius,
        ),
        near: 0.05,
        cascades: CascadeSettings {
            count: s.cascades,
            split_lambda: s.split_lambda,
            max_distance: s.max_distance,
            blend: s.cascade_blend,
            caster_extension: s.caster_extension,
        },
    })
}

/// One authored light in shading units (photometric value x `exposure`),
/// oriented by its world transform (local -Z forward, local X right).
pub(crate) fn scene_light(instance: &LightInstance3D, exposure: f32) -> Result<Light> {
    let world = Mat4::from_cols_array_2d(&instance.world);
    ensure!(
        world.is_finite(),
        "light {}: nonfinite transform",
        instance.id
    );
    let position = world.w_axis.truncate();
    let forward = world.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
    ensure!(
        forward != Vec3::ZERO,
        "light {}: degenerate transform",
        instance.id
    );
    let color = |c: [f32; 3]| Vec3::from_array(c);
    let light = match &instance.light {
        SceneLight::Directional(l) => {
            l.validate()?;
            Light {
                shadow: directional_shadow(&l.shadows),
                ..Light::directional(forward, color(l.color), l.lux * exposure)
            }
        }
        SceneLight::Point(l) => {
            l.validate()?;
            Light {
                direction: forward,
                shadow: local_shadow(&l.shadows),
                ..Light::point(
                    position,
                    color(l.color),
                    photometric::candela_from_lumens(l.lumens) * exposure,
                    l.range,
                )
            }
        }
        SceneLight::Spot(l) => {
            l.validate()?;
            Light {
                shadow: local_shadow(&l.shadows),
                ..Light::spot(
                    position,
                    forward,
                    color(l.color),
                    photometric::candela_from_lumens(l.lumens) * exposure,
                    l.range,
                    l.inner_angle_radians,
                    l.outer_angle_radians,
                )
            }
        }
        SceneLight::Area(l) => {
            l.validate()?;
            let right = world.transform_vector3(Vec3::X);
            let right = (right - forward * right.dot(forward)).normalize_or_zero();
            ensure!(
                right != Vec3::ZERO,
                "light {}: degenerate transform",
                instance.id
            );
            let luminance =
                photometric::area_luminance_from_lumens(l.lumens, l.width * l.height, l.two_sided);
            Light {
                shape: LightShape::Area {
                    width: l.width,
                    height: l.height,
                    right,
                    two_sided: l.two_sided,
                },
                color: color(l.color),
                intensity: luminance * exposure,
                position,
                direction: forward,
                range: l.range,
                shadow: local_shadow(&l.shadows),
            }
        }
    };
    light
        .validate()
        .with_context(|| format!("light {}", instance.id))?;
    Ok(light)
}

/// The frame's lights in shading units of its 3D camera's exposure (none
/// without a camera).
pub(crate) fn light_list(frame: &SceneRenderFrame) -> Result<LightList> {
    let Some(camera) = &frame.camera3d else {
        return Ok(LightList::default());
    };
    camera.camera.lighting.validate()?;
    let exposure = camera.camera.lighting.exposure;
    let lights = frame
        .lights
        .iter()
        .map(|instance| scene_light(instance, exposure))
        .collect::<Result<Vec<_>>>()?;
    let list = LightList { lights };
    list.validate()?;
    Ok(list)
}

pub(crate) fn material_params(draw: &MeshDraw3D) -> MaterialParams {
    let m = &draw.material;
    MaterialParams {
        tint: m.tint,
        unlit: m.unlit,
        metallic: m.metallic,
        roughness: m.roughness,
        normal_scale: m.normal_scale,
        receive_shadows: m.receive_shadows,
        cast_shadows: m.cast_shadows,
        transparent: m.transparent,
    }
}

/// CPU checks of a frame's 3D meshes (transforms, material values, material
/// shaders against the contract) without a GPU. Mesh and texture data are
/// checked by the sprite renderer's frame validation.
pub(crate) fn validate_meshes(frame: &SceneRenderFrame) -> Result<()> {
    if frame.meshes.is_empty() {
        return Ok(());
    }
    let camera = frame
        .camera3d
        .as_ref()
        .context("3D meshes require an active Camera3D")?;
    camera.camera.validate()?;
    let mut shaders = HashSet::new();
    for draw in &frame.meshes {
        let model = Mat4::from_cols_array_2d(&draw.model);
        ensure!(
            model.is_finite() && model.determinant().abs() > 1e-8,
            "nonfinite model transform"
        );
        draw.material.validate()?;
        validate_params(&material_params(draw))?;
        let source = draw.shader.as_deref().unwrap_or(LIT_WGSL);
        if shaders.insert(source) {
            compose_material(source, MaterialTarget::Mesh)
                .with_context(|| format!("material shader of mesh {}", draw.id))?;
        }
    }
    Ok(())
}

pub(crate) fn engine_vertices(mesh: &Mesh) -> Vec<EngineVertex> {
    mesh.vertices
        .iter()
        .enumerate()
        .map(|(index, v)| EngineVertex {
            position: v.position,
            uv: v.uv,
            color: v.color,
            normal: mesh.normals.get(index).copied().unwrap_or([0.0; 3]),
        })
        .collect()
}

/// Arc-keyed GPU uploads of the 3D path (the sprite cache's rules: the same
/// `Arc` means unchanged; entries retain their source so addresses are not
/// reused; entries absent from a frame are released). Stream textures map
/// to the renderer's persistent [`TextureStreams`] textures instead.
#[derive(Default)]
pub(crate) struct SceneUploads {
    meshes: HashMap<usize, (Arc<Mesh>, Option<Arc<GpuMesh>>)>,
    textures: HashMap<usize, (Arc<Texture>, Arc<GpuTexture>)>,
    pub streams: TextureStreams,
}

/// A frame's 3D inputs, borrowing the frame.
pub(crate) struct SceneInputs<'a> {
    pub meshes: Vec<SceneMesh<'a>>,
    pub stats: RenderStats,
}

impl SceneUploads {
    /// Upload what `frame` needs (after [`validate_meshes`] and the sprite
    /// frame validation) and build the pipeline's mesh list.
    pub fn sync<'a>(
        &mut self,
        gpu: &GpuContext,
        frame: &'a SceneRenderFrame,
    ) -> Result<SceneInputs<'a>> {
        let mut stats = RenderStats::default();
        let streams = self.sync_streams(gpu, &frame.streams)?;
        let mut live_meshes = HashSet::new();
        let mut live_textures = HashSet::new();
        let mut meshes = Vec::with_capacity(frame.meshes.len());
        for draw in &frame.meshes {
            let key = address(&draw.mesh);
            live_meshes.insert(key);
            let entry = match self.meshes.entry(key) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    let uploaded = if draw.mesh.vertices.is_empty() {
                        None
                    } else {
                        Some(GpuMesh::from_engine_vertices(
                            &gpu.device,
                            "engine mesh",
                            &engine_vertices(&draw.mesh),
                            &draw.mesh.indices,
                        )?)
                    };
                    stats.mesh_uploads += 1;
                    slot.insert((draw.mesh.clone(), uploaded))
                }
            };
            let Some(mesh) = entry.1.clone() else {
                continue;
            };
            let mut texture = |source: &Option<Arc<Texture>>| -> Result<Option<Arc<GpuTexture>>> {
                let Some(source) = source else {
                    return Ok(None);
                };
                let key = address(source);
                if let Some(stream) = streams.get(&key) {
                    return Ok(Some(stream.clone()));
                }
                live_textures.insert(key);
                if let Some((_, uploaded)) = self.textures.get(&key) {
                    return Ok(Some(uploaded.clone()));
                }
                let uploaded = GpuTexture::rgba8(
                    &gpu.device,
                    &gpu.queue,
                    "engine texture",
                    [source.width, source.height],
                    &source.pixels,
                )?;
                self.textures
                    .insert(key, (source.clone(), uploaded.clone()));
                stats.texture_uploads += 1;
                Ok(Some(uploaded))
            };
            let base_color = texture(&draw.texture)?;
            let metallic_roughness = texture(&draw.metallic_roughness_texture)?;
            let normal_map = texture(&draw.normal_texture)?;
            meshes.push(SceneMesh {
                id: &draw.id,
                mesh,
                world_from_local: Mat4::from_cols_array_2d(&draw.model),
                shader: draw.shader.as_deref(),
                params: material_params(draw),
                base_color,
                metallic_roughness,
                normal_map,
            });
        }
        self.meshes.retain(|key, _| live_meshes.contains(key));
        self.textures.retain(|key, _| live_textures.contains(key));
        stats.cached_meshes = self.meshes.len();
        stats.cached_textures = self.textures.len();
        Ok(SceneInputs { meshes, stats })
    }

    /// Upload stream frames; returns the stream textures by the address of
    /// the frame `Arc` draws reference.
    fn sync_streams(
        &mut self,
        gpu: &GpuContext,
        frames: &[StreamFrame],
    ) -> Result<HashMap<usize, Arc<GpuTexture>>> {
        let mut by_address = HashMap::new();
        for frame in frames {
            let texture = match frame.mode {
                TextureStreamMode::CpuUpload => self.streams.sync_cpu(
                    &gpu.device,
                    &gpu.queue,
                    &frame.name,
                    frame.size,
                    frame.revision,
                    &frame.texture.pixels,
                )?,
                TextureStreamMode::External => {
                    self.streams
                        .external(&gpu.device, &frame.name, frame.size)?
                }
            };
            by_address.insert(address(&frame.texture), texture);
        }
        Ok(by_address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::rendering3d::Camera3D;

    #[test]
    fn camera_views_carry_projection_ambient_and_overrides() {
        let mut camera = CameraInstance3D {
            camera: Camera3D::default(),
            world: Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0)).to_cols_array_2d(),
        };
        camera
            .camera
            .render_features
            .insert("engine.opaque".into(), false);
        let view = camera_view(&camera, ViewId::MAIN, [320, 180], [640, 360]).unwrap();
        let expected = camera_matrix(&camera, 640.0 / 360.0).unwrap();
        assert!(view.clip_from_world().abs_diff_eq(expected, 1e-5));
        assert_eq!(view.ambient, Vec3::splat(0.08));
        assert_eq!(view.features.get("engine.opaque"), Some(false));
        assert_eq!(view.render_size, [320, 180]);
    }

    #[test]
    fn authored_lights_become_exposed_pipeline_lights() {
        use engine_project::lights::*;
        use engine_render_pipeline::ShadowFilter;
        let rotation = Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        let at = |position: Vec3| (Mat4::from_translation(position) * rotation).to_cols_array_2d();
        let mut sun = DirectionalLight::default();
        sun.shadows.enabled = true;
        sun.shadows.cascades = 2;
        let mut spot = SpotLight {
            lumens: 4.0 * std::f32::consts::PI,
            ..Default::default()
        };
        spot.shadows.enabled = true;
        spot.shadows.filter = ShadowFilterMode::Pcss;
        let mut frame = SceneRenderFrame::from(engine_project::runtime::RenderFrame {
            camera: Default::default(),
            draws: vec![],
        });
        frame.camera3d = Some(CameraInstance3D {
            camera: Camera3D {
                lighting: engine_project::rendering3d::SceneLighting {
                    exposure: 0.5,
                    ..Default::default()
                },
                ..Default::default()
            },
            world: Mat4::IDENTITY.to_cols_array_2d(),
        });
        frame.lights = vec![
            LightInstance3D {
                id: "sun".into(),
                light: SceneLight::Directional(sun),
                world: rotation.to_cols_array_2d(),
            },
            LightInstance3D {
                id: "spot".into(),
                light: SceneLight::Spot(spot),
                world: at(Vec3::new(1.0, 4.0, 0.0)),
            },
            LightInstance3D {
                id: "area".into(),
                light: SceneLight::Area(AreaLight {
                    lumens: std::f32::consts::PI * 2.0,
                    width: 2.0,
                    height: 1.0,
                    ..Default::default()
                }),
                world: at(Vec3::new(0.0, 3.0, 0.0)),
            },
        ];
        let list = light_list(&frame).unwrap();
        let [sun, spot, area] = &list.lights[..] else {
            panic!("{list:?}");
        };
        assert!(
            sun.direction.abs_diff_eq(Vec3::NEG_Y, 1e-6),
            "-Z rotated down"
        );
        assert_eq!(sun.intensity, 1.5, "3 lux x exposure 0.5");
        assert_eq!(sun.shadow.unwrap().cascades.count, 2);
        assert!((spot.intensity - 0.5).abs() < 1e-6, "1 cd x exposure");
        assert!(spot.position.abs_diff_eq(Vec3::new(1.0, 4.0, 0.0), 1e-6));
        assert!(matches!(
            spot.shadow.unwrap().filter,
            ShadowFilter::Pcss { .. }
        ));
        // 2 pi lm over a 2 m^2 one-sided rectangle: 1 cd/m^2.
        assert!((area.intensity - 0.5).abs() < 1e-6);
        assert!(matches!(
            area.shape,
            LightShape::Area { right, .. } if right.abs_diff_eq(Vec3::X, 1e-6)
        ));
        assert!(area.shadow.is_none());
        frame.camera3d = None;
        assert!(light_list(&frame).unwrap().lights.is_empty());
    }
}
