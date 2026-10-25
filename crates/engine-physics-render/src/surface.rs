//! `physics.cloth`, `physics.soft_body` and `physics.fragments`: dynamic world-space triangle
//! surfaces rewritten from the extract and drawn in the `Opaque` stage.

use crate::buffers::GrowBuffer;
use crate::config::{PhysicsRenderConfig, SurfaceShading};
use crate::extract::{FragmentDraw, PhysicsRenderExtract};
use crate::gpu::{depth_state, shader_module, view_layout};
use crate::status::{FeatureActivity, FeatureStatus, PhysicsRenderStatusHandle, ShadingReport};
use anyhow::Result;
use engine_render_pipeline::{
    DrawStats, ENGINE_VERTEX_LAYOUT, EngineVertex, FeatureContext, FeatureDescriptor, FeatureSetup,
    HDR_FORMAT, PipelineFeature, Stage, names,
};
use glam::Vec3;

pub const CLOTH: &str = "physics.cloth";
pub const SOFT_BODY: &str = "physics.soft_body";
pub const FRAGMENTS: &str = "physics.fragments";
const SHADER: &str = include_str!("wgsl/surface.wgsl");

/// Which surfaces a [`SurfaceFeature`] draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceKind {
    Cloth,
    SoftBody,
    Fragments,
}
impl SurfaceKind {
    pub const fn feature_name(self) -> &'static str {
        match self {
            Self::Cloth => CLOTH,
            Self::SoftBody => SOFT_BODY,
            Self::Fragments => FRAGMENTS,
        }
    }
}

/// Vertices and optional indices built for one upload.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceBatch {
    pub vertices: Vec<EngineVertex>,
    /// Empty for non-indexed (fragment) batches.
    pub indices: Vec<u32>,
    /// Objects drawn.
    pub objects: usize,
    /// Objects skipped because they would exceed the caps.
    pub dropped_for_cap: usize,
    /// Objects skipped because their data is inconsistent (length mismatch, bad indices).
    pub invalid: usize,
}

/// One indexed object: positions, normals, triangles and the colour of its vertices.
pub struct IndexedObject<'a> {
    pub positions: &'a [Vec3],
    pub normals: &'a [Vec3],
    pub triangles: &'a [[u32; 3]],
    pub color: [f32; 4],
}

fn finite_vertices(positions: &[Vec3]) -> bool {
    positions.iter().all(|p| p.is_finite())
}

/// Concatenates objects into one indexed batch. An object that does not fit the caps whole is
/// dropped (never split). Triangles referencing missing vertices make the object `invalid`.
pub fn build_indexed<'a>(
    objects: impl Iterator<Item = IndexedObject<'a>>,
    max_vertices: usize,
    max_indices: usize,
) -> SurfaceBatch {
    let mut batch = SurfaceBatch::default();
    for object in objects {
        let count = object.positions.len();
        let consistent = object.normals.len() == count
            && object
                .triangles
                .iter()
                .all(|t| t.iter().all(|&i| (i as usize) < count))
            && finite_vertices(object.positions);
        if !consistent {
            batch.invalid += 1;
            continue;
        }
        if batch.vertices.len() + count > max_vertices
            || batch.indices.len() + object.triangles.len() * 3 > max_indices
        {
            batch.dropped_for_cap += 1;
            continue;
        }
        let base = batch.vertices.len() as u32;
        batch
            .vertices
            .extend(
                object
                    .positions
                    .iter()
                    .zip(object.normals)
                    .map(|(p, n)| EngineVertex {
                        position: p.to_array(),
                        uv: [0.0; 2],
                        color: object.color,
                        normal: n.to_array(),
                    }),
            );
        batch.indices.extend(
            object
                .triangles
                .iter()
                .flat_map(|t| t.iter().map(|&i| base + i)),
        );
        batch.objects += 1;
    }
    batch
}

