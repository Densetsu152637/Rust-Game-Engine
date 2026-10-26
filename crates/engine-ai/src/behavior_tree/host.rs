//! The seam between a behavior tree and the game that runs it.
//!
//! A tree never touches a world. It asks its [`Host`] to start navigation, emit
//! events and run native tasks. `engine-project` implements the host over the ECS;
//! tests implement it with plain recorders.
use crate::blackboard::Blackboard;
use serde_json::Value;
use std::collections::BTreeMap;

/// Outcome of one task step. Also the result of a node or the whole tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Success,
    Failure,
}

/// Progress of the navigation request a `move_to` task issued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveStatus {
    /// The agent is travelling, or the request was not yet acknowledged.
    Moving,
    Arrived,
    Failed,
}

/// Identifies one native call site in a compiled tree.
#[derive(Clone, Copy, Debug)]
pub struct NativeCall<'a> {
    /// Unique per call site within the tree; stable for a given document.
    pub site: usize,
    /// Graph node id of the task or service node.
    pub node: &'a str,
    /// Registered native task name.
    pub name: &'a str,
    /// Authored opaque parameters (`null` when absent).
    pub params: &'a Value,
}

/// What a running behavior tree needs from its game.
pub trait Host {
    /// Starts navigating to `target`, replacing any earlier request.
    fn request_move(&mut self, target: [f32; 3]);
    /// Progress of the latest request.
    fn move_status(&mut self) -> MoveStatus;
    /// Cancels the latest request (a move task was aborted or timed out).
    fn cancel_move(&mut self);
    /// Emits a game event for the agent. Hosts bound and rate-limit these.
    fn emit_event(&mut self, name: &str, payload: &Value);
    /// First step of a native task or service.
    fn native_start(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard) -> Status;
    /// Later steps of a running native task, or the periodic run of a service.
    fn native_tick(
        &mut self,
        call: &NativeCall<'_>,
        blackboard: &mut Blackboard,
        dt: f32,
    ) -> Status;
    /// The task was interrupted, or the service's branch deactivated.
    fn native_abort(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard);
}

/// State handed to a [`NativeTask`] callback. `W` is the world type of the host
/// (a Bevy `World` in `engine-project`); `subject` is the agent's opaque id.
pub struct TaskContext<'a, W: ?Sized> {
    pub world: &'a mut W,
    pub subject: u64,
    pub blackboard: &'a mut Blackboard,
    /// Authored parameters of the node (`null` when absent).
    pub params: &'a Value,
    /// Seconds since the previous callback of this task (0 in `start`/`abort`).
    pub dt: f32,
}

/// A game-implemented task. One instance exists per agent and call site.
///
/// `start` runs when the node activates; returning `Running` makes the tree call
/// `tick` every tick until it returns `Success` or `Failure`; `abort` runs when
/// the tree interrupts a running task (a higher priority branch preempted it).
/// Used as a service, `tick` runs at the authored interval and its result is
/// ignored.
pub trait NativeTask<W: ?Sized>: Send + Sync {
    fn start(&mut self, cx: &mut TaskContext<'_, W>) -> Status;
    fn tick(&mut self, cx: &mut TaskContext<'_, W>) -> Status;
    fn abort(&mut self, cx: &mut TaskContext<'_, W>);
}

/// Creates the task instance for an agent. `subject` is the agent's opaque id.
pub type TaskFactory<W> = fn(&mut W, u64) -> Result<Box<dyn NativeTask<W>>, String>;

/// Names to task factories, shared by every agent of a game.
pub struct NativeTaskRegistry<W: ?Sized> {
    factories: BTreeMap<String, TaskFactory<W>>,
}

impl<W: ?Sized> Default for NativeTaskRegistry<W> {
    fn default() -> Self {
        Self {
            factories: BTreeMap::new(),
        }
    }
}

impl<W: ?Sized> Clone for NativeTaskRegistry<W> {
    fn clone(&self) -> Self {
        Self {
            factories: self.factories.clone(),
        }
    }
}

impl<W: ?Sized> NativeTaskRegistry<W> {
    /// Registers `factory` under `name`; a repeated name is an error.
    pub fn register(&mut self, name: &str, factory: TaskFactory<W>) -> Result<(), String> {
        if name.is_empty() || name.len() > 128 {
            return Err("native task names are 1-128 characters".to_owned());
        }
        if self.factories.contains_key(name) {
            return Err(format!("native task {name:?} is already registered"));
        }
        self.factories.insert(name.to_owned(), factory);
        Ok(())
    }
    pub fn contains(&self, name: &str) -> bool {
        self.factories.contains_key(name)
    }
    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.factories.keys()
    }
}

/// The live task instances of one agent, keyed by call site.
pub struct NativeTaskInstances<W: ?Sized> {
    tasks: BTreeMap<usize, Box<dyn NativeTask<W>>>,
    errors: Vec<String>,
}

impl<W: ?Sized> Default for NativeTaskInstances<W> {
    fn default() -> Self {
        Self {
            tasks: BTreeMap::new(),
            errors: Vec::new(),
        }
    }
}

/// Most factory errors remembered per agent.
const MAX_ERRORS: usize = 8;

impl<W: ?Sized> NativeTaskInstances<W> {
    fn instance(
        &mut self,
        registry: &NativeTaskRegistry<W>,
        world: &mut W,
        subject: u64,
        call: &NativeCall<'_>,
    ) -> Option<&mut Box<dyn NativeTask<W>>> {
        if let std::collections::btree_map::Entry::Vacant(slot) = self.tasks.entry(call.site) {
            let created = match registry.factories.get(call.name) {
                Some(factory) => factory(world, subject),
                None => Err(format!("native task {:?} is not registered", call.name)),
            };
            match created {
                Ok(task) => {
                    slot.insert(task);
                }
                Err(message) => {
                    if self.errors.len() < MAX_ERRORS {
                        self.errors.push(message);
                    }
                    return None;
                }
            }
        }
        self.tasks.get_mut(&call.site)
    }

    /// Runs `start`; a task that cannot be created fails.
    pub fn start(
        &mut self,
        registry: &NativeTaskRegistry<W>,
        world: &mut W,
        subject: u64,
        call: &NativeCall<'_>,
        blackboard: &mut Blackboard,
    ) -> Status {
        let Some(task) = self.instance(registry, world, subject, call) else {
            return Status::Failure;
        };
        task.start(&mut TaskContext {
            world,
            subject,
            blackboard,
            params: call.params,
            dt: 0.0,
        })
    }

    pub fn tick(
        &mut self,
        registry: &NativeTaskRegistry<W>,
        world: &mut W,
        subject: u64,
        call: &NativeCall<'_>,
        blackboard: &mut Blackboard,
        dt: f32,
    ) -> Status {
        let Some(task) = self.instance(registry, world, subject, call) else {
            return Status::Failure;
        };
        task.tick(&mut TaskContext {
            world,
            subject,
            blackboard,
            params: call.params,
            dt,
        })
    }

    pub fn abort(
        &mut self,
        world: &mut W,
        subject: u64,
        call: &NativeCall<'_>,
        blackboard: &mut Blackboard,
    ) {
        if let Some(task) = self.tasks.get_mut(&call.site) {
            task.abort(&mut TaskContext {
                world,
                subject,
                blackboard,
                params: call.params,
                dt: 0.0,
            });
        }
    }

    /// Takes the factory errors recorded since the last call.
    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }
}
