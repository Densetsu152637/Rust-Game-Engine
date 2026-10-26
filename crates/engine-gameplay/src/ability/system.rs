//! Actors, attributes, tags and active gameplay effects.
//!
//! [`AbilitySystem`] owns every actor of a game (keyed by an opaque [`ActorId`], iterated in
//! id order), their attributes and tags, the active effects on them, ability instances and
//! the prediction journals. Everything is deterministic: no clocks, no hashing, and time
//! only advances through [`AbilitySystem::tick_effects`].
use super::defs::{
    Bound, Capture, CaptureFrom, DurationPolicy, DurationRefresh, EffectDef, InhibitionPolicy,
    Library, Magnitude, ModOp, ModifierDef, PeriodReset, StackExpiration, StackKind,
};
use super::tags::{TagContainer, tag_matches, valid_tag};
use engine_net_predict::{PredictionHooks, PredictionKey, UndoReason};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

/// Opaque actor id (an entity's bits in `engine-project`).
pub type ActorId = u64;

pub const MAX_ACTORS: usize = 1024;
pub const MAX_EFFECTS_PER_ACTOR: usize = 64;
pub const MAX_INSTANCES_PER_ACTOR: usize = 8;
pub const MAX_LOOSE_TAGS: usize = 64;
/// Most periodic executions of one effect in one tick; later due executions are skipped.
pub const MAX_EXECUTIONS_PER_TICK: u64 = 256;
/// Finished ability outcomes remembered for [`AbilitySystem::outcome`].
pub const MAX_OUTCOMES: usize = 1024;
/// Undrained events kept; older ones are dropped and counted.
pub const MAX_PENDING_EVENTS: usize = 4096;
/// Time tolerance (seconds) for expiry, periods and waits, so accumulated `f32` steps hit
/// their exact tick.
pub const TIME_EPSILON: f64 = 1e-5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct EffectHandle(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct InstanceId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct AttributeValue {
    pub base: f32,
    pub current: f32,
}

/// An effect definition bound to a source and level, ready to apply.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectSpec {
    pub effect: String,
    pub level: u32,
    pub source: ActorId,
    /// Tags every side effect of the application with this prediction key.
    pub key: Option<PredictionKey>,
    pub(crate) def: usize,
    /// Final magnitudes captured from the source when the spec was made.
    pub(crate) source_captures: Vec<Option<f32>>,
}

/// Why an effect did not apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyFailure {
    UnknownEffect,
    UnknownActor,
    /// The target misses a required tag or holds a blocked one.
    ApplicationTags,
    /// The stack is full and the effect denies overflow.
    StackOverflow,
    /// The target holds [`MAX_EFFECTS_PER_ACTOR`] active effects.
    Capacity,
}

impl ApplyFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownEffect => "unknown_effect",
            Self::UnknownActor => "unknown_actor",
            Self::ApplicationTags => "application_tags",
            Self::StackOverflow => "stack_overflow",
            Self::Capacity => "capacity",
        }
    }
}

/// Why an ability did not activate or commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailReason {
    UnknownActor,
    UnknownAbility,
    NotGranted,
    AlreadyActive,
    /// An active ability blocks abilities with this one's tags.
    Blocked,
    OwnerTags,
    SourceTags,
    TargetTags,
    NoTarget,
    Cooldown,
    Cost,
    /// The native behaviour is missing, failed to build or refused `can_activate`.
    Behaviour,
    Capacity,
}

impl FailReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownActor => "unknown_actor",
            Self::UnknownAbility => "unknown_ability",
            Self::NotGranted => "not_granted",
            Self::AlreadyActive => "already_active",
            Self::Blocked => "blocked",
            Self::OwnerTags => "owner_tags",
            Self::SourceTags => "source_tags",
            Self::TargetTags => "target_tags",
            Self::NoTarget => "no_target",
            Self::Cooldown => "cooldown",
            Self::Cost => "cost",
            Self::Behaviour => "behaviour",
            Self::Capacity => "capacity",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveReason {
    Expired,
    Removed,
    RemovedByTags,
    PredictionUndone,
}

impl RemoveReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Removed => "removed",
            Self::RemovedByTags => "removed_by_tags",
            Self::PredictionUndone => "prediction_undone",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    Requested,
    CancelledByTags,
    CommitFailed,
    TargetLost,
    Behaviour,
    PredictionUndone,
    Revoked,
    ActorRemoved,
    Shutdown,
}

impl CancelReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::CancelledByTags => "cancelled_by_tags",
            Self::CommitFailed => "commit_failed",
            Self::TargetLost => "target_lost",
            Self::Behaviour => "behaviour",
            Self::PredictionUndone => "prediction_undone",
            Self::Revoked => "revoked",
            Self::ActorRemoved => "actor_removed",
            Self::Shutdown => "shutdown",
        }
    }
}

/// Lifecycle state of an ability instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Active,
    Ended,
    Cancelled(CancelReason),
}

