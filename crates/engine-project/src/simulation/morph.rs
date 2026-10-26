//! Morph weights have no transform authority; skinning owns final mesh deformation.
use super::*;
use engine_assets::{
    ChannelValues, ImportedChannel, ImportedMorphPrimitive, ImportedPrimitive, Interpolation,
};
use serde::{Deserialize, Serialize};

#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MorphWeights {
    pub model: String,
    pub mesh_node: usize,
    pub primitive: usize,
    /// Empty selects the imported node defaults, then mesh defaults.
    pub weights: Vec<f32>,
}
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MorphPlayer {
    /// Clip index in the retained model. Only its weight channel for this node is used.
    pub clip: usize,
    /// Native code can seek by changing this value, even while paused.
    pub time: f32,
    pub speed: f32,
    pub looping: bool,
    pub playing: bool,
}
impl Default for MorphPlayer {
    fn default() -> Self {
        Self {
            clip: 0,
            time: 0.,
            speed: 1.,
            looping: true,
            playing: true,
        }
    }
}
#[derive(Component, Clone, Debug, PartialEq)]
pub struct EvaluatedMorphWeights(pub Vec<f32>);

pub(super) fn step(world: &mut World, dt: f32) -> Result<()> {
    if !dt.is_finite() || dt < 0. {
        return Err(invalid("Invalid morph step"));
    }
    let mut query = world.query::<(Entity, &mut MorphPlayer)>();
    let mut owners = Vec::new();
    for (owner, mut player) in query.iter_mut(world) {
        if !player.time.is_finite() || !player.speed.is_finite() || player.speed.abs() > 100. {
            return Err(invalid("Invalid morph playback time or speed"));
        }
        if player.playing {
            player.time += dt * player.speed;
        }
        if !player.time.is_finite() {
            return Err(invalid("Morph playback overflow"));
        }
        owners.push(owner);
    }
    for owner in owners {
        if world.get::<MorphWeights>(owner).is_none() {
            return Err(invalid("MorphPlayer requires MorphWeights"));
        }
    }
    Ok(())
}

pub(super) fn sample(channel: &ImportedChannel, width: usize, time: f32) -> Result<Vec<f32>> {
    let ChannelValues::Weights(values) = &channel.values else {
        return Err(invalid("Expected weight channel"));
    };
    let cubic = channel.interpolation == Interpolation::CubicSpline;
    let stride = width
        .checked_mul(if cubic { 3 } else { 1 })
        .ok_or_else(|| invalid("Morph stride overflow"))?;
    if width == 0
        || width > 256
        || channel.times.is_empty()
        || channel.times.len().checked_mul(stride) != Some(values.len())
        || !time.is_finite()
    {
        return Err(invalid("Invalid morph channel shape"));
    }
    let times = &channel.times;
    let upper = times.partition_point(|t| *t <= time);
    let a = upper.saturating_sub(1).min(times.len() - 1);
    let offset = if cubic { width } else { 0 };
    if upper == 0 || upper == times.len() || channel.interpolation == Interpolation::Step {
        return Ok(values[a * stride + offset..a * stride + offset + width].to_vec());
    }
    let b = upper;
    let duration = times[b] - times[a];
    let t = (time - times[a]) / duration;
    let mut result = Vec::with_capacity(width);
    for i in 0..width {
        let left = values[a * stride + offset + i];
        let right = values[b * stride + offset + i];
        let value = if cubic {
            let t2 = t * t;
            let t3 = t2 * t;
            (2. * t3 - 3. * t2 + 1.) * left
                + (t3 - 2. * t2 + t) * duration * values[a * stride + 2 * width + i]
                + (-2. * t3 + 3. * t2) * right
                + (t3 - t2) * duration * values[b * stride + i]
        } else {
            left + (right - left) * t
        };
        if !value.is_finite() {
            return Err(invalid("Nonfinite sampled morph weight"));
        }
        result.push(value);
    }
    Ok(result)
}

pub(super) fn deform(
    source: &ImportedPrimitive,
    morph: Option<&ImportedMorphPrimitive>,
    weights: &[f32],
) -> Result<(Vec<Vec3>, Vec<Vec3>)> {
    let mut positions: Vec<_> = source
        .vertices
        .iter()
        .map(|v| Vec3::from(v.position))
        .collect();
    let mut normals: Vec<_> = source
        .vertices
        .iter()
        .map(|v| Vec3::from(v.normal.unwrap_or([0.; 3])))
        .collect();
    if let Some(morph) = morph {
        if weights.len() != morph.targets.len() || weights.iter().any(|v| !v.is_finite()) {
            return Err(invalid("Morph weight count or value invalid"));
        }
        for (target, &weight) in morph.targets.iter().zip(weights) {
            for (output, deltas) in [
                (&mut positions, &target.positions),
                (&mut normals, &target.normals),
            ] {
                if let Some(deltas) = deltas {
                    if output.len() != deltas.len() {
                        return Err(invalid("Morph delta count invalid"));
                    }
                    for (value, delta) in output.iter_mut().zip(deltas) {
                        *value += Vec3::from(*delta) * weight;
                    }
                }
            }
        }
    }
    if positions.iter().chain(&normals).any(|v| !v.is_finite()) {
        return Err(invalid("Morph deformation overflow"));
    }
    for normal in &mut normals {
        *normal = normal.normalize_or_zero();
    }
    Ok((positions, normals))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn step_linear_and_cubic_sample_vector_weights_at_endpoints_and_inside() {
        let mut channel = ImportedChannel {
            target_node: 0,
            interpolation: Interpolation::Linear,
            times: vec![0., 2.],
            values: ChannelValues::Weights(vec![-1., 2., 3., -2.]),
        };
        assert_eq!(sample(&channel, 2, -1.).unwrap(), vec![-1., 2.]);
        assert_eq!(sample(&channel, 2, 1.).unwrap(), vec![1., 0.]);
        assert_eq!(sample(&channel, 2, 5.).unwrap(), vec![3., -2.]);
        channel.interpolation = Interpolation::Step;
        assert_eq!(sample(&channel, 2, 1.).unwrap(), vec![-1., 2.]);
        channel.interpolation = Interpolation::CubicSpline;
        // dt=2: outgoing tangent 2 on key zero contributes +0.5 at the midpoint.
        channel.values =
            ChannelValues::Weights(vec![0., 0., 0., 0., 2., 0., 0., 0., 2., 4., 0., 0.]);
        assert_eq!(sample(&channel, 2, 1.).unwrap(), vec![1.5, 2.]);
        assert_eq!(sample(&channel, 2, 2.).unwrap(), vec![2., 4.]);
        assert!(sample(&channel, 1, 1.).is_err());
    }
}