/// Flat-shaded, non-indexed triangles of destruction fragments in world space; fracture
/// (interior) triangles use `interior`, the rest `exterior`.
pub fn build_fragments(
    fragments: &[FragmentDraw],
    exterior: [f32; 4],
    interior: [f32; 4],
    max_vertices: usize,
) -> SurfaceBatch {
    let mut batch = SurfaceBatch::default();
    for fragment in fragments {
        let mesh = &fragment.mesh;
        let valid = mesh.interior.len() == mesh.triangles.len()
            && mesh
                .triangles
                .iter()
                .all(|t| t.iter().all(|&i| (i as usize) < mesh.positions.len()))
            && finite_vertices(&mesh.positions)
            && fragment.world_from_local.is_finite();
        if !valid {
            batch.invalid += 1;
            continue;
        }
        if batch.vertices.len() + mesh.triangles.len() * 3 > max_vertices {
            batch.dropped_for_cap += 1;
            continue;
        }
        for (triangle, &is_interior) in mesh.triangles.iter().zip(&mesh.interior) {
            let p = triangle.map(|i| {
                fragment
                    .world_from_local
                    .transform_point3(mesh.positions[i as usize])
            });
            let normal = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
            let color = if is_interior { interior } else { exterior };
            for point in p {
                batch.vertices.push(EngineVertex {
                    position: point.to_array(),
                    uv: [0.0; 2],
                    color,
                    normal: normal.to_array(),
                });
            }
        }
        batch.objects += 1;
    }
    batch
}

pub struct SurfaceFeature {
    kind: SurfaceKind,
    config: PhysicsRenderConfig,
    status: PhysicsRenderStatusHandle,
    pipeline: Option<wgpu::RenderPipeline>,
    vertices: GrowBuffer,
    indices: GrowBuffer,
    uploaded: Option<u64>,
    vertex_count: u32,
    index_count: u32,
    last: FeatureStatus,
}

impl SurfaceFeature {
    pub fn new(
        kind: SurfaceKind,
        config: PhysicsRenderConfig,
        status: PhysicsRenderStatusHandle,
    ) -> Self {
        let vertex_cap = config.max_surface_vertices as u64 * size_of::<EngineVertex>() as u64;
        let index_cap = config.max_surface_vertices as u64 * INDICES_PER_VERTEX as u64 * 4;
        Self {
            kind,
            config,
            status,
            pipeline: None,
            vertices: GrowBuffer::new(
                "physics surface vertices",
                wgpu::BufferUsages::VERTEX,
                vertex_cap,
            ),
            indices: GrowBuffer::new(
                "physics surface indices",
                wgpu::BufferUsages::INDEX,
                index_cap,
            ),
            uploaded: None,
            vertex_count: 0,
            index_count: 0,
            last: FeatureStatus::default(),
        }
    }

    fn enabled(&self) -> bool {
        match self.kind {
            SurfaceKind::Cloth => self.config.enabled.cloth,
            SurfaceKind::SoftBody => self.config.enabled.soft_bodies,
            SurfaceKind::Fragments => self.config.enabled.fragments,
        }
    }

    fn build(&self, extract: &PhysicsRenderExtract) -> SurfaceBatch {
        let max_vertices = self.config.max_surface_vertices;
        let max_indices = max_vertices * INDICES_PER_VERTEX;
        match self.kind {
            SurfaceKind::Cloth => build_indexed(
                extract.data.cloths.values().map(|c| IndexedObject {
                    positions: &c.positions,
                    normals: &c.normals,
                    triangles: &c.triangles,
                    color: self.config.cloth_color,
                }),
                max_vertices,
                max_indices,
            ),
            SurfaceKind::SoftBody => build_indexed(
                extract.data.soft_bodies.values().map(|b| IndexedObject {
                    positions: &b.positions,
                    normals: &b.normals,
                    triangles: &b.triangles,
                    color: self.config.soft_body_color,
                }),
                max_vertices,
                max_indices,
            ),
            SurfaceKind::Fragments => build_fragments(
                &extract.fragments,
                self.config.fragment_color,
                self.config.fragment_interior_color,
                max_vertices,
            ),
        }
    }
}

/// Index budget per vertex cap (a closed triangle mesh has about six indices per vertex).
const INDICES_PER_VERTEX: usize = 6;

