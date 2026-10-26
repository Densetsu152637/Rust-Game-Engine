//! Binds a running state tree to the ECS world: the behavior tree world host
//! (navigation, events, native tasks) plus host inputs and native evaluators.
use super::super::host::WorldHost;
use bevy_ecs::prelude::*;
use engine_ai::{
    BbValue, Blackboard,
    behavior_tree::{
        Host, MoveStatus, NativeCall, NativeTaskInstances, NativeTaskRegistry, Status,
    },
    state_tree::StateTreeHost,
};
use serde_json::Value;

/// Host inputs `copy_evaluator` nodes may read.
pub const INPUTS: [&str; 2] = ["self.position", "time.seconds"];

pub(super) struct StateWorldHost<'a> {
    /// Navigation, events and native tasks, shared with behavior trees.
    pub base: WorldHost<'a>,
    pub evaluators: &'a NativeTaskRegistry<World>,
    pub evaluator_natives: &'a mut NativeTaskInstances<World>,
    /// Simulated seconds since the runtime started.
    pub time: f64,
}

impl Host for StateWorldHost<'_> {
    fn request_move(&mut self, target: [f32; 3]) {
        self.base.request_move(target);
    }
    fn move_status(&mut self) -> MoveStatus {
        self.base.move_status()
    }
    fn cancel_move(&mut self) {
        self.base.cancel_move();
    }
    fn emit_event(&mut self, name: &str, payload: &Value) {
        self.base.emit_event(name, payload);
    }
    fn native_start(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard) -> Status {
        self.base.native_start(call, blackboard)
    }
    fn native_tick(
        &mut self,
        call: &NativeCall<'_>,
        blackboard: &mut Blackboard,
        dt: f32,
    ) -> Status {
        self.base.native_tick(call, blackboard, dt)
    }
    fn native_abort(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard) {
        self.base.native_abort(call, blackboard);
    }
}

impl StateTreeHost for StateWorldHost<'_> {
    fn input(&mut self, name: &str) -> Option<BbValue> {
        match name {
            "self.position" => crate::world_transform(self.base.world, self.base.entity)
                .ok()
                .map(|t| BbValue::Vec3(t.position())),
            "time.seconds" => Some(BbValue::Float(self.time)),
            _ => None,
        }
    }

    fn evaluator_start(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard) {
        self.evaluator_natives.start(
            self.evaluators,
            self.base.world,
            self.base.entity.to_bits(),
            call,
            blackboard,
        );
    }

    fn evaluator_tick(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard, dt: f32) {
        self.evaluator_natives.tick(
            self.evaluators,
            self.base.world,
            self.base.entity.to_bits(),
            call,
            blackboard,
            dt,
        );
    }

    fn evaluator_stop(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard) {
        self.evaluator_natives.abort(
            self.base.world,
            self.base.entity.to_bits(),
            call,
            blackboard,
        );
    }
}
