//! The perception host over the ECS: occlusion rays against the 3D physics
//! colliders and the hearing attenuation hook.
use super::component::Occlusion;
use crate::simulation::physics::{Dimension, QueryFilter, SensorFilter, ray_cast};
use bevy_ecs::prelude::*;
use engine_ai::perception::{Id, PerceptionHost};
use glam::Vec3;

/// Attenuation of one noise for one listener, in `[0, 1]`: arguments are the world,
/// the listener, the listener position, the noise source entity (if any) and the
/// noise position. An acoustics-aware game registers one with
/// `register_hearing_attenuation` to occlude noises through geometry.
pub type HearingHook = fn(&World, Entity, [f32; 3], Option<Entity>, [f32; 3]) -> f32;

pub(super) struct WorldPerceptionHost<'a> {
    pub world: &'a World,
    pub occlusion: Occlusion,
    /// Physics layer bits that occlude.
    pub mask: u32,
    pub hearing: Option<HearingHook>,
}

impl PerceptionHost for WorldPerceptionHost<'_> {
    fn line_of_sight(&mut self, perceiver: Id, target: Id, from: [f32; 3], to: [f32; 3]) -> bool {
        if self.occlusion == Occlusion::None {
            return true;
        }
        let origin = Vec3::from(from);
        let delta = Vec3::from(to) - origin;
        let distance = delta.length();
        if distance < 1e-4 {
            return true;
        }
        let filter = QueryFilter {
            mask: self.mask,
            exclude: vec![Entity::from_bits(perceiver), Entity::from_bits(target)],
            sensors: SensorFilter::Exclude,
            ..QueryFilter::default()
        };
        // A world without a physics service has nothing to occlude with.
        !matches!(
            ray_cast(
                self.world,
                Dimension::Three,
                origin,
                delta / distance,
                distance,
                &filter
            ),
            Ok(Some(_))
        )
    }

    fn hearing_attenuation(
        &mut self,
        listener: Id,
        listener_position: [f32; 3],
        source: Option<Id>,
        source_position: [f32; 3],
    ) -> f32 {
        match self.hearing {
            Some(hook) => hook(
                self.world,
                Entity::from_bits(listener),
                listener_position,
                source.map(Entity::from_bits),
                source_position,
            ),
            None => 1.0,
        }
    }
}
