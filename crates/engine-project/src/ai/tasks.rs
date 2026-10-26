//! Built-in native tasks for behavior trees and state trees.
//!
//! Both runtimes register them under reserved names, so a `native_task` node can use
//! them with no game code:
//!
//! * `engine.run_query`: runs an environment query and writes the best item to
//!   blackboard keys.
//! * `engine.use_smart_object`: claims a smart object slot, optionally walks there,
//!   occupies it for a duration and releases it.
use super::{
    env_query::{self, Poll, QueryHandle},
    smart_object,
};
use crate::simulation::NavigationAgent;
use bevy_ecs::prelude::*;
use engine_ai::{
    BbValue,
    behavior_tree::{NativeTask, Status, TaskContext},
    smart_object::{ReleaseReason, SlotRef, SlotState},
};
use serde::Deserialize;

pub const RUN_QUERY: &str = "engine.run_query";
pub const USE_SMART_OBJECT: &str = "engine.use_smart_object";

const EPS: f64 = 1e-6;

type Created = std::result::Result<Box<dyn NativeTask<World>>, String>;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RunQueryParams {
    /// Name of a template in an `engine.EnvironmentQueries`.
    query: String,
    /// `vec3` key holding the query's `target` position.
    target_key: Option<String>,
    /// `vec3` key that receives the best item's position.
    position_key: Option<String>,
    /// `entity` key that receives the best item's subject.
    entity_key: Option<String>,
}

#[derive(Default)]
struct RunQuery {
    handle: Option<QueryHandle>,
    params: RunQueryParams,
}

impl RunQuery {
    fn poll(&mut self, cx: &mut TaskContext<'_, World>) -> Status {
        let Some(handle) = self.handle else {
            return Status::Failure;
        };
        match env_query::take_query(cx.world, handle) {
            Poll::Pending => Status::Running,
            Poll::Ready(result) => {
                self.handle = None;
                let found = env_query::write_best(
                    cx.world,
                    &result,
                    cx.blackboard,
                    self.params.position_key.as_deref(),
                    self.params.entity_key.as_deref(),
                );
                if found {
                    Status::Success
                } else {
                    Status::Failure
                }
            }
            Poll::Failed(_) | Poll::Unknown => {
                self.handle = None;
                Status::Failure
            }
        }
    }
}

impl NativeTask<World> for RunQuery {
    fn start(&mut self, cx: &mut TaskContext<'_, World>) -> Status {
        let Ok(params) = serde_json::from_value::<RunQueryParams>(cx.params.clone()) else {
            return Status::Failure;
        };
        self.params = params;
        let target = match &self.params.target_key {
            Some(key) => match cx.blackboard.get(key) {
                Some(BbValue::Vec3(p)) => Some(*p),
                _ => return Status::Failure,
            },
            None => None,
        };
        let querier = Entity::from_bits(cx.subject);
        match env_query::submit_query(cx.world, querier, &self.params.query, target) {
            Ok(handle) => {
                self.handle = Some(handle);
                self.poll(cx)
            }
            Err(_) => Status::Failure,
        }
    }
    fn tick(&mut self, cx: &mut TaskContext<'_, World>) -> Status {
        self.poll(cx)
    }
    fn abort(&mut self, cx: &mut TaskContext<'_, World>) {
        if let Some(handle) = self.handle.take() {
            env_query::cancel_query(cx.world, handle);
        }
    }
}

pub fn run_query_factory(_: &mut World, _: u64) -> Created {
    Ok(Box::<RunQuery>::default())
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UseParams {
    /// Slots must carry all of these tags.
    tags: Vec<String>,
    /// Seconds to occupy the slot; 0 occupies until the task is aborted.
    duration: f32,
    /// How close the user must get to the slot to occupy it.
    arrive_distance: f32,
    /// Drive the entity's `engine.NavigationAgent` to the slot.
    walk: bool,
    /// `navigation` asset for walking when the agent names none.
    navmesh: Option<String>,
    /// `string` key that receives the slot's behaviour id.
    behavior_key: Option<String>,
    /// `vec3` key that receives the slot position.
    position_key: Option<String>,
}

impl Default for UseParams {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            duration: 0.0,
            arrive_distance: 1.0,
            walk: false,
            navmesh: None,
            behavior_key: None,
            position_key: None,
        }
    }
}

#[derive(Default)]
struct UseSmartObject {
    params: UseParams,
    slot: Option<SlotRef>,
    position: [f32; 3],
    elapsed: f64,
    walking: bool,
}

impl UseSmartObject {
    fn user(cx: &TaskContext<'_, World>) -> Entity {
        Entity::from_bits(cx.subject)
    }

