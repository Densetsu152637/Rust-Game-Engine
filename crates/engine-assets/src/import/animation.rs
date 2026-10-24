use super::*;
use gltf::{
    accessor::{DataType, Dimensions},
    animation::{Property, util::ReadOutputs},
};
use std::collections::HashSet;

pub(super) fn import_skins(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
) -> Result<Vec<ImportedSkin>> {
    let mut skins = Vec::new();
    for skin in document.skins() {
        let joints: Vec<_> = skin.joints().map(|n| n.index()).collect();
        if joints.is_empty() || joints.iter().copied().collect::<HashSet<_>>().len() != joints.len()
        {
            return Err(invalid("skin requires unique nonempty joints"));
        }
        let inverse_bind_matrices = if let Some(accessor) = skin.inverse_bind_matrices() {
            if accessor.count() != joints.len()
                || accessor.data_type() != DataType::F32
                || accessor.dimensions() != Dimensions::Mat4
                || accessor.normalized()
            {
                return Err(invalid("invalid inverse bind matrix accessor"));
            }
            skin.reader(|buffer| Some(buffers[buffer.index()].as_slice()))
                .read_inverse_bind_matrices()
                .ok_or_else(|| invalid("cannot read bind matrices"))?
                .collect()
        } else {
            vec![
                [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                    [0.0, 0.0, 0.0, 1.0]
                ];
                joints.len()
            ]
        };
        finite(inverse_bind_matrices.iter().flatten().flatten().copied())?;
        skins.push(ImportedSkin {
            name: skin.name().map(str::to_owned),
            joints,
            inverse_bind_matrices,
            skeleton: skin.skeleton().map(|n| n.index()),
        });
    }
    Ok(skins)
}
pub(super) fn import_clips(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    limits: &ImportLimits,
) -> Result<Vec<ImportedClip>> {
    let mut clips = Vec::new();
    let mut key_count = 0;
    for clip in document.animations() {
        let mut channels = Vec::new();
        let mut duration = 0.0_f32;
        let mut targets = HashSet::new();
        for channel in clip.channels() {
            let property = channel.target().property();
            let property_key = match property {
                Property::Translation => 0,
                Property::Rotation => 1,
                Property::Scale => 2,
                Property::MorphTargetWeights => {
                    return Err(AssetError::Unsupported("morph animation".into()));
                }
            };
            let target_node = channel.target().node().index();
            if !targets.insert((target_node, property_key)) {
                return Err(invalid("duplicate animation target channel"));
            }
            let input = channel.sampler().input();
            let output = channel.sampler().output();
            if input.data_type() != DataType::F32
                || input.dimensions() != Dimensions::Scalar
                || input.normalized()
            {
                return Err(invalid("invalid animation time format"));
            }
            let expected = if property == Property::Rotation {
                Dimensions::Vec4
            } else {
                Dimensions::Vec3
            };
            if output.data_type() != DataType::F32
                || output.dimensions() != expected
                || output.normalized()
            {
                return Err(invalid("invalid animation output format"));
            }
            add_budget(
                &mut key_count,
                input.count(),
                limits.max_animation_keys,
                "animation keys",
            )?;
            let interpolation = match channel.sampler().interpolation() {
                gltf::animation::Interpolation::Step => Interpolation::Step,
                gltf::animation::Interpolation::Linear => Interpolation::Linear,
                gltf::animation::Interpolation::CubicSpline => Interpolation::CubicSpline,
            };
            let multiplier = if interpolation == Interpolation::CubicSpline {
                3
            } else {
                1
            };
            if input.count().checked_mul(multiplier) != Some(output.count()) {
                return Err(invalid("animation key/value count mismatch"));
            }
            let reader = channel.reader(|buffer| Some(buffers[buffer.index()].as_slice()));
            let times: Vec<f32> = reader
                .read_inputs()
                .ok_or_else(|| invalid("cannot read animation times"))?
                .collect();
            if times.is_empty()
                || times.iter().any(|v| !v.is_finite() || *v < 0.0)
                || times.windows(2).any(|v| v[0] >= v[1])
            {
                return Err(invalid(
                    "animation times must be finite, nonnegative and strictly increasing",
                ));
            }
            duration = duration.max(times.last().copied().unwrap_or(0.0));
            let values = match reader
                .read_outputs()
                .ok_or_else(|| invalid("cannot read animation outputs"))?
            {
                ReadOutputs::Translations(values) => ChannelValues::Translation(values.collect()),
                ReadOutputs::Rotations(values) => {
                    ChannelValues::Rotation(values.into_f32().collect())
                }
                ReadOutputs::Scales(values) => ChannelValues::Scale(values.collect()),
                ReadOutputs::MorphTargetWeights(_) => {
                    return Err(AssetError::Unsupported("morph animation".into()));
                }
            };
            match &values {
                ChannelValues::Translation(values) | ChannelValues::Scale(values) => {
                    finite(values.iter().flatten().copied())?
                }
                ChannelValues::Rotation(values) => {
                    finite(values.iter().flatten().copied())?;
                    let offset = usize::from(interpolation == Interpolation::CubicSpline);
                    for value in values.iter().skip(offset).step_by(multiplier) {
                        if value.iter().map(|v| v * v).sum::<f32>() < 1e-12 {
                            return Err(invalid("zero animation quaternion"));
                        }
                    }
                }
            }
            channels.push(ImportedChannel {
                target_node,
                interpolation,
                times,
                values,
            });
        }
        clips.push(ImportedClip {
            name: clip.name().map(str::to_owned),
            duration,
            channels,
        });
    }
    Ok(clips)
}