/// Everything observable that happened, in order. Drain with
/// [`AbilitySystem::drain_events`].
#[derive(Clone, Debug, PartialEq)]
pub enum GameplayEvent {
    AbilityActivated {
        actor: ActorId,
        ability: String,
        instance: InstanceId,
        target: Option<ActorId>,
        predicted: bool,
    },
    AbilityCommitted {
        actor: ActorId,
        ability: String,
        instance: InstanceId,
    },
    AbilityEnded {
        actor: ActorId,
        ability: String,
        instance: InstanceId,
    },
    AbilityCancelled {
        actor: ActorId,
        ability: String,
        instance: InstanceId,
        reason: CancelReason,
    },
    AbilityFailed {
        actor: ActorId,
        ability: String,
        reason: FailReason,
    },
    EffectApplied {
        target: ActorId,
        source: ActorId,
        effect: String,
        /// `None` for an executed instant effect.
        handle: Option<EffectHandle>,
        stacks: u32,
        predicted: bool,
    },
    EffectRemoved {
        target: ActorId,
        effect: String,
        handle: EffectHandle,
        reason: RemoveReason,
    },
    /// The current value changed (bitwise).
    AttributeChanged {
        actor: ActorId,
        attribute: String,
        old: f32,
        new: f32,
        base: f32,
    },
    /// An `emit_event` step.
    Custom {
        actor: ActorId,
        event: String,
        payload: Value,
    },
}

/// Read-only view of an active effect.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EffectInfo {
    pub handle: EffectHandle,
    pub effect: String,
    pub source: ActorId,
    pub level: u32,
    pub stacks: u32,
    /// Seconds left; `None` for infinite effects.
    pub remaining: Option<f32>,
    pub executions: u64,
    pub inhibited: bool,
    pub predicted: bool,
}