    fn stop_walking(&mut self, cx: &mut TaskContext<'_, World>) {
        if std::mem::take(&mut self.walking)
            && let Some(mut agent) = cx.world.get_mut::<NavigationAgent>(Self::user(cx))
        {
            agent.navmesh = None;
            agent.target = None;
            agent.waypoints.clear();
            agent.repath += 1;
        }
    }

    fn release(&mut self, cx: &mut TaskContext<'_, World>, reason: ReleaseReason) {
        if let Some(slot) = self.slot.take() {
            let _ = smart_object::release_slot(cx.world, slot, Self::user(cx), reason);
        }
        self.stop_walking(cx);
    }

    fn distance(&self, cx: &TaskContext<'_, World>) -> f32 {
        let here = crate::world_transform(cx.world, Self::user(cx))
            .map(|t| t.position())
            .unwrap_or([f32::MAX; 3]);
        let d = [
            here[0] - self.position[0],
            here[1] - self.position[1],
            here[2] - self.position[2],
        ];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }
}

impl NativeTask<World> for UseSmartObject {
    fn start(&mut self, cx: &mut TaskContext<'_, World>) -> Status {
        let Ok(params) = serde_json::from_value::<UseParams>(cx.params.clone()) else {
            return Status::Failure;
        };
        self.params = params;
        self.elapsed = 0.0;
        let user = Self::user(cx);
        let Ok(here) = crate::world_transform(cx.world, user).map(|t| t.position()) else {
            return Status::Failure;
        };
        let Ok(candidates) =
            smart_object::find_slots(cx.world, &self.params.tags, true, Some(here))
        else {
            return Status::Failure;
        };
        // Claims are served in call order; agents tick in entity order, so the lowest
        // entity wins a slot two agents want in the same tick.
        let Some(chosen) = candidates
            .into_iter()
            .take(16)
            .find(|s| matches!(smart_object::claim_slot(cx.world, s.slot, user), Ok(Ok(()))))
        else {
            return Status::Failure;
        };
        self.slot = Some(chosen.slot);
        self.position = chosen.position;
        if let Some(key) = &self.params.behavior_key {
            let _ = cx
                .blackboard
                .set(key, BbValue::String(chosen.behavior.clone()));
        }
        if let Some(key) = &self.params.position_key {
            let _ = cx.blackboard.set(key, BbValue::Vec3(chosen.position));
        }
        if self.params.walk && self.distance(cx) > self.params.arrive_distance {
            let navmesh = cx
                .world
                .get::<NavigationAgent>(user)
                .and_then(|a| a.navmesh.clone())
                .or_else(|| self.params.navmesh.clone());
            let target = chosen.position;
            let walked = navmesh.is_some_and(|mesh| {
                cx.world
                    .get_mut::<NavigationAgent>(user)
                    .is_some_and(|mut agent| {
                        agent.navmesh = Some(mesh);
                        agent.waypoints.clear();
                        agent.target = Some(target);
                        agent.repath += 1;
                        true
                    })
            });
            if !walked {
                self.release(cx, ReleaseReason::Aborted);
                return Status::Failure;
            }
            self.walking = true;
        }
        self.tick(cx)
    }

    fn tick(&mut self, cx: &mut TaskContext<'_, World>) -> Status {
        let Some(slot) = self.slot else {
            return Status::Failure;
        };
        let user = Self::user(cx).to_bits();
        let state = smart_object::slot_info(cx.world, slot)
            .ok()
            .flatten()
            .map(|i| i.state);
        match state {
            Some(SlotState::Claimed { user: holder, .. }) if holder == user => {
                if self.distance(cx) <= self.params.arrive_distance {
                    self.stop_walking(cx);
                    let occupied = smart_object::occupy_slot(cx.world, slot, Self::user(cx));
                    if !matches!(occupied, Ok(Ok(()))) {
                        self.slot = None;
                        return Status::Failure;
                    }
                    self.elapsed = 0.0;
                }
                Status::Running
            }
            Some(SlotState::Occupied { user: holder }) if holder == user => {
                self.elapsed += f64::from(cx.dt);
                if self.params.duration > 0.0
                    && self.elapsed + EPS >= f64::from(self.params.duration)
                {
                    self.release(cx, ReleaseReason::Completed);
                    Status::Success
                } else {
                    Status::Running
                }
            }
            // The claim expired, or the slot went to someone else.
            _ => {
                self.slot = None;
                self.stop_walking(cx);
                Status::Failure
            }
        }
    }

    fn abort(&mut self, cx: &mut TaskContext<'_, World>) {
        self.release(cx, ReleaseReason::Aborted);
    }
}

pub fn use_smart_object_factory(_: &mut World, _: u64) -> Created {
    Ok(Box::<UseSmartObject>::default())
}
