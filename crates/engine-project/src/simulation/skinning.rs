use super::morph::{EvaluatedMorphWeights, MorphPlayer, MorphWeights};
use super::*;
use crate::{
    GeometryOverride,
    content::{Mesh, Vertex},
    model_import::ModelDocument,
};
use engine_animation::skin_vertices;
use engine_assets::{ChannelValues, ImportedMorphPrimitive, ImportedPrimitive, ImportedSkin};
use std::sync::Arc;

const MAX_VERTICES: usize = 1_000_000;
#[derive(PartialEq)]
struct Config {
    model: String,
    node: usize,
    primitive: usize,
    bindings: Vec<String>,
    skinned: bool,
    morphed: bool,
}
struct BoundSkin {
    config: Config,
    targets: Vec<Entity>,
    source: ImportedPrimitive,
    skin: Option<ImportedSkin>,
    morph: Option<ImportedMorphPrimitive>,
    defaults: Vec<f32>,
    output: Option<Arc<Mesh>>,
}
#[derive(Default)]
pub(super) struct Skins {
    bindings: BTreeMap<Entity, BoundSkin>,
    models: BTreeMap<String, (u64, Arc<ModelDocument>)>,
}
fn remove_output(world: &mut World, owner: Entity, binding: &BoundSkin) {
    let owns = world
        .get::<GeometryOverride>(owner)
        .zip(binding.output.as_ref())
        .is_some_and(|(current, old)| Arc::ptr_eq(&current.0, old));
    if let Ok(mut entity) = world.get_entity_mut(owner) {
        if owns {
            entity.remove::<GeometryOverride>();
        }
        entity.remove::<EvaluatedMorphWeights>();
    }
}
impl Skins {
    pub fn clear(&self, world: &mut World) {
        for (&owner, binding) in &self.bindings {
            remove_output(world, owner, binding);
        }
    }
    pub fn sync(&mut self, world: &mut World) -> Result<()> {
        let requested: Vec<_> = world
            .iter_entities()
            .filter_map(|e| {
                let skin = e.get::<SkinBinding>();
                let morph = e.get::<MorphWeights>();
                if skin.is_none() && morph.is_none() {
                    return None;
                }
                Some((e.id(), skin.cloned(), morph.cloned()))
            })
            .collect();
        let active: BTreeSet<_> = requested.iter().map(|(e, _, _)| *e).collect();
        let removed: Vec<_> = self
            .bindings
            .keys()
            .filter(|e| !active.contains(e))
            .copied()
            .collect();
        for owner in removed {
            if let Some(binding) = self.bindings.remove(&owner) {
                remove_output(world, owner, &binding);
            }
        }
        let used: BTreeSet<_> = requested
            .iter()
            .map(|(_, skin, morph)| {
                skin.as_ref().map_or_else(
                    || {
                        morph
                            .as_ref()
                            .expect("requested morph or skin")
                            .model
                            .clone()
                    },
                    |skin| skin.model.clone(),
                )
            })
            .collect();
        self.models.retain(|id, _| used.contains(id));
        let mut vertex_count = 0usize;
        let mut delta_count = 0usize;
        for (owner, skin, morph) in requested {
            if world.get::<physics::Cloth>(owner).is_some()
                || world.get::<physics::SoftBody>(owner).is_some()
            {
                return Err(invalid(
                    "Morph/skin deformation cannot share cloth or soft-body geometry authority",
                ));
            }
            let (model, node, primitive) = if let Some(skin) = &skin {
                (&skin.model, skin.mesh_node, skin.primitive)
            } else {
                let morph = morph.as_ref().expect("requested morph or skin");
                (&morph.model, morph.mesh_node, morph.primitive)
            };
            if morph.as_ref().is_some_and(|m| {
                &m.model != model || m.mesh_node != node || m.primitive != primitive
            }) {
                return Err(invalid("Morph and skin model bindings must match"));
            }
            let config = Config {
                model: model.clone(),
                node,
                primitive,
                bindings: skin.as_ref().map_or_else(Vec::new, |s| s.bindings.clone()),
                skinned: skin.is_some(),
                morphed: morph.is_some(),
            };
            if config.model.is_empty() || config.model.len() > 128 {
                return Err(invalid("Invalid deformation model ID"));
            }
            let targets = if let Some(skin) = &skin {
                if skin.bindings.is_empty() || skin.bindings.len() > 4096 {
                    return Err(invalid("Skin binding requires 1..4096 node references"));
                }
                animation::bindings(
                    world,
                    owner,
                    &AnimationPlayer {
                        bindings: skin.bindings.clone(),
                        ..Default::default()
                    },
                )?
            } else {
                vec![]
            };
            if self
                .bindings
                .get(&owner)
                .is_none_or(|old| old.config != config || old.targets != targets)
            {
                if !self.models.contains_key(&config.model) {
                    let project = &world.resource::<crate::runtime::ProjectResource>().0;
                    project.require_asset(&config.model, "model")?;
                    let source = crate::schema::read_bounded(
                        &project.resolve_asset(&config.model)?,
                        64 * 1024 * 1024,
                    )?;
                    let bytes = source.len() as u64;
                    if self.models.len() >= 64
                        || self.models.values().map(|v| v.0).sum::<u64>() + bytes
                            > 256 * 1024 * 1024
                    {
                        return Err(invalid("Runtime deformation model cache budget exceeded"));
                    }
                    let model: ModelDocument = serde_json::from_slice(&source)?;
                    model.validate()?;
                    self.models
                        .insert(config.model.clone(), (bytes, Arc::new(model)));
                }
                let model = &self.models[&config.model].1;
                if config.skinned && model.scene.nodes.len() != targets.len() {
                    return Err(invalid("Skin bindings differ from model node count"));
                }
                let node = model
                    .scene
                    .nodes
                    .get(config.node)
                    .ok_or_else(|| invalid("Mesh node outside model"))?;
                let mesh_index = node.mesh.ok_or_else(|| invalid("Node has no mesh"))?;
                let source = model
                    .scene
                    .meshes
                    .get(mesh_index)
                    .and_then(|m| m.primitives.get(config.primitive))
                    .ok_or_else(|| invalid("Primitive outside mesh"))?
                    .clone();
                let skin = if config.skinned {
                    Some(
                        node.skin
                            .and_then(|s| model.scene.skins.get(s))
                            .ok_or_else(|| invalid("Mesh node has no skin"))?
                            .clone(),
                    )
                } else {
                    None
                };
                if let Some(skin) = &skin {
                    for vertex in &source.vertices {
                        if vertex.weights.is_none()
                            || vertex.joints.is_none_or(|j| {
                                j.iter().any(|&i| usize::from(i) >= skin.joints.len())
                            })
                        {
                            return Err(invalid("Invalid skin vertex palette"));
                        }
                    }
                }
                let morph = model
                    .scene
                    .morphs
                    .primitives
                    .iter()
                    .find(|m| m.mesh == mesh_index && m.primitive == config.primitive)
                    .cloned();
                if config.morphed && morph.is_none() {
                    return Err(invalid("Morph binding primitive has no targets"));
                }
                let defaults = model
                    .scene
                    .morphs
                    .node_weights
                    .get(&config.node)
                    .cloned()
                    .unwrap_or_else(|| morph.as_ref().map_or_else(Vec::new, |m| m.weights.clone()));
                let output = self.bindings.remove(&owner).and_then(|old| old.output);
                self.bindings.insert(
                    owner,
                    BoundSkin {
                        config,
                        targets,
                        source,
                        skin,
                        morph,
                        defaults,
                        output,
                    },
                );
            }
            vertex_count = vertex_count
                .checked_add(self.bindings[&owner].source.vertices.len())
                .ok_or_else(|| invalid("Vertex count overflow"))?;
            delta_count = delta_count
                .checked_add(self.bindings[&owner].morph.as_ref().map_or(0, |m| {
                    m.targets
                        .iter()
                        .map(|t| {
                            t.positions.as_ref().map_or(0, Vec::len)
                                + t.normals.as_ref().map_or(0, Vec::len)
                                + t.tangents.as_ref().map_or(0, Vec::len)
                        })
                        .sum::<usize>()
                }))
                .ok_or_else(|| invalid("Morph delta budget overflow"))?;
            if delta_count > MAX_VERTICES {
                return Err(invalid("Runtime morph delta budget exceeded"));
            }
            if vertex_count > MAX_VERTICES {
                return Err(invalid("Runtime deformed vertex budget exceeded"));
            }
        }
        Ok(())
    }
    pub fn evaluate(&mut self, world: &mut World) -> Result<()> {
        for (&owner, binding) in &mut self.bindings {
            if world.get::<GeometryOverride>(owner).is_some_and(|current| {
                binding
                    .output
                    .as_ref()
                    .is_none_or(|old| !Arc::ptr_eq(&current.0, old))
            }) {
                return Err(invalid("GeometryOverride already has another writer"));
            }
            let mut weights = world
                .get::<MorphWeights>(owner)
                .filter(|m| !m.weights.is_empty())
                .map_or_else(|| binding.defaults.clone(), |m| m.weights.clone());
            if let Some(player) = world.get::<MorphPlayer>(owner) {
                let model = &self.models[&binding.config.model].1;
                let clip = model
                    .scene
                    .clips
                    .get(player.clip)
                    .ok_or_else(|| invalid("Morph clip outside model"))?;
                let channel = clip
                    .channels
                    .iter()
                    .find(|c| {
                        c.target_node == binding.config.node
                            && matches!(c.values, ChannelValues::Weights(_))
                    })
                    .ok_or_else(|| invalid("Morph clip has no weight channel for node"))?;
                let time = if player.looping && clip.duration > 0. {
                    player.time.rem_euclid(clip.duration)
                } else {
                    player.time.clamp(0., clip.duration)
                };
                weights = morph::sample(channel, binding.defaults.len(), time)?;
            }
            let (mut positions, mut normals) =
                morph::deform(&binding.source, binding.morph.as_ref(), &weights)?;
            if let Some(skin) = &binding.skin {
                let globals = binding
                    .targets
                    .iter()
                    .map(|e| {
                        crate::world_transform(world, *e)
                            .map(|t| Mat4::from_cols_array_2d(&t.matrix))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let matrices = skin_palette(skin, &globals, binding.config.node)?;
                let joints: Vec<_> = binding
                    .source
                    .vertices
                    .iter()
                    .map(|v| v.joints.expect("validated joints"))
                    .collect();
                let skin_weights: Vec<_> = binding
                    .source
                    .vertices
                    .iter()
                    .map(|v| v.weights.expect("validated weights"))
                    .collect();
                (positions, normals) =
                    skin_vertices(&positions, &normals, &joints, &skin_weights, &matrices)
                        .map_err(mapped)?;
            }
            let mesh = Arc::new(Mesh {
                normals: normals.iter().map(|n| n.to_array()).collect(),
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
            });
            let mut entity = world.entity_mut(owner);
            entity.insert(GeometryOverride(mesh.clone()));
            if binding.config.morphed {
                entity.insert(EvaluatedMorphWeights(weights));
            } else {
                entity.remove::<EvaluatedMorphWeights>();
            }
            binding.output = Some(mesh);
        }
        Ok(())
    }
}
