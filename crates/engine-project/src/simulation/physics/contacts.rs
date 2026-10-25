//! Reference feature: contact and sensor transitions become `physics.contact.start` and
//! `physics.contact.stop` events, delivered to both entities after each substep.
use super::{Context, Feature, emit};
use crate::Result;
use bevy_ecs::prelude::*;

pub(super) struct Contacts;
impl Feature for Contacts {
    fn name(&self) -> &'static str {
        "contact"
    }
    fn after_substep(&mut self, cx: Context<'_>, world: &mut World, _dt: f32) -> Result<()> {
        let events = [
            (2, cx.worlds.two.drain_events().collect::<Vec<_>>()),
            (3, cx.worlds.three.drain_events().collect::<Vec<_>>()),
        ];
        for (dimension, events) in events {
            for event in events {
                let (Some(first), Some(second)) = (
                    Entity::try_from_bits(event.first),
                    Entity::try_from_bits(event.second),
                ) else {
                    continue;
                };
                for (target, other) in [(first, second), (second, first)] {
                    if world.get_entity(target).is_err() {
                        continue;
                    }
                    let other_id = world
                        .get::<crate::AuthoredId>(other)
                        .map(|id| id.id.clone());
                    let source = world.get_entity(other).is_ok().then_some(other);
                    let name = if event.started { "start" } else { "stop" };
                    let payload = serde_json::json!({"dimension":dimension,"other":other_id});
                    emit(world, self, name, target, source, payload)?;
                }
            }
        }
        Ok(())
    }
}
