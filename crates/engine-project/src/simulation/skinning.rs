use super::*;
use crate::{
    GeometryOverride,
    content::{Mesh, Vertex},
    model_import::ModelDocument,
};
use engine_animation::skin_vertices;
use engine_assets::{ImportedPrimitive, ImportedSkin};
use std::sync::Arc;

const MAX_VERTICES: usize = 1_000_000;
struct BoundSkin {
    config: SkinBinding,
    targets: Vec<Entity>,
    source: ImportedPrimitive,
    skin: ImportedSkin,
}
#[derive(Default)]
pub(super) struct Skins {
    bindings: BTreeMap<Entity, BoundSkin>,
    models: BTreeMap<String, (u64, Arc<ModelDocument>)>,
}
impl Skins {
    pub fn clear(&self, world: &mut World) {
        for e in self.bindings.keys() {
            if let Ok(mut entity) = world.get_entity_mut(*e) {
                entity.remove::<GeometryOverride>();
            }
        }
    }
    pub fn sync(&mut self, world: &mut World) -> Result<()> {
        let removed: Vec<_> = self
            .bindings
            .keys()
            .filter(|e| world.get::<SkinBinding>(**e).is_none())
            .copied()
            .collect();
        for e in removed {
            self.bindings.remove(&e);
            if let Ok(mut entity) = world.get_entity_mut(e) {
                entity.remove::<GeometryOverride>();
            }
        }
        let requested: Vec<_> = world
            .iter_entities()
            .filter_map(|e| e.get::<SkinBinding>().map(|c| (e.id(), c.clone())))
            .collect();
        let used: BTreeSet<_> = requested.iter().map(|(_, c)| c.model.clone()).collect();
        self.models.retain(|id, _| used.contains(id));
        let mut vertex_count = 0usize;
        for (owner, config) in requested {
            if config.bindings.is_empty() || config.bindings.len() > 4096 {
                return Err(invalid("Skin binding requires 1..4096 node references"));
            }
            let targets = animation::bindings(
                world,
                owner,
                &AnimationPlayer {
                    bindings: config.bindings.clone(),
                    ..Default::default()
                },
            )?;
            if self
                .bindings
                .get(&owner)
                .is_none_or(|old| old.config != config || old.targets != targets)
            {
                if !self.models.contains_key(&config.model) {
                    let project = &world.resource::<crate::runtime::ProjectResource>().0;
                    project.require_asset(&config.model, "model")?;
                    let path = project.resolve_asset(&config.model)?;
                    let source = crate::schema::read_bounded(&path, 64 * 1024 * 1024)?;
                    let bytes = source.len() as u64;
                    if self.models.len() >= 64
                        || self.models.values().map(|v| v.0).sum::<u64>() + bytes
                            > 256 * 1024 * 1024
                    {
                        return Err(invalid("Runtime skin model cache budget exceeded"));
                    }
                    let model: ModelDocument = serde_json::from_slice(&source)?;
                    model.validate()?;
                    self.models
                        .insert(config.model.clone(), (bytes, Arc::new(model)));
                }
                let model = &self.models[&config.model].1;
                if model.scene.nodes.len() != targets.len() {
                    return Err(invalid("Skin bindings differ from model node count"));
                }
                let node = model
                    .scene
                    .nodes
                    .get(config.mesh_node)
                    .ok_or_else(|| invalid("Skin mesh node outside model"))?;
                let mesh = node
                    .mesh
                    .and_then(|m| model.scene.meshes.get(m))
                    .ok_or_else(|| invalid("Skin node has no mesh"))?;
                let source = mesh
                    .primitives
                    .get(config.primitive)
                    .ok_or_else(|| invalid("Skin primitive outside mesh"))?;
                let skin = node
                    .skin
                    .and_then(|s| model.scene.skins.get(s))
                    .ok_or_else(|| invalid("Mesh node has no skin"))?;
                if source.vertices.len() > MAX_VERTICES {
                    return Err(invalid("Skin vertex budget exceeded"));
                }
                for vertex in &source.vertices {
                    let joints = vertex
                        .joints
                        .ok_or_else(|| invalid("Skin vertex missing joint indices"))?;
                    if vertex.weights.is_none()
                        || joints.iter().any(|j| usize::from(*j) >= skin.joints.len())
                    {
                        return Err(invalid(
                            "Skin vertex weights missing or palette index invalid",
                        ));
                    }
                }
                self.bindings.insert(
                    owner,
                    BoundSkin {
                        config,
                        targets,
                        source: source.clone(),
                        skin: skin.clone(),
                    },
                );
            }
            vertex_count += self.bindings[&owner].source.vertices.len();
            if vertex_count > MAX_VERTICES {
                return Err(invalid("Runtime skinned vertex budget exceeded"));
            }
        }
        Ok(())
    }
    pub fn evaluate(&self, world: &mut World) -> Result<()> {
        for (owner, binding) in &self.bindings {
            let globals = binding
                .targets
                .iter()
                .map(|e| {
                    crate::world_transform(world, *e).map(|t| Mat4::from_cols_array_2d(&t.matrix))
                })
                .collect::<Result<Vec<_>>>()?;
            let matrices = skin_palette(&binding.skin, &globals, binding.config.mesh_node)?;
            let positions: Vec<_> = binding
                .source
                .vertices
                .iter()
                .map(|v| Vec3::from(v.position))
                .collect();
            let normals: Vec<_> = binding
                .source
                .vertices
                .iter()
                .map(|v| Vec3::from(v.normal.unwrap_or([0., 1., 0.])))
                .collect();
            let joints: Vec<_> = binding
                .source
                .vertices
                .iter()
                .map(|v| v.joints.expect("validated joints"))
                .collect();
            let weights: Vec<_> = binding
                .source
                .vertices
                .iter()
                .map(|v| v.weights.expect("validated weights"))
                .collect();
            let (positions, _) = skin_vertices(&positions, &normals, &joints, &weights, &matrices)
                .map_err(mapped)?;
            let mesh = Mesh {
                vertices: binding
                    .source
                    .vertices
                    .iter()
                    .zip(positions)
                    .map(|(source, position)| Vertex {
                        position: position.to_array(),
                        uv: source.uv0.unwrap_or([0.; 2]),
                        color: source.color.unwrap_or([1.; 4]),
                    })
                    .collect(),
                indices: binding.source.indices.clone(),
            };
            world
                .entity_mut(*owner)
                .insert(GeometryOverride(Arc::new(mesh)));
        }
        Ok(())
    }
}
