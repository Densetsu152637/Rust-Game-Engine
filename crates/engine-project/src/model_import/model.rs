use super::*;
use engine_assets::{ChannelValues, ImportedScene, Interpolation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDocument {
    pub version: u32,
    pub scene: ImportedScene,
    /// Original node index -> authored prefab entity ID (before instantiation prefix).
    pub node_entities: Vec<String>,
}
pub fn load_model(path: &Path) -> Result<ModelDocument> {
    let document: ModelDocument = serde_json::from_slice(&read_bounded(path, 256 * 1024 * 1024)?)?;
    document.validate()?;
    Ok(document)
}
fn invalid() -> Error {
    Error::new(
        "asset.model.invalid",
        "Invalid or over-budget imported model document",
    )
}
impl ModelDocument {
    pub fn validate(&self) -> Result<()> {
        let scene = &self.scene;
        scene.validate_morphs().map_err(|_| invalid())?;
        if self.version != 1
            || self.node_entities.len() != scene.nodes.len()
            || self
                .node_entities
                .iter()
                .any(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
            || self.node_entities.iter().collect::<BTreeSet<_>>().len() != self.node_entities.len()
        {
            return Err(invalid());
        }
        let limits = ImportLimits::default();
        let mut count = scene.nodes.len()
            + scene.meshes.len()
            + scene.materials.len()
            + scene.images.len()
            + scene.textures.len()
            + scene.skins.len()
            + scene.clips.len()
            + scene.scenes.len();
        if count > limits.max_entities {
            return Err(invalid());
        }
        let mut vertices = 0_usize;
        let mut indices = 0_usize;
        let mut keys = 0_usize;
        let mut image_bytes = 0_usize;
        let index = |i: Option<usize>, length: usize| i.is_none_or(|v| v < length);
        let finite = |values: &[f32]| values.iter().all(|v| v.is_finite());
        let mut child_occurrences = vec![0_usize; scene.nodes.len()];
        for (i, node) in scene.nodes.iter().enumerate() {
            count += node.children.len();
            if count > limits.max_entities {
                return Err(invalid());
            }
            if !index(node.parent, scene.nodes.len())
                || !index(node.mesh, scene.meshes.len())
                || !index(node.skin, scene.skins.len())
                || node.children.iter().any(|&child| {
                    child >= scene.nodes.len() || scene.nodes[child].parent != Some(i)
                })
                || !finite(&node.transform.translation)
                || !finite(&node.transform.rotation)
                || !finite(&node.transform.scale)
                || node.matrix.iter().any(|v| !finite(v))
            {
                return Err(invalid());
            }
            for &child in &node.children {
                child_occurrences[child] += 1;
            }
        }
        if scene
            .nodes
            .iter()
            .zip(child_occurrences)
            .any(|(node, count)| count != usize::from(node.parent.is_some()))
        {
            return Err(invalid());
        }
        // Bound parent traversal by the authored runtime hierarchy limit.
        for start in 0..scene.nodes.len() {
            let mut node = Some(start);
            let mut depth = 0;
            while let Some(i) = node {
                depth += 1;
                if depth > 256 {
                    return Err(invalid());
                }
                node = scene.nodes[i].parent;
            }
        }
        for roots in &scene.scenes {
            count += roots.len();
            if count > limits.max_entities {
                return Err(invalid());
            }
            if roots
                .iter()
                .any(|&i| i >= scene.nodes.len() || scene.nodes[i].parent.is_some())
            {
                return Err(invalid());
            }
        }
        if !index(scene.default_scene, scene.scenes.len()) {
            return Err(invalid());
        }
        let mut mesh_joints = Vec::with_capacity(scene.meshes.len());
        for mesh in &scene.meshes {
            let mut maximum_joint = 0_u16;
            let mut has_all_joints = true;
            count += mesh.primitives.len();
            if count > limits.max_entities {
                return Err(invalid());
            }
            for primitive in &mesh.primitives {
                vertices = vertices
                    .checked_add(primitive.vertices.len())
                    .ok_or_else(invalid)?;
                indices = indices
                    .checked_add(primitive.indices.len())
                    .ok_or_else(invalid)?;
                if vertices > limits.max_vertices || indices > limits.max_indices {
                    return Err(invalid());
                }
                if primitive.vertices.is_empty()
                    || primitive.indices.is_empty()
                    || !primitive.indices.len().is_multiple_of(3)
                    || !index(primitive.material, scene.materials.len())
                    || primitive
                        .indices
                        .iter()
                        .any(|&i| i as usize >= primitive.vertices.len())
                {
                    return Err(invalid());
                }
                for v in &primitive.vertices {
                    if !finite(&v.position)
                        || v.normal.is_some_and(|a| !finite(&a))
                        || v.tangent.is_some_and(|a| !finite(&a))
                        || v.uv0.is_some_and(|a| !finite(&a))
                        || v.uv1.is_some_and(|a| !finite(&a))
                        || v.color.is_some_and(|a| !finite(&a))
                        || v.weights.is_some_and(|a| !finite(&a))
                        || v.joints.is_some() != v.weights.is_some()
                    {
                        return Err(invalid());
                    }
                    if let Some(joints) = v.joints {
                        for joint in joints {
                            maximum_joint = maximum_joint.max(joint);
                        }
                    } else {
                        has_all_joints = false;
                    }
                    if let Some(weights) = v.weights {
                        let sum = weights.iter().sum::<f32>();
                        if weights.iter().any(|w| !(0.0..=1.0).contains(w))
                            || !sum.is_finite()
                            || sum <= 1e-6
                        {
                            return Err(invalid());
                        }
                    }
                }
            }
            mesh_joints.push((maximum_joint, has_all_joints));
        }
        for material in &scene.materials {
            if !finite(&material.base_color)
                || !finite(&material.emissive)
                || !finite(&[
                    material.metallic,
                    material.roughness,
                    material.normal_scale,
                    material.occlusion_strength,
                    material.alpha_cutoff,
                ])
            {
                return Err(invalid());
            }
            for reference in [
                material.base_color_texture,
                material.metallic_roughness_texture,
                material.normal_texture,
                material.occlusion_texture,
                material.emissive_texture,
            ]
            .into_iter()
            .flatten()
            {
                if reference.texture >= scene.textures.len() {
                    return Err(invalid());
                }
            }
        }
        for texture in &scene.textures {
            if texture.image >= scene.images.len() {
                return Err(invalid());
            }
        }
        for image in &scene.images {
            let expected = (image.width as usize)
                .checked_mul(image.height as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or_else(invalid)?;
            image_bytes = image_bytes.checked_add(expected).ok_or_else(invalid)?;
            if image.width == 0
                || image.height == 0
                || image.width > limits.max_image_dimension
                || image.height > limits.max_image_dimension
                || image.rgba8.len() != expected
            {
                return Err(invalid());
            }
        }
        for skin in &scene.skins {
            count += skin.joints.len();
            if count > limits.max_entities {
                return Err(invalid());
            }
            if skin.joints.is_empty()
                || skin.joints.len() != skin.inverse_bind_matrices.len()
                || !index(skin.skeleton, scene.nodes.len())
                || skin.joints.iter().any(|&i| i >= scene.nodes.len())
                || skin.joints.iter().collect::<BTreeSet<_>>().len() != skin.joints.len()
                || skin
                    .inverse_bind_matrices
                    .iter()
                    .flatten()
                    .any(|v| !finite(v))
            {
                return Err(invalid());
            }
        }
        for node in &scene.nodes {
            if let (Some(mesh), Some(skin)) = (node.mesh, node.skin) {
                let (maximum, complete) = mesh_joints[mesh];
                if !complete || maximum as usize >= scene.skins[skin].joints.len() {
                    return Err(invalid());
                }
            }
        }
        for clip in &scene.clips {
            let mut targets = BTreeSet::new();
            count += clip.channels.len();
            if count > limits.max_entities {
                return Err(invalid());
            }
            if !clip.duration.is_finite() || clip.duration < 0. {
                return Err(invalid());
            }
            for channel in &clip.channels {
                keys = keys.checked_add(channel.times.len()).ok_or_else(invalid)?;
                if keys > limits.max_animation_keys {
                    return Err(invalid());
                }
                if channel.target_node >= scene.nodes.len()
                    || channel.times.is_empty()
                    || channel
                        .times
                        .iter()
                        .any(|v| !v.is_finite() || *v < 0. || *v > clip.duration)
                    || channel.times.windows(2).any(|v| v[0] >= v[1])
                {
                    return Err(invalid());
                }
                let expected = channel
                    .times
                    .len()
                    .checked_mul(if channel.interpolation == Interpolation::CubicSpline {
                        3
                    } else {
                        1
                    })
                    .ok_or_else(invalid)?;
                let property = match &channel.values {
                    ChannelValues::Translation(_) => 0,
                    ChannelValues::Rotation(_) => 1,
                    ChannelValues::Scale(_) => 2,
                    ChannelValues::Weights(_) => 3,
                };
                if !targets.insert((channel.target_node, property)) {
                    return Err(invalid());
                }
                let valid = match &channel.values {
                    ChannelValues::Weights(v) => {
                        let width = scene.nodes[channel.target_node]
                            .mesh
                            .and_then(|mesh| {
                                scene.morphs.primitives.iter().find(|m| m.mesh == mesh)
                            })
                            .map_or(0, |m| m.targets.len());
                        keys = keys.checked_add(v.len()).ok_or_else(invalid)?;
                        width > 0 && expected.checked_mul(width) == Some(v.len()) && finite(v)
                    }
                    ChannelValues::Translation(v) | ChannelValues::Scale(v) => {
                        v.len() == expected && v.iter().all(|v| finite(v))
                    }
                    ChannelValues::Rotation(v) => {
                        v.len() == expected && v.iter().all(|v| finite(v))
                    }
                };
                if !valid {
                    return Err(invalid());
                }
            }
        }
        if count > limits.max_entities
            || vertices > limits.max_vertices
            || indices > limits.max_indices
            || keys > limits.max_animation_keys
            || image_bytes > limits.max_decoded_image_bytes
        {
            return Err(invalid());
        }
        Ok(())
    }
}
