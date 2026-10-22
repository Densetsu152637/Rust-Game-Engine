//! Shared fixtures for the behavioural suites. Every integration test file is
//! its own crate, so items used by only some files are allowed to be unused.
#![allow(dead_code)]

use engine_core::prelude::*;

/// Deterministic splitmix64 generator so "random" simulations are reproducible.
#[derive(Clone, Debug)]
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform value in `0..bound` (bound must be non-zero).
    pub fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }

    pub fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.below(denominator) < numerator
    }
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Vel {
    pub x: i32,
    pub y: i32,
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Health(pub i32);

#[derive(Component, Clone, Debug, PartialEq, Eq, Default)]
pub struct Label(pub String);

/// Zero-sized marker components.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Tag;

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Other;

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Frozen;

#[derive(Bundle, Clone, Debug)]
pub struct Body {
    pub pos: Pos,
    pub vel: Vel,
}

impl Body {
    pub fn at(x: i32, y: i32) -> Self {
        Self {
            pos: Pos { x, y },
            vel: Vel::default(),
        }
    }
}

#[derive(Bundle, Clone, Debug)]
pub struct Creature {
    pub body: Body,
    pub health: Health,
}

/// Ordered event log resource used to observe system execution order.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Log(pub Vec<&'static str>);

#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Counter(pub i64);

#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Seen(pub Vec<i64>);

/// A shared app with the standard 1/60 fixed step used by most tests.
pub const DT: f32 = 0.25;

pub fn app() -> EngineApp {
    EngineApp::new()
}

pub fn update(app: &mut EngineApp) -> &mut Schedule {
    app.schedule_mut(Stage::Update)
}

/// Count entities in a world that have component `T` (ignores resource entities).
pub fn count<T: Component>(world: &mut World) -> usize {
    world.query::<&T>().iter(world).count()
}

pub fn count_with<F: engine_core::bevy_ecs::query::QueryFilter>(world: &mut World) -> usize {
    world.query_filtered::<Entity, F>().iter(world).count()
}

/// Run a one-off system with deferred commands applied.
pub fn run_once<M>(world: &mut World, system: impl IntoSystem<(), (), M>) {
    use engine_core::bevy_ecs::system::RunSystemOnce;
    world.run_system_once(system).expect("system must run");
}

pub fn log(app: &EngineApp) -> Vec<&'static str> {
    app.world().resource::<Log>().0.clone()
}
