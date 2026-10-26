//! Conversion retains every source node, including non-joint transform ancestors.
use super::*;
use engine_animation::{LocalPose, Skeleton};
use engine_assets::{ChannelValues, ImportedScene, ImportedSkin, Interpolation};
pub fn imported_skeleton(scene: &ImportedScene) -> Result<Skeleton> {
    if scene.nodes.is_empty() || scene.nodes.len() > 4096 {
        return Err(invalid("Imported skeleton node budget exceeded"));
    }
    let mut rest = Vec::with_capacity(scene.nodes.len());
    for node in &scene.nodes {
        let pose = LocalPose {
            translation: Vec3::from(node.transform.translation),
            rotation: Quat::from_array(node.transform.rotation),
            scale: Vec3::from(node.transform.scale),
        };
        pose.validate().map_err(mapped)?;
        if !pose
            .matrix()
            .abs_diff_eq(Mat4::from_cols_array_2d(&node.matrix), 0.0001)
        {
            return Err(invalid("Imported node matrix contains unsupported shear"));
        }
        rest.push(pose);
    }
    Skeleton::new(
        scene.nodes.iter().map(|n| n.parent).collect(),
        vec![Mat4::IDENTITY; scene.nodes.len()],
        rest,
    )
    .map_err(mapped)
}
pub fn imported_clip(scene: &ImportedScene, index: usize) -> Result<ClipDocument> {
    if scene.nodes.is_empty() || scene.nodes.len() > 4096 {
        return Err(invalid("Imported animation node budget exceeded"));
    }
    let source = scene
        .clips
        .get(index)
        .ok_or_else(|| invalid("Missing imported clip"))?;
    if source.channels.len() > 12288 {
        return Err(invalid("Imported channel budget exceeded"));
    }
    let mut tracks = BTreeMap::<usize, ClipTrack>::new();
    for channel in &source.channels {
        if matches!(channel.values, ChannelValues::Weights(_)) {
            continue;
        }
        let interpolation = match channel.interpolation {
            Interpolation::Step => Sampling::Step,
            Interpolation::Linear => Sampling::Linear,
            Interpolation::CubicSpline => {
                return Err(invalid(
                    "Cubic spline animation is preserved in model metadata but not sampled",
                ));
            }
        };
        if channel.target_node >= scene.nodes.len() || channel.times.len() > 1_000_000 {
            return Err(invalid("Imported animation target or key budget invalid"));
        }
        let track = tracks.entry(channel.target_node).or_insert(ClipTrack {
            joint: channel.target_node,
            translation: None,
            rotation: None,
            scale: None,
        });
        let duplicate = match &channel.values {
            ChannelValues::Weights(_) => continue,
            ChannelValues::Translation(values) => track
                .translation
                .replace(VectorChannel {
                    times: channel.times.clone(),
                    values: values.clone(),
                    interpolation,
                })
                .is_some(),
            ChannelValues::Rotation(values) => track
                .rotation
                .replace(RotationChannel {
                    times: channel.times.clone(),
                    values: values.clone(),
                    interpolation,
                })
                .is_some(),
            ChannelValues::Scale(values) => track
                .scale
                .replace(VectorChannel {
                    times: channel.times.clone(),
                    values: values.clone(),
                    interpolation,
                })
                .is_some(),
        };
        if duplicate {
            return Err(invalid("Duplicate imported animation channel"));
        }
    }
    let result = ClipDocument {
        version: 1,
        duration: source.duration,
        joints: scene.nodes.len(),
        tracks: tracks.into_values().collect(),
        events: vec![],
    };
    result.clone().into_clip()?;
    Ok(result)
}
/// Mesh-local palette: inverse(mesh global) * joint global * inverse bind.
/// The renderer applies the mesh node's world transform after skinning.
pub fn skin_palette(skin: &ImportedSkin, globals: &[Mat4], mesh_node: usize) -> Result<Vec<Mat4>> {
    if skin.joints.is_empty()
        || skin.joints.len() > 4096
        || skin.joints.len() != skin.inverse_bind_matrices.len()
    {
        return Err(invalid("Invalid skin palette"));
    }
    let mesh = globals
        .get(mesh_node)
        .ok_or_else(|| invalid("Missing mesh node"))?;
    let inverse = mesh.inverse();
    if !inverse.is_finite() {
        return Err(invalid("Singular mesh transform"));
    }
    skin.joints
        .iter()
        .zip(&skin.inverse_bind_matrices)
        .map(|(joint, bind)| {
            let global = globals
                .get(*joint)
                .ok_or_else(|| invalid("Skin joint outside hierarchy"))?;
            let result = inverse * *global * Mat4::from_cols_array_2d(bind);
            if !result.is_finite() {
                return Err(invalid("Nonfinite skin palette"));
            }
            Ok(result)
        })
        .collect()
}
