//! Binds a running behavior tree to the ECS world: navigation requests go to the
//! entity's `engine.NavigationAgent`, native tasks run against the `World`.
use crate::simulation::NavigationAgent;
use bevy_ecs::prelude::*;
use engine_ai::{
    Blackboard,
    behavior_tree::{
        Host, MoveStatus, NativeCall, NativeTaskInstances, NativeTaskRegistry, Status,
    },
};
use serde_json::Value;

/// Most `emit_event` node and service events one agent may emit per tick.
pub(super) const MAX_TASK_EVENTS: usize = 8;

/// Outstanding navigation request of one agent.
pub(super) struct Motion {
    /// A request was issued and not cancelled; navigation events apply to it.
    pub requested: bool,
    pub status: MoveStatus,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            requested: false,
            status: MoveStatus::Moving,
        }
    }
}

impl Motion {
    /// Applies a `navigation.*` event delivered for the agent's entity.
    pub fn apply(&mut self, event: &str) {
        if !self.requested {
            return;
        }
        match event {
            "navigation.arrived" => self.status = MoveStatus::Arrived,
            "navigation.failed" => self.status = MoveStatus::Failed,
            _ => {}
        }
    }
    fn fail(&mut self) {
        self.requested = false;
        self.status = MoveStatus::Failed;
    }
}

pub(super) struct WorldHost<'a> {
    pub world: &'a mut World,
    pub entity: Entity,
    pub registry: &'a NativeTaskRegistry<World>,
    pub natives: &'a mut NativeTaskInstances<World>,
    pub motion: &'a mut Motion,
    /// Navigation asset used when the entity's agent names none.
    pub navmesh: Option<&'a str>,
    /// Events from `emit_event` nodes, in emission order.
    pub events: Vec<(String, Value)>,
    /// Task events dropped by the per-tick bound.
    pub dropped: usize,
}

impl Host for WorldHost<'_> {
    fn request_move(&mut self, target: [f32; 3]) {
        let Some(mut agent) = self.world.get_mut::<NavigationAgent>(self.entity) else {
            self.motion.fail();
            return;
        };
        let Some(navmesh) = agent
            .navmesh
            .clone()
            .or_else(|| self.navmesh.map(str::to_owned))
        else {
            self.motion.fail();
            return;
        };
        agent.navmesh = Some(navmesh);
        agent.waypoints.clear();
        agent.target = Some(target);
        agent.repath += 1;
        self.motion.requested = true;
        self.motion.status = MoveStatus::Moving;
    }

    fn move_status(&mut self) -> MoveStatus {
        self.motion.status
    }

    fn cancel_move(&mut self) {
        self.motion.requested = false;
        if let Some(mut agent) = self.world.get_mut::<NavigationAgent>(self.entity) {
            agent.navmesh = None;
            agent.target = None;
            agent.waypoints.clear();
            agent.repath += 1;
        }
    }

    fn emit_event(&mut self, name: &str, payload: &Value) {
        if self.events.len() >= MAX_TASK_EVENTS {
            self.dropped += 1;
            return;
        }
        self.events.push((name.to_owned(), payload.clone()));
    }

    fn native_start(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard) -> Status {
        self.natives.start(
            self.registry,
            self.world,
            self.entity.to_bits(),
            call,
            blackboard,
        )
    }

    fn native_tick(
        &mut self,
        call: &NativeCall<'_>,
        blackboard: &mut Blackboard,
        dt: f32,
    ) -> Status {
        self.natives.tick(
            self.registry,
            self.world,
            self.entity.to_bits(),
            call,
            blackboard,
            dt,
        )
    }

    fn native_abort(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard) {
        self.natives
            .abort(self.world, self.entity.to_bits(), call, blackboard);
    }
}