impl PipelineFeature for SurfaceFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        let mut descriptor =
            FeatureDescriptor::new(self.kind.feature_name(), Stage::Opaque).after(names::OPAQUE);
        if !self.enabled() {
            descriptor = descriptor.disabled_by_default();
        }
        descriptor
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let name = self.kind.feature_name();
        let source = SHADER.replace("__ROUGHNESS__", &format!("{:?}", self.config.roughness));
        let module = shader_module(setup, name, &source)?;
        let layout = view_layout(setup, name);
        let entry = match self.config.shading {
            SurfaceShading::Lit => "fs_lit",
            SurfaceShading::NormalShaded => "fs_normal",
        };
        self.pipeline = Some(setup.gpu.device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some(name),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(ENGINE_VERTEX_LAYOUT)],
                },
                // Two-sided: cloth is a thin sheet and the shader flips the normal.
                primitive: Default::default(),
                depth_stencil: Some(depth_state(true, true)),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(HDR_FORMAT.into())],
                }),
                multiview_mask: None,
                cache: None,
            },
        ));
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let name = self.kind.feature_name();
        let mut status = FeatureStatus {
            cap: self.config.max_surface_vertices,
            shading: match self.config.shading {
                SurfaceShading::Lit => ShadingReport::Lit,
                SurfaceShading::NormalShaded => ShadingReport::NormalShaded,
            },
            ..Default::default()
        };
        let extract = ctx
            .world
            .and_then(|w| w.get_resource::<PhysicsRenderExtract>());
        match extract {
            None => {
                status.activity = FeatureActivity::Skipped;
                status.reason = Some(
                    "no PhysicsRenderExtract in the render world; call install_physics_extraction"
                        .into(),
                );
                self.vertex_count = 0;
                self.index_count = 0;
            }
            Some(extract) => {
                if self.uploaded != Some(extract.revision) {
                    let batch = self.build(extract);
                    let device = &ctx.gpu.device;
                    let queue = &ctx.gpu.queue;
                    self.vertices
                        .write(device, queue, bytemuck::cast_slice(&batch.vertices));
                    self.indices
                        .write(device, queue, bytemuck::cast_slice(&batch.indices));
                    self.vertex_count = batch.vertices.len() as u32;
                    self.index_count = batch.indices.len() as u32;
                    self.uploaded = Some(extract.revision);
                    let mut reasons = Vec::new();
                    if batch.dropped_for_cap > 0 {
                        reasons.push(format!(
                            "{} objects dropped: over the {} vertex cap",
                            batch.dropped_for_cap, self.config.max_surface_vertices
                        ));
                    }
                    if batch.invalid > 0 {
                        reasons.push(format!(
                            "{} objects skipped: inconsistent mesh data",
                            batch.invalid
                        ));
                    }
                    if self.kind == SurfaceKind::Fragments && extract.fragments_truncated {
                        reasons.push(format!(
                            "fragments capped at {} by the extraction",
                            self.config.max_fragments
                        ));
                    }
                    self.last = FeatureStatus {
                        truncated: !reasons.is_empty(),
                        reason: (!reasons.is_empty()).then(|| reasons.join("; ")),
                        ..Default::default()
                    };
                }
                status.truncated = self.last.truncated;
                status.reason = self.last.reason.clone();
                status.primitives = self.vertex_count as usize;
                status.activity = if self.vertex_count > 0 {
                    FeatureActivity::Drew
                } else {
                    FeatureActivity::Idle
                };
            }
        }
        if self.vertex_count > 0
            && let (Some(pipeline), Some(vertices)) = (&self.pipeline, self.vertices.buffer())
        {
            let indexed = self.index_count > 0;
            let mut pass = ctx.scene_pass(name)?;
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, ctx.view_bind_group, &[]);
            pass.set_vertex_buffer(0, vertices.slice(..self.vertices.len));
            if indexed {
                if let Some(indices) = self.indices.buffer() {
                    pass.set_index_buffer(
                        indices.slice(..self.indices.len),
                        wgpu::IndexFormat::Uint32,
                    );
                    pass.draw_indexed(0..self.index_count, 0, 0..1);
                }
            } else {
                pass.draw(0..self.vertex_count, 0..1);
            }
            drop(pass);
            ctx.record_draws(DrawStats {
                draw_calls: 1,
                instances: 1,
            });
        }
        self.status.set(name, status);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::simulation::physics::FragmentRender;
    use glam::Mat4;
    use std::sync::Arc;

    fn quad() -> (Vec<Vec3>, Vec<Vec3>, Vec<[u32; 3]>) {
        (
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            vec![Vec3::Z; 4],
            vec![[0, 1, 2], [0, 2, 3]],
        )
    }

    fn object<'a>(
        q: &'a (Vec<Vec3>, Vec<Vec3>, Vec<[u32; 3]>),
        color: [f32; 4],
    ) -> IndexedObject<'a> {
        IndexedObject {
            positions: &q.0,
            normals: &q.1,
            triangles: &q.2,
            color,
        }
    }

    #[test]
    fn indexed_objects_concatenate_with_rebased_indices() {
        let q = quad();
        let batch = build_indexed(
            [
                object(&q, [1.0, 0.0, 0.0, 1.0]),
                object(&q, [0.0, 1.0, 0.0, 1.0]),
            ]
            .into_iter(),
            100,
            100,
        );
        assert_eq!(batch.vertices.len(), 8);
        assert_eq!(batch.indices, vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]);
        assert_eq!(batch.objects, 2);
        assert_eq!(batch.vertices[0].color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(batch.vertices[4].color, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(batch.vertices[2].normal, [0.0, 0.0, 1.0]);
        assert_eq!(size_of::<EngineVertex>(), 48);
    }

    #[test]
    fn objects_that_exceed_a_cap_are_dropped_whole_and_counted() {
        let q = quad();
        let batch = build_indexed(
            [
                object(&q, [1.0; 4]),
                object(&q, [1.0; 4]),
                object(&q, [1.0; 4]),
            ]
            .into_iter(),
            9,
            100,
        );
        assert_eq!((batch.objects, batch.dropped_for_cap), (2, 1));
        assert_eq!(batch.vertices.len(), 8);
        let batch = build_indexed(
            [object(&q, [1.0; 4]), object(&q, [1.0; 4])].into_iter(),
            100,
            6,
        );
        assert_eq!((batch.objects, batch.dropped_for_cap), (1, 1));
    }

    #[test]
    fn inconsistent_objects_are_skipped_not_uploaded() {
        let mut q = quad();
        q.2.push([0, 1, 9]);
        let bad_index = build_indexed([object(&q, [1.0; 4])].into_iter(), 100, 100);
        assert_eq!((bad_index.objects, bad_index.invalid), (0, 1));
        assert!(bad_index.vertices.is_empty());
        let mut q = quad();
        q.1.pop();
        assert_eq!(
            build_indexed([object(&q, [1.0; 4])].into_iter(), 100, 100).invalid,
            1
        );
        let mut q = quad();
        q.0[2] = Vec3::NAN;
        assert_eq!(
            build_indexed([object(&q, [1.0; 4])].into_iter(), 100, 100).invalid,
            1
        );
    }

    fn fragment(translation: Vec3) -> FragmentDraw {
        FragmentDraw {
            owner: bevy_ecs::entity::Entity::PLACEHOLDER,
            fragment: bevy_ecs::entity::Entity::PLACEHOLDER,
            world_from_local: Mat4::from_translation(translation),
            mesh: Arc::new(FragmentRender {
                positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z],
                triangles: vec![[0, 2, 1], [0, 1, 3]],
                interior: vec![false, true],
            }),
        }
    }

    #[test]
    fn fragments_become_flat_world_space_triangles_with_interior_colours() {
        let outer = [0.5; 4];
        let inner = [1.0, 0.0, 0.0, 1.0];
        let batch = build_fragments(&[fragment(Vec3::new(10.0, 0.0, 0.0))], outer, inner, 100);
        assert_eq!(batch.vertices.len(), 6);
        assert!(batch.indices.is_empty());
        assert_eq!(batch.vertices[0].position, [10.0, 0.0, 0.0]);
        assert_eq!(batch.vertices[0].color, outer);
        assert_eq!(batch.vertices[3].color, inner);
        // Triangle [0, 2, 1] winds clockwise seen from +Z: its normal is -Z.
        assert_eq!(batch.vertices[0].normal, [0.0, 0.0, -1.0]);
        let capped = build_fragments(&[fragment(Vec3::ZERO), fragment(Vec3::X)], outer, inner, 6);
        assert_eq!((capped.objects, capped.dropped_for_cap), (1, 1));
    }

    #[test]
    fn the_shader_composes_and_validates_in_both_shading_modes() {
        let composer = engine_render_pipeline::ShaderComposer::new();
        let source = SHADER.replace("__ROUGHNESS__", &format!("{:?}", 1.0_f32));
        let composed = composer.compose(&source).unwrap();
        let module = engine_render_pipeline::validate_wgsl(&composed).unwrap();
        let entries: Vec<_> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(entries.contains(&"fs_lit") && entries.contains(&"fs_normal"));
    }
}
