//! Built-in tree tasks for abilities, registered for behavior trees and state trees:
//!
//! * `engine.activate_ability` `{ability, target_key?, wait?}`: activates a granted
//!   ability on the agent (the target is an `entity` blackboard key). Without `wait` it
//!   succeeds on activation; with `wait` it runs until the instance ends (success) or is
//!   cancelled (failure), and aborting the task cancels the instance.
//! * `engine.wait_ability_end` `{ability}`: runs while any instance of the ability is
//!   active on the agent, then succeeds.
use super::{InstanceId, Outcome};
use bevy_ecs::prelude::*;
use engine_ai::{
    BbValue,
    behavior_tree::{NativeTask, Status, TaskContext},
};
use serde::Deserialize;

pub const ACTIVATE_ABILITY: &str = "engine.activate_ability";
pub const WAIT_ABILITY_END: &str = "engine.wait_ability_end";

type Created = std::result::Result<Box<dyn NativeTask<World>>, String>;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ActivateParams {
    ability: String,
    /// `entity` key holding the target's authored id.
    target_key: Option<String>,
    /// Run until the ability ends.
    wait: bool,
}

#[derive(Default)]
struct ActivateAbility {
    waiting: Option<InstanceId>,
}

impl ActivateAbility {
    fn poll(&mut self, world: &World) -> Status {
        let Some(id) = self.waiting else {
            return Status::Failure;
        };
        match super::instance_outcome(world, id) {
            Some(Outcome::Active) => Status::Running,
            Some(Outcome::Ended) => {
                self.waiting = None;
                Status::Success
            }
            _ => {
                self.waiting = None;
                Status::Failure
            }
        }
    }
}

impl NativeTask<World> for ActivateAbility {
    fn start(&mut self, cx: &mut TaskContext<'_, World>) -> Status {
        self.waiting = None;
        let Ok(params) = serde_json::from_value::<ActivateParams>(cx.params.clone()) else {
            return Status::Failure;
        };
        let target = match &params.target_key {
            Some(key) => {
                let Some(BbValue::Entity(id)) = cx.blackboard.get(key) else {
                    return Status::Failure;
                };
                let found = cx
                    .world
                    .get_resource::<crate::runtime::EntityLookup>()
                    .and_then(|l| l.get(id));
                let Some(entity) = found else {
                    return Status::Failure;
                };
                Some(entity)
            }
            None => None,
        };
        let actor = Entity::from_bits(cx.subject);
        match super::try_activate_ability(cx.world, actor, &params.ability, target) {
            Ok(Ok(id)) if params.wait => {
                self.waiting = Some(id);
                self.poll(cx.world)
            }
            Ok(Ok(_)) => Status::Success,
            _ => Status::Failure,
        }
    }

    fn tick(&mut self, cx: &mut TaskContext<'_, World>) -> Status {
        self.poll(cx.world)
    }

    fn abort(&mut self, cx: &mut TaskContext<'_, World>) {
        if let Some(id) = self.waiting.take() {
            let _ = super::cancel_instance(cx.world, id);
        }
    }
}

pub fn activate_ability_factory(_: &mut World, _: u64) -> Created {
    Ok(Box::<ActivateAbility>::default())
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WaitParams {
    ability: String,
}

#[derive(Default)]
struct WaitAbilityEnd {
    ability: String,
}

impl WaitAbilityEnd {
    fn poll(&self, cx: &TaskContext<'_, World>) -> Status {
        if super::is_ability_active(cx.world, Entity::from_bits(cx.subject), &self.ability) {
            Status::Running
        } else {
            Status::Success
        }
    }
}

impl NativeTask<World> for WaitAbilityEnd {
    fn start(&mut self, cx: &mut TaskContext<'_, World>) -> Status {
        let Ok(params) = serde_json::from_value::<WaitParams>(cx.params.clone()) else {
            return Status::Failure;
        };
        self.ability = params.ability;
        self.poll(cx)
    }

    fn tick(&mut self, cx: &mut TaskContext<'_, World>) -> Status {
        self.poll(cx)
    }

    fn abort(&mut self, _: &mut TaskContext<'_, World>) {}
}

pub fn wait_ability_end_factory(_: &mut World, _: u64) -> Created {
    Ok(Box::<WaitAbilityEnd>::default())
}