/// Initial state of an actor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActorInit {
    /// Attribute set names, in evaluation order.
    pub sets: Vec<String>,
    /// Initial base values overriding the definitions.
    pub attributes: BTreeMap<String, f32>,
    /// Granted abilities and their levels.
    pub abilities: Vec<(String, u32)>,
    /// Loose tags.
    pub tags: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveEffect {
    pub(crate) handle: EffectHandle,
    pub(crate) def: usize,
    pub(crate) spec: EffectSpec,
    /// Final magnitudes fixed at application (snapshot captures, or every modifier of a
    /// predicted instant stand-in).
    pub(crate) captures: Vec<Option<f32>>,
    pub(crate) start: f64,
    pub(crate) duration: Option<f64>,
    pub(crate) anchor: f64,
    pub(crate) periods: u64,
    pub(crate) executions: u64,
    pub(crate) stacks: u32,
    pub(crate) inhibited: bool,
    pub(crate) predicted: Option<PredictionKey>,
    /// A predicted instant effect, held as a modifier on current values until confirmed.
    pub(crate) stand_in: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Slot {
    pub(crate) name: String,
    pub(crate) set: usize,
    pub(crate) index: usize,
    pub(crate) value: AttributeValue,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Actor {
    pub(crate) slots: Vec<Slot>,
    pub(crate) lookup: BTreeMap<String, usize>,
    pub(crate) loose: TagContainer,
    /// Loose + granted by effects + owned by active abilities.
    pub(crate) tags: TagContainer,
    /// Block tags of active abilities.
    pub(crate) blocked: TagContainer,
    pub(crate) effects: Vec<ActiveEffect>,
    pub(crate) granted: BTreeMap<String, u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct Instance {
    pub(crate) actor: ActorId,
    pub(crate) def: usize,
    pub(crate) level: u32,
    pub(crate) source: ActorId,
    pub(crate) target: Option<ActorId>,
    pub(crate) key: Option<PredictionKey>,
    pub(crate) committed: bool,
    pub(crate) step: usize,
    pub(crate) wait: Option<f64>,
}

#[derive(Clone, Debug)]
pub(crate) enum JournalOp {
    Added {
        actor: ActorId,
        handle: EffectHandle,
    },
    Replaced {
        actor: ActorId,
        before: Box<ActiveEffect>,
    },
    Removed {
        actor: ActorId,
        effect: Box<ActiveEffect>,
    },
    Activated {
        instance: InstanceId,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct EndedNative {
    pub(crate) instance: InstanceId,
    pub(crate) actor: ActorId,
    pub(crate) def: usize,
    pub(crate) target: Option<ActorId>,
    pub(crate) cancelled: bool,
}

/// The whole ability-system state of a game.
#[derive(Clone, Debug)]
pub struct AbilitySystem {
    pub(crate) library: Arc<Library>,
    pub(crate) actors: BTreeMap<ActorId, Actor>,
    pub(crate) instances: BTreeMap<InstanceId, Instance>,
    pub(crate) outcomes: VecDeque<(InstanceId, Outcome)>,
    pub(crate) journals: BTreeMap<PredictionKey, Vec<JournalOp>>,
    pub(crate) events: Vec<GameplayEvent>,
    pub(crate) ended_natives: Vec<EndedNative>,
    pub(crate) now: f64,
    next_handle: u64,
    pub(crate) next_instance: u64,
    dropped_events: u64,
}

fn clamp(v: f32, lo: Option<f32>, hi: Option<f32>) -> f32 {
    let v = lo.map_or(v, |lo| v.max(lo));
    hi.map_or(v, |hi| v.min(hi))
}

/// `base` after one modifier execution of `stacks` stacks: a stack of n behaves like n
/// separate applications.
pub(crate) fn execute_op(op: ModOp, base: f32, magnitude: f32, stacks: f32) -> f32 {
    match op {
        ModOp::Add => base + magnitude * stacks,
        ModOp::Multiply => base * (1.0 + (magnitude - 1.0) * stacks),
        ModOp::Divide => {
            let d = 1.0 + (magnitude - 1.0) * stacks;
            if d == 0.0 { base } else { base / d }
        }
        ModOp::Override => magnitude,
    }
}

impl AbilitySystem {
    pub fn new(library: Arc<Library>) -> Self {
        Self {
            library,
            actors: BTreeMap::new(),
            instances: BTreeMap::new(),
            outcomes: VecDeque::new(),
            journals: BTreeMap::new(),
            events: Vec::new(),
            ended_natives: Vec::new(),
            now: 0.0,
            next_handle: 1,
            next_instance: 1,
            dropped_events: 0,
        }
    }

    pub fn library(&self) -> &Arc<Library> {
        &self.library
    }

    /// Seconds advanced by [`Self::tick_effects`].
    pub fn now(&self) -> f64 {
        self.now
    }

    pub(crate) fn push(&mut self, event: GameplayEvent) {
        if self.events.len() >= MAX_PENDING_EVENTS {
            self.events.remove(0);
            self.dropped_events += 1;
        }
        self.events.push(event);
    }

    /// Takes every event since the last drain, in order.
    pub fn drain_events(&mut self) -> Vec<GameplayEvent> {
        std::mem::take(&mut self.events)
    }

    /// Events dropped because nobody drained them.
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }

    /// Prediction keys with side effects not yet committed or undone.
    pub fn pending_predictions(&self) -> usize {
        self.journals.len()
    }

    // ---------------------------------------------------------------- actors

    /// Adds an actor with its attribute sets, granted abilities and loose tags.
    ///
    /// # Errors
    /// Unknown names, an existing id, invalid tags or the actor limit.
    pub fn add_actor(&mut self, id: ActorId, init: &ActorInit) -> Result<(), String> {
        if self.actors.contains_key(&id) {
            return Err(format!("actor {id} already exists"));
        }
        if self.actors.len() >= MAX_ACTORS {
            return Err(format!("at most {MAX_ACTORS} actors"));
        }
        if init.tags.len() > MAX_LOOSE_TAGS {
            return Err(format!("at most {MAX_LOOSE_TAGS} tags"));
        }
        let mut actor = Actor::default();
        for name in &init.sets {
            let (set, def) = self
                .library
                .set(name)
                .ok_or_else(|| format!("unknown attribute set {name:?}"))?;
            if actor.slots.iter().any(|s| s.set == set) {
                return Err(format!("attribute set {name:?} is listed twice"));
            }
            for (index, attribute) in def.attributes.iter().enumerate() {
                actor
                    .lookup
                    .insert(attribute.name.clone(), actor.slots.len());
                actor.slots.push(Slot {
                    name: attribute.name.clone(),
                    set,
                    index,
                    value: AttributeValue {
                        base: attribute.base,
                        current: attribute.base,
                    },
                });
            }
        }
        for (name, value) in &init.attributes {
            let &i = actor
                .lookup
                .get(name)
                .ok_or_else(|| format!("attribute {name:?} is not in the actor's sets"))?;
            if !value.is_finite() {
                return Err(format!("attribute {name:?} must be finite"));
            }
            actor.slots[i].value = AttributeValue {
                base: *value,
                current: *value,
            };
        }
        for (name, level) in &init.abilities {
            if self.library.ability(name).is_none() || *level == 0 {
                return Err(format!("unknown ability {name:?} or level 0"));
            }
            actor.granted.insert(name.clone(), *level);
        }
        for tag in &init.tags {
            if !valid_tag(tag) {
                return Err(format!("invalid tag {tag:?}"));
            }
            actor.loose.add(tag);
        }
        // Clamp initial bases in evaluation order.
        for i in 0..actor.slots.len() {
            let (lo, hi) = self.bounds(&actor, i, None);
            let v = clamp(actor.slots[i].value.base, lo, hi);
            actor.slots[i].value = AttributeValue {
                base: v,
                current: v,
            };
        }
        actor.tags = actor.loose.clone();
        self.actors.insert(id, actor);
        self.recompute(id, false);
        Ok(())
    }

    /// Removes an actor: its ability instances are cancelled, its effects dropped (no
    /// removal events).
    pub fn remove_actor(&mut self, id: ActorId) -> bool {
        let ids: Vec<InstanceId> = self
            .instances
            .iter()
            .filter(|(_, i)| i.actor == id)
            .map(|(k, _)| *k)
            .collect();
        for instance in ids {
            self.finish(instance, Outcome::Cancelled(CancelReason::ActorRemoved));
        }
        self.actors.remove(&id).is_some()
    }

    pub fn has_actor(&self, id: ActorId) -> bool {
        self.actors.contains_key(&id)
    }

    pub fn actor_ids(&self) -> Vec<ActorId> {
        self.actors.keys().copied().collect()
    }

    // ------------------------------------------------------------ attributes

    pub fn attribute(&self, actor: ActorId, name: &str) -> Option<AttributeValue> {
        let actor = self.actors.get(&actor)?;
        actor.lookup.get(name).map(|&i| actor.slots[i].value)
    }

    /// Every attribute of the actor in evaluation order.
    pub fn attributes(&self, actor: ActorId) -> Vec<(String, AttributeValue)> {
        self.actors.get(&actor).map_or_else(Vec::new, |a| {
            a.slots.iter().map(|s| (s.name.clone(), s.value)).collect()
        })
    }

    /// Sets a base value (clamped) and re-aggregates; false for unknown names or a
    /// non-finite value.
    pub fn set_base(&mut self, actor: ActorId, name: &str, value: f32) -> bool {
        if !value.is_finite() {
            return false;
        }
        let Some(a) = self.actors.get(&actor) else {
            return false;
        };
        let Some(&i) = a.lookup.get(name) else {
            return false;
        };
        let (lo, hi) = self.bounds(a, i, None);
        let v = clamp(value, lo, hi);
        if let Some(a) = self.actors.get_mut(&actor) {
            a.slots[i].value.base = v;
        }
        self.recompute(actor, true);
        true
    }

    /// Clamp bounds of slot `i`, reading bound attributes from `current` when given (the
    /// values being recomputed) or the stored current values.
    fn bounds(
        &self,
        actor: &Actor,
        i: usize,
        current: Option<&[f32]>,
    ) -> (Option<f32>, Option<f32>) {
        let slot = &actor.slots[i];
        let def = &self.library.set_at(slot.set).attributes[slot.index];
        let read = |b: &Option<Bound>| match b {
            None => None,
            Some(Bound::Value(v)) => Some(*v),
            Some(Bound::Attribute(name)) => actor.lookup.get(name).map(|&j| match current {
                Some(c) => c[j],
                None => actor.slots[j].value.current,
            }),
        };
        (read(&def.min), read(&def.max))
    }

    /// Reads an attribute's current value; `local` overrides one actor's values with the
    /// in-progress aggregation.
    fn read(&self, actor: ActorId, name: &str, local: Option<(ActorId, &[f32])>) -> Option<f32> {
        let a = self.actors.get(&actor)?;
        let &i = a.lookup.get(name)?;
        Some(match local {
            Some((who, values)) if who == actor => values[i],
            _ => a.slots[i].value.current,
        })
    }

    pub(crate) fn magnitude(
        &self,
        modifier: &ModifierDef,
        captured: Option<f32>,
        spec: &EffectSpec,
        target: ActorId,
        local: Option<(ActorId, &[f32])>,
    ) -> f32 {
        if let Some(v) = captured {
            return v;
        }
        match &modifier.magnitude {
            Magnitude::Scalable(s) => s.at(spec.level),
            Magnitude::AttributeBased(a) => {
                let who = match a.from {
                    CaptureFrom::Source => spec.source,
                    CaptureFrom::Target => target,
                };
                // A missing actor or attribute reads as 0.
                a.from_value(self.read(who, &a.attribute, local).unwrap_or(0.0))
            }
        }
    }

    /// Re-aggregates every current value of `id` in evaluation order:
    /// `override` (latest applied wins) or `((base + Σadd) * (1 + Σ(mul-1))) / (1 + Σ(div-1))`,
    /// then the clamp bounds.
    pub(crate) fn recompute(&mut self, id: ActorId, notify: bool) {
        let Some(actor) = self.actors.get(&id) else {
            return;
        };
        let library = self.library.clone();
        let mut current: Vec<f32> = actor.slots.iter().map(|s| s.value.current).collect();
        for i in 0..actor.slots.len() {
            let slot = &actor.slots[i];
            let (mut add, mut mul, mut div) = (0.0f32, 0.0f32, 0.0f32);
            let mut overridden = None;
            for effect in &actor.effects {
                let def = library.effect_at(effect.def);
                if effect.inhibited || !(effect.stand_in || aggregates(def)) {
                    continue;
                }
                let stacks = effect.stacks as f32;
                for (mi, modifier) in def.modifiers.iter().enumerate() {
                    if modifier.attribute != slot.name {
                        continue;
                    }
                    let m = self.magnitude(
                        modifier,
                        effect.captures[mi],
                        &effect.spec,
                        id,
                        Some((id, &current)),
                    );
                    match modifier.op {
                        ModOp::Add => add += m * stacks,
                        ModOp::Multiply => mul += (m - 1.0) * stacks,
                        ModOp::Divide => div += (m - 1.0) * stacks,
                        ModOp::Override => overridden = Some(m),
                    }
                }
            }
            let base = slot.value.base;
            let v = overridden.unwrap_or_else(|| {
                let v = (base + add) * (1.0 + mul);
                let d = 1.0 + div;
                if d == 0.0 { v } else { v / d }
            });
            let (lo, hi) = self.bounds(actor, i, Some(&current));
            let v = clamp(v, lo, hi);
            current[i] = if v.is_finite() { v } else { base };
        }
        let mut changes = Vec::new();
        if let Some(actor) = self.actors.get_mut(&id) {
            for (slot, value) in actor.slots.iter_mut().zip(current) {
                if slot.value.current.to_bits() != value.to_bits() {
                    changes.push(GameplayEvent::AttributeChanged {
                        actor: id,
                        attribute: slot.name.clone(),
                        old: slot.value.current,
                        new: value,
                        base: slot.value.base,
                    });
                    slot.value.current = value;
                }
            }
        }
        if notify {
            for event in changes {
                self.push(event);
            }
        }
    }

    // ------------------------------------------------------------------ tags

    pub fn tags(&self, actor: ActorId) -> Option<&TagContainer> {
        self.actors.get(&actor).map(|a| &a.tags)
    }

    /// Owned tags (loose, granted by effects, owned by active abilities).
    pub fn has_tag(&self, actor: ActorId, tag: &str, exact: bool) -> bool {
        self.actors
            .get(&actor)
            .is_some_and(|a| a.tags.has(tag, exact))
    }

    /// Adds one count of a loose tag.
    ///
    /// # Errors
    /// Unknown actor, invalid tag or [`MAX_LOOSE_TAGS`] distinct loose tags.
    pub fn add_loose_tag(&mut self, actor: ActorId, tag: &str) -> Result<(), String> {
        if !valid_tag(tag) {
            return Err(format!("invalid tag {tag:?}"));
        }
        let a = self
            .actors
            .get_mut(&actor)
            .ok_or_else(|| format!("unknown actor {actor}"))?;
        if a.loose.count(tag) == 0 && a.loose.len() >= MAX_LOOSE_TAGS {
            return Err(format!("at most {MAX_LOOSE_TAGS} loose tags"));
        }
        a.loose.add(tag);
        self.refresh(actor);
        Ok(())
    }

    pub fn remove_loose_tag(&mut self, actor: ActorId, tag: &str) -> bool {
        let removed = self
            .actors
            .get_mut(&actor)
            .is_some_and(|a| a.loose.remove(tag));
        if removed {
            self.refresh(actor);
        }
        removed
    }

    fn rebuild_tags(&mut self, id: ActorId) {
        let library = &self.library;
        let Some(actor) = self.actors.get_mut(&id) else {
            return;
        };
        let mut tags = actor.loose.clone();
        for effect in &actor.effects {
            if !effect.inhibited {
                tags.add_all(&library.effect_at(effect.def).granted_tags);
            }
        }
        let mut blocked = TagContainer::new();
        for instance in self.instances.values().filter(|i| i.actor == id) {
            let def = library.ability_at(instance.def);
            tags.add_all(&def.activation_owned_tags);
            blocked.add_all(&def.block_abilities_with_tags);
        }
        actor.tags = tags;
        actor.blocked = blocked;
    }

    /// Rebuilds tags and re-evaluates ongoing requirements in handle order; resumed
    /// periodic effects follow their inhibition policy.
    fn update_inhibition(&mut self, id: ActorId) {
        self.rebuild_tags(id);
        let library = self.library.clone();
        let now = self.now;
        let mut execute = Vec::new();
        let count = self.actors.get(&id).map_or(0, |a| a.effects.len());
        for i in 0..count {
            let Some(actor) = self.actors.get_mut(&id) else {
                return;
            };
            let def = library.effect_at(actor.effects[i].def);
            if def.ongoing.is_empty() {
                continue;
            }
            let met = def.ongoing.met(&actor.tags);
            let effect = &mut actor.effects[i];
            if met != effect.inhibited {
                continue;
            }
            effect.inhibited = !met;
            if met && def.is_periodic() {
                match def.inhibition {
                    InhibitionPolicy::NeverReset => {}
                    InhibitionPolicy::ResetPeriod => {
                        effect.anchor = now;
                        effect.periods = 0;
                    }
                    InhibitionPolicy::ExecuteAndResetPeriod => {
                        effect.anchor = now;
                        effect.periods = 0;
                        if effect.predicted.is_none() {
                            effect.executions += 1;
                            execute.push(effect.handle);
                        }
                    }
                }
            }
            self.rebuild_tags(id);
        }
        for handle in execute {
            if let Some(e) = self.effect(id, handle).cloned() {
                self.execute(id, e.def, &e.spec, &e.captures, e.stacks);
            }
        }
    }

    /// Tags, inhibition and current values after a change on `id`.
    pub(crate) fn refresh(&mut self, id: ActorId) {
        self.update_inhibition(id);
        self.recompute(id, true);
    }

    // --------------------------------------------------------------- effects

    pub(crate) fn effect(&self, actor: ActorId, handle: EffectHandle) -> Option<&ActiveEffect> {
        let a = self.actors.get(&actor)?;
        a.effects
            .binary_search_by_key(&handle, |e| e.handle)
            .ok()
            .map(|i| &a.effects[i])
    }

    fn effect_mut(&mut self, actor: ActorId, handle: EffectHandle) -> Option<&mut ActiveEffect> {
        let a = self.actors.get_mut(&actor)?;
        let i = a.effects.binary_search_by_key(&handle, |e| e.handle).ok()?;
        Some(&mut a.effects[i])
    }

    fn insert_effect(&mut self, actor: ActorId, effect: ActiveEffect) {
        if let Some(a) = self.actors.get_mut(&actor) {
            match a.effects.binary_search_by_key(&effect.handle, |e| e.handle) {
                Ok(i) => a.effects[i] = effect,
                Err(i) => a.effects.insert(i, effect),
            }
        }
    }

    fn journal(&mut self, key: Option<PredictionKey>, op: JournalOp) {
        if let Some(key) = key {
            self.journals.entry(key).or_default().push(op);
        }
    }

    /// Active effects on `actor`, in handle order.
    pub fn effects(&self, actor: ActorId) -> Vec<EffectInfo> {
        let Some(a) = self.actors.get(&actor) else {
            return Vec::new();
        };
        a.effects
            .iter()
            .map(|e| EffectInfo {
                handle: e.handle,
                effect: e.spec.effect.clone(),
                source: e.spec.source,
                level: e.spec.level,
                stacks: e.stacks,
                remaining: e.duration.map(|d| (e.start + d - self.now).max(0.0) as f32),
                executions: e.executions,
                inhibited: e.inhibited,
                predicted: e.predicted.is_some(),
            })
            .collect()
    }

    /// Builds a spec of `effect` from `source` at `level`, capturing the source's snapshot
    /// attributes now.
    ///
    /// # Errors
    /// Unknown effect name.
    pub fn make_spec(
        &self,
        effect: &str,
        source: ActorId,
        level: u32,
    ) -> Result<EffectSpec, ApplyFailure> {
        let (def, _) = self
            .library
            .effect(effect)
            .ok_or(ApplyFailure::UnknownEffect)?;
        Ok(self.spec_at(def, source, level, None))
    }

    pub(crate) fn spec_at(
        &self,
        def: usize,
        source: ActorId,
        level: u32,
        key: Option<PredictionKey>,
    ) -> EffectSpec {
        let effect = self.library.effect_at(def);
        let mut spec = EffectSpec {
            effect: effect.name.clone(),
            level: level.max(1),
            source,
            key,
            def,
            source_captures: Vec::new(),
        };
        spec.source_captures = effect
            .modifiers
            .iter()
            .map(|m| match &m.magnitude {
                Magnitude::AttributeBased(a)
                    if a.from == CaptureFrom::Source && a.capture == Capture::Snapshot =>
                {
                    Some(self.magnitude(m, None, &spec, source, None))
                }
                _ => None,
            })
            .collect();
        spec
    }

    /// Shorthand for [`Self::make_spec`] + [`Self::apply_spec`].
    pub fn apply_effect(
        &mut self,
        source: ActorId,
        target: ActorId,
        effect: &str,
        level: u32,
        key: Option<PredictionKey>,
    ) -> Result<Option<EffectHandle>, ApplyFailure> {
        let mut spec = self.make_spec(effect, source, level)?;
        spec.key = key;
        self.apply_spec(spec, target)
    }

    /// Applies a spec to `target`. Instant effects execute on base values and return
    /// `None` (a predicted instant effect returns the handle of its stand-in); duration and
    /// infinite effects return the handle of the new or stacked active effect.
    ///
    /// # Errors
    /// See [`ApplyFailure`]; a failure changes nothing.
    pub fn apply_spec(
        &mut self,
        spec: EffectSpec,
        target: ActorId,
    ) -> Result<Option<EffectHandle>, ApplyFailure> {
        let library = self.library.clone();
        let def = library
            .definitions()
            .effects
            .get(spec.def)
            .ok_or(ApplyFailure::UnknownEffect)?;
        let actor = self.actors.get(&target).ok_or(ApplyFailure::UnknownActor)?;
        if !def.application.met(&actor.tags) {
            return Err(ApplyFailure::ApplicationTags);
        }
        let key = spec.key;
        let existing = (def.policy != DurationPolicy::Instant
            && def.stacking.kind != StackKind::None)
            .then(|| {
                actor.effects.iter().find(|e| {
                    !e.stand_in
                        && e.def == spec.def
                        && (def.stacking.kind == StackKind::ByTarget
                            || e.spec.source == spec.source)
                })
            })
            .flatten()
            .map(|e| (e.handle, e.stacks));
        let adds_effect =
            existing.is_none() && (def.policy != DurationPolicy::Instant || key.is_some());
        if adds_effect && actor.effects.len() >= MAX_EFFECTS_PER_ACTOR {
            return Err(ApplyFailure::Capacity);
        }
        if let Some((_, stacks)) = existing
            && stacks >= def.stacking.limit
            && def.stacking.deny_overflow
        {
            return Err(ApplyFailure::StackOverflow);
        }
        if !def.remove_effects_with_tags.is_empty() {
            self.remove_matching(target, &def.remove_effects_with_tags, key, Some(spec.def));
        }
        let now = self.now;
        if let Some((handle, _)) = existing {
            let Some(effect) = self.effect_mut(target, handle) else {
                return Err(ApplyFailure::UnknownActor);
            };
            let before = effect.clone();
            if effect.stacks < def.stacking.limit {
                effect.stacks += 1;
            }
            if def.stacking.duration_refresh == DurationRefresh::RefreshOnSuccess {
                effect.start = now;
            }
            if def.stacking.period_reset == PeriodReset::ResetOnSuccess {
                effect.anchor = now;
                effect.periods = 0;
            }
            let stacks = effect.stacks;
            self.journal(
                key,
                JournalOp::Replaced {
                    actor: target,
                    before: Box::new(before),
                },
            );
            self.push(GameplayEvent::EffectApplied {
                target,
                source: spec.source,
                effect: def.name.clone(),
                handle: Some(handle),
                stacks,
                predicted: key.is_some(),
            });
            self.refresh(target);
            return Ok(Some(handle));
        }
        if def.policy == DurationPolicy::Instant && key.is_none() {
            self.push(GameplayEvent::EffectApplied {
                target,
                source: spec.source,
                effect: def.name.clone(),
                handle: None,
                stacks: 1,
                predicted: false,
            });
            let captures = self.captures(def, &spec, target, false);
            self.execute(target, spec.def, &spec, &captures, 1);
            return Ok(None);
        }
        let stand_in = def.policy == DurationPolicy::Instant;
        let captures = self.captures(def, &spec, target, stand_in);
        let handle = EffectHandle(self.next_handle);
        self.next_handle += 1;
        let inhibited = !def.ongoing.met(&self.actors[&target].tags);
        let source = spec.source;
        let effect = ActiveEffect {
            handle,
            def: spec.def,
            spec,
            captures,
            start: now,
            duration: (def.policy == DurationPolicy::Duration).then_some(f64::from(def.duration)),
            anchor: now,
            periods: 0,
            executions: 0,
            stacks: 1,
            inhibited,
            predicted: key,
            stand_in,
        };
        self.insert_effect(target, effect);
        self.journal(
            key,
            JournalOp::Added {
                actor: target,
                handle,
            },
        );
        self.push(GameplayEvent::EffectApplied {
            target,
            source,
            effect: def.name.clone(),
            handle: Some(handle),
            stacks: 1,
            predicted: key.is_some(),
        });
        self.update_inhibition(target);
        if def.is_periodic()
            && def.execute_on_application
            && key.is_none()
            && let Some(e) = self.effect_mut(target, handle)
            && !e.inhibited
        {
            e.executions += 1;
            let e = e.clone();
            self.execute(target, e.def, &e.spec, &e.captures, e.stacks);
        }
        self.recompute(target, true);
        Ok(Some(handle))
    }

    /// Final magnitudes fixed at application: the spec's source snapshots, target snapshots,
    /// and with `all` every other modifier too.
    fn captures(
        &self,
        def: &EffectDef,
        spec: &EffectSpec,
        target: ActorId,
        all: bool,
    ) -> Vec<Option<f32>> {
        def.modifiers
            .iter()
            .enumerate()
            .map(|(i, m)| {
                if let Some(v) = spec.source_captures.get(i).copied().flatten() {
                    return Some(v);
                }
                let snapshot_target = matches!(&m.magnitude, Magnitude::AttributeBased(a)
                    if a.from == CaptureFrom::Target && a.capture == Capture::Snapshot);
                (all || snapshot_target).then(|| self.magnitude(m, None, spec, target, None))
            })
            .collect()
    }

    /// Runs an effect's modifiers on base values once.
    fn execute(
        &mut self,
        target: ActorId,
        def: usize,
        spec: &EffectSpec,
        captures: &[Option<f32>],
        stacks: u32,
    ) {
        let library = self.library.clone();
        for (mi, modifier) in library.effect_at(def).modifiers.iter().enumerate() {
            let m = self.magnitude(
                modifier,
                captures.get(mi).copied().flatten(),
                spec,
                target,
                None,
            );
            let Some(actor) = self.actors.get(&target) else {
                return;
            };
            let Some(&i) = actor.lookup.get(&modifier.attribute) else {
                continue;
            };
            let base = actor.slots[i].value.base;
            let (lo, hi) = self.bounds(actor, i, None);
            let v = clamp(execute_op(modifier.op, base, m, stacks as f32), lo, hi);
            if let Some(actor) = self.actors.get_mut(&target) {
                actor.slots[i].value.base = if v.is_finite() { v } else { base };
            }
        }
        self.recompute(target, true);
    }

    fn remove_internal(
        &mut self,
        actor: ActorId,
        handle: EffectHandle,
        reason: RemoveReason,
    ) -> Option<ActiveEffect> {
        let a = self.actors.get_mut(&actor)?;
        let i = a.effects.binary_search_by_key(&handle, |e| e.handle).ok()?;
        let effect = a.effects.remove(i);
        self.push(GameplayEvent::EffectRemoved {
            target: actor,
            effect: effect.spec.effect.clone(),
            handle,
            reason,
        });
        Some(effect)
    }

    fn remove_matching(
        &mut self,
        target: ActorId,
        tags: &[String],
        key: Option<PredictionKey>,
        except_def: Option<usize>,
    ) -> usize {
        let library = self.library.clone();
        let Some(actor) = self.actors.get(&target) else {
            return 0;
        };
        let doomed: Vec<EffectHandle> = actor
            .effects
            .iter()
            .filter(|e| Some(e.def) != except_def)
            .filter(|e| {
                let def = library.effect_at(e.def);
                def.asset_tags
                    .iter()
                    .chain(&def.granted_tags)
                    .any(|t| tags.iter().any(|q| tag_matches(t, q)))
            })
            .map(|e| e.handle)
            .collect();
        for &handle in &doomed {
            if let Some(effect) = self.remove_internal(target, handle, RemoveReason::RemovedByTags)
            {
                self.journal(
                    key,
                    JournalOp::Removed {
                        actor: target,
                        effect: Box::new(effect),
                    },
                );
            }
        }
        if !doomed.is_empty() {
            self.refresh(target);
        }
        doomed.len()
    }

    /// Removes one active effect.
    pub fn remove_effect(&mut self, target: ActorId, handle: EffectHandle) -> bool {
        let removed = self
            .remove_internal(target, handle, RemoveReason::Removed)
            .is_some();
        if removed {
            self.refresh(target);
        }
        removed
    }

    /// Removes active effects whose asset or granted tags match any of `tags`
    /// (hierarchically).
    pub fn remove_effects_with_tags(&mut self, target: ActorId, tags: &[String]) -> usize {
        self.remove_matching(target, tags, None, None)
    }

    /// Advances time: periodic executions (in actor then handle order), expiry and stack
    /// expiration, then re-aggregates every actor so live captures across actors settle.
    pub fn tick_effects(&mut self, dt: f32) {
        if !(dt.is_finite() && dt > 0.0) {
            return;
        }
        self.now += f64::from(dt);
        let now = self.now;
        let library = self.library.clone();
        let ids = self.actor_ids();
        for id in &ids {
            let id = *id;
            self.update_inhibition(id);
            let handles: Vec<EffectHandle> =
                self.actors[&id].effects.iter().map(|e| e.handle).collect();
            let mut removed = false;
            for handle in handles {
                let Some(effect) = self.effect_mut(id, handle) else {
                    continue;
                };
                let def = library.effect_at(effect.def);
                if def.is_periodic() {
                    let end = effect.duration.map_or(now, |d| now.min(effect.start + d));
                    let period = f64::from(def.period);
                    let mut due = 0;
                    while effect.anchor + (effect.periods + 1) as f64 * period <= end + TIME_EPSILON
                    {
                        if due == MAX_EXECUTIONS_PER_TICK {
                            effect.periods =
                                ((end + TIME_EPSILON - effect.anchor) / period).floor() as u64;
                            break;
                        }
                        effect.periods += 1;
                        due += 1;
                    }
                    if due > 0 && !effect.inhibited && effect.predicted.is_none() {
                        effect.executions += due;
                        let e = effect.clone();
                        for _ in 0..due {
                            self.execute(id, e.def, &e.spec, &e.captures, e.stacks);
                        }
                    }
                }
                let Some(effect) = self.effect_mut(id, handle) else {
                    continue;
                };
                let Some(duration) = effect.duration else {
                    continue;
                };
                if now + TIME_EPSILON < effect.start + duration {
                    continue;
                }
                match def.stacking.expiration {
                    StackExpiration::RemoveSingleStack if effect.stacks > 1 => {
                        effect.stacks -= 1;
                        effect.start = now;
                    }
                    StackExpiration::RefreshDuration => effect.start = now,
                    _ => {
                        self.remove_internal(id, handle, RemoveReason::Expired);
                        removed = true;
                    }
                }
            }
            if removed {
                self.update_inhibition(id);
            }
        }
        for id in ids {
            self.recompute(id, true);
        }
    }
}

/// Non-periodic duration and infinite effects modify current values.
fn aggregates(def: &EffectDef) -> bool {
    def.policy != DurationPolicy::Instant && def.period <= 0.0
}

impl PredictionHooks for AbilitySystem {
    /// Keeps the key's side effects: predicted instant stand-ins execute on base values,
    /// other effects and ability instances become authoritative.
    fn commit(&mut self, key: PredictionKey) {
        let Some(ops) = self.journals.remove(&key) else {
            return;
        };
        let mut touched = BTreeSet::new();
        for op in ops {
            match op {
                JournalOp::Added { actor, handle } => {
                    let Some(effect) = self.effect_mut(actor, handle) else {
                        continue;
                    };
                    if effect.predicted != Some(key) {
                        continue;
                    }
                    touched.insert(actor);
                    if effect.stand_in {
                        let a = self.actors.get_mut(&actor).expect("effect owner");
                        let i = a
                            .effects
                            .binary_search_by_key(&handle, |e| e.handle)
                            .expect("effect present");
                        let e = a.effects.remove(i);
                        self.execute(actor, e.def, &e.spec, &e.captures, 1);
                    } else {
                        effect.predicted = None;
                    }
                }
                JournalOp::Activated { instance } => {
                    if let Some(i) = self.instances.get_mut(&instance)
                        && i.key == Some(key)
                    {
                        i.key = None;
                    }
                }
                JournalOp::Replaced { .. } | JournalOp::Removed { .. } => {}
            }
        }
        for actor in touched {
            self.refresh(actor);
        }
    }

    /// Reverts the key's side effects newest first: added effects are removed, stacked or
    /// removed effects restored, predicted activations cancelled.
    fn undo(&mut self, key: PredictionKey, _reason: UndoReason) {
        let Some(ops) = self.journals.remove(&key) else {
            return;
        };
        let mut touched = BTreeSet::new();
        for op in ops.into_iter().rev() {
            match op {
                JournalOp::Added { actor, handle } => {
                    self.remove_internal(actor, handle, RemoveReason::PredictionUndone);
                    touched.insert(actor);
                }
                JournalOp::Replaced { actor, before }
                | JournalOp::Removed {
                    actor,
                    effect: before,
                } => {
                    self.insert_effect(actor, *before);
                    touched.insert(actor);
                }
                JournalOp::Activated { instance } => {
                    if let Some(i) = self.instances.get(&instance) {
                        touched.insert(i.actor);
                        self.finish(instance, Outcome::Cancelled(CancelReason::PredictionUndone));
                    }
                }
            }
        }
        for actor in touched {
            self.refresh(actor);
        }
    }
}
