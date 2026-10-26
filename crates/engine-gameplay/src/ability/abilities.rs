//! Ability activation, commit and lifecycle, data-only steps and native behaviours.
use super::defs::{CommitPolicy, Instancing, Step, StepTarget};
use super::system::{
    AbilitySystem, ActorId, CancelReason, EndedNative, FailReason, GameplayEvent, Instance,
    InstanceId, JournalOp, MAX_INSTANCES_PER_ACTOR, MAX_OUTCOMES, Outcome, TIME_EPSILON,
};
use super::tags::tag_matches;
use engine_net_predict::PredictionKey;
use serde_json::Value;
use std::collections::BTreeMap;

/// A request to activate a granted ability.
#[derive(Clone, Debug, PartialEq)]
pub struct ActivationRequest {
    pub actor: ActorId,
    pub ability: String,
    pub target: Option<ActorId>,
    /// Instigator whose tags `source` requirements check; the owner when absent.
    pub source: Option<ActorId>,
    /// Client prediction: every side effect is tagged with this key.
    pub key: Option<PredictionKey>,
}

impl ActivationRequest {
    pub fn new(actor: ActorId, ability: &str) -> Self {
        Self {
            actor,
            ability: ability.to_owned(),
            target: None,
            source: None,
            key: None,
        }
    }
    pub fn with_target(mut self, target: ActorId) -> Self {
        self.target = Some(target);
        self
    }
    pub fn with_source(mut self, source: ActorId) -> Self {
        self.source = Some(source);
        self
    }
    pub fn with_key(mut self, key: PredictionKey) -> Self {
        self.key = Some(key);
        self
    }
}

/// Read-only view of an active ability instance.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceInfo {
    pub id: InstanceId,
    pub actor: ActorId,
    pub ability: String,
    pub level: u32,
    /// Instigator of the activation (the owner unless a source was given).
    pub source: ActorId,
    pub target: Option<ActorId>,
    pub committed: bool,
    pub predicted: bool,
}

impl AbilitySystem {
    // --------------------------------------------------------------- granting

    /// Grants (or re-levels) an ability.
    ///
    /// # Errors
    /// Unknown actor or ability, or level 0.
    pub fn grant_ability(
        &mut self,
        actor: ActorId,
        ability: &str,
        level: u32,
    ) -> Result<(), String> {
        if self.library.ability(ability).is_none() || level == 0 {
            return Err(format!("unknown ability {ability:?} or level 0"));
        }
        let a = self
            .actors
            .get_mut(&actor)
            .ok_or_else(|| format!("unknown actor {actor}"))?;
        a.granted.insert(ability.to_owned(), level);
        Ok(())
    }

    /// Revokes an ability, cancelling its active instances.
    pub fn revoke_ability(&mut self, actor: ActorId, ability: &str) -> bool {
        let revoked = self
            .actors
            .get_mut(&actor)
            .is_some_and(|a| a.granted.remove(ability).is_some());
        if revoked {
            self.cancel_where(actor, CancelReason::Revoked, |name, _| name == ability);
        }
        revoked
    }

    /// Granted abilities and their levels, by name.
    pub fn granted(&self, actor: ActorId) -> Vec<(String, u32)> {
        self.actors.get(&actor).map_or_else(Vec::new, |a| {
            a.granted.iter().map(|(k, v)| (k.clone(), *v)).collect()
        })
    }

    /// Granted abilities triggered by the gameplay event `event`, by name.
    pub fn triggered_by(&self, actor: ActorId, event: &str) -> Vec<String> {
        let Some(a) = self.actors.get(&actor) else {
            return Vec::new();
        };
        a.granted
            .keys()
            .filter(|name| {
                self.library
                    .ability(name)
                    .is_some_and(|(_, d)| d.triggers.iter().any(|t| t == event))
            })
            .cloned()
            .collect()
    }

    // ------------------------------------------------------------- activation

    /// Every activation rule, in order: actor, ability, grant, capacity / instancing,
    /// blocked by an active ability, owner, source and target tags, target presence,
    /// cooldown, cost. Returns the ability's definition index.
    ///
    /// # Errors
    /// The first failing rule.
    pub fn can_activate(&self, request: &ActivationRequest) -> Result<usize, FailReason> {
        let actor = self
            .actors
            .get(&request.actor)
            .ok_or(FailReason::UnknownActor)?;
        let (index, def) = self
            .library
            .ability(&request.ability)
            .ok_or(FailReason::UnknownAbility)?;
        let level = *actor
            .granted
            .get(&request.ability)
            .ok_or(FailReason::NotGranted)?;
        let mut active = self.instances.values().filter(|i| i.actor == request.actor);
        if active.clone().count() >= MAX_INSTANCES_PER_ACTOR {
            return Err(FailReason::Capacity);
        }
        if def.instancing == Instancing::PerActor && active.any(|i| i.def == index) {
            return Err(FailReason::AlreadyActive);
        }
        if def
            .tags
            .iter()
            .any(|t| actor.blocked.iter().any(|(b, _)| tag_matches(t, b)))
        {
            return Err(FailReason::Blocked);
        }
        if !def.owner.met(&actor.tags) {
            return Err(FailReason::OwnerTags);
        }
        if !def.source.is_empty() {
            let source = request.source.unwrap_or(request.actor);
            if !self
                .actors
                .get(&source)
                .is_some_and(|s| def.source.met(&s.tags))
            {
                return Err(FailReason::SourceTags);
            }
        }
        let target = request.target.and_then(|t| self.actors.get(&t));
        if (def.needs_target() || request.target.is_some()) && target.is_none() {
            return Err(FailReason::NoTarget);
        }
        if !def.target.is_empty() && !target.is_some_and(|t| def.target.met(&t.tags)) {
            return Err(FailReason::TargetTags);
        }
        if self.on_cooldown(request.actor, index) {
            return Err(FailReason::Cooldown);
        }
        if !self.can_afford(request.actor, index, level) {
            return Err(FailReason::Cost);
        }
        Ok(index)
    }

    fn on_cooldown(&self, actor: ActorId, ability: usize) -> bool {
        let Some(cooldown) = &self.library.ability_at(ability).cooldown else {
            return false;
        };
        let Some((_, def)) = self.library.effect(cooldown) else {
            return false;
        };
        self.actors
            .get(&actor)
            .is_some_and(|a| a.tags.has_any(&def.granted_tags, false))
    }

    /// Every attribute the cost modifies must stay at or above 0 after one execution on its
    /// current value.
    fn can_afford(&self, actor: ActorId, ability: usize, level: u32) -> bool {
        let Some(cost) = &self.library.ability_at(ability).cost else {
            return true;
        };
        let Some((def, effect)) = self.library.effect(cost) else {
            return true;
        };
        let spec = self.spec_at(def, actor, level, None);
        effect.modifiers.iter().enumerate().all(|(i, m)| {
            let Some(value) = self.attribute(actor, &m.attribute) else {
                return true;
            };
            let magnitude = self.magnitude(m, spec.source_captures[i], &spec, actor, None);
            super::system::execute_op(m.op, value.current, magnitude, 1.0) >= 0.0
        })
    }

    /// Starts an instance after [`Self::can_activate`] passed: cancels abilities with the
    /// cancel tags, grants owned tags, journals a predicted activation.
    pub(crate) fn begin_instance(&mut self, request: &ActivationRequest, def: usize) -> InstanceId {
        let library = self.library.clone();
        let ability = library.ability_at(def);
        if !ability.cancel_abilities_with_tags.is_empty() {
            let cancel = &ability.cancel_abilities_with_tags;
            self.cancel_where(request.actor, CancelReason::CancelledByTags, |_, d| {
                library
                    .ability_at(d)
                    .tags
                    .iter()
                    .any(|t| cancel.iter().any(|c| tag_matches(t, c)))
            });
        }
        let level = self.actors[&request.actor].granted[&request.ability];
        let id = InstanceId(self.next_instance);
        self.next_instance += 1;
        self.instances.insert(
            id,
            Instance {
                actor: request.actor,
                def,
                level,
                source: request.source.unwrap_or(request.actor),
                target: request.target,
                key: request.key,
                committed: false,
                step: 0,
                wait: None,
            },
        );
        if let Some(key) = request.key {
            self.journals
                .entry(key)
                .or_default()
                .push(JournalOp::Activated { instance: id });
        }
        self.push(GameplayEvent::AbilityActivated {
            actor: request.actor,
            ability: ability.name.clone(),
            instance: id,
            target: request.target,
            predicted: request.key.is_some(),
        });
        self.refresh(request.actor);
        id
    }

    pub(crate) fn push_failed(&mut self, request: &ActivationRequest, reason: FailReason) {
        self.push(GameplayEvent::AbilityFailed {
            actor: request.actor,
            ability: request.ability.clone(),
            reason,
        });
    }

    fn pending_key(&self, key: Option<PredictionKey>) -> Option<PredictionKey> {
        key.filter(|k| self.journals.contains_key(k))
    }

    /// Commits cost and cooldown (re-checked first). Committing twice is a no-op.
    ///
    /// # Errors
    /// Unknown instance, cooldown or cost. Nothing changes on failure.
    pub fn commit(&mut self, id: InstanceId) -> Result<(), FailReason> {
        let instance = self.instances.get(&id).ok_or(FailReason::UnknownAbility)?;
        if instance.committed {
            return Ok(());
        }
        let (actor, def, level) = (instance.actor, instance.def, instance.level);
        let key = self.pending_key(instance.key);
        if self.on_cooldown(actor, def) {
            return Err(FailReason::Cooldown);
        }
        if !self.can_afford(actor, def, level) {
            return Err(FailReason::Cost);
        }
        let library = self.library.clone();
        let ability = library.ability_at(def);
        for effect in [&ability.cost, &ability.cooldown].into_iter().flatten() {
            if let Some((index, _)) = library.effect(effect) {
                let spec = self.spec_at(index, actor, level, key);
                // Requirement failures on the owner skip the effect, as in Unreal.
                let _ = self.apply_spec(spec, actor);
            }
        }
        if let Some(i) = self.instances.get_mut(&id) {
            i.committed = true;
        }
        self.push(GameplayEvent::AbilityCommitted {
            actor,
            ability: ability.name.clone(),
            instance: id,
        });
        Ok(())
    }

    /// Ends an instance normally (`ability.ended`).
    pub fn end_instance(&mut self, id: InstanceId) -> bool {
        self.finish(id, Outcome::Ended)
    }

    /// Cancels an instance (`ability.cancelled`, reason `requested`).
    pub fn cancel_instance(&mut self, id: InstanceId) -> bool {
        self.finish(id, Outcome::Cancelled(CancelReason::Requested))
    }

    /// Cancels every active instance of `ability` on `actor`.
    pub fn cancel_ability(&mut self, actor: ActorId, ability: &str) -> usize {
        self.cancel_where(actor, CancelReason::Requested, |name, _| name == ability)
    }

    /// Cancels the actor's active abilities whose tags match any of `tags`.
    pub fn cancel_abilities_with_tags(&mut self, actor: ActorId, tags: &[String]) -> usize {
        let library = self.library.clone();
        self.cancel_where(actor, CancelReason::Requested, |_, d| {
            library
                .ability_at(d)
                .tags
                .iter()
                .any(|t| tags.iter().any(|c| tag_matches(t, c)))
        })
    }

    fn cancel_where(
        &mut self,
        actor: ActorId,
        reason: CancelReason,
        matches: impl Fn(&str, usize) -> bool,
    ) -> usize {
        let doomed: Vec<InstanceId> = self
            .instances
            .iter()
            .filter(|(_, i)| {
                i.actor == actor && matches(&self.library.ability_at(i.def).name, i.def)
            })
            .map(|(k, _)| *k)
            .collect();
        for &id in &doomed {
            self.finish(id, Outcome::Cancelled(reason));
        }
        doomed.len()
    }

    /// Removes an instance and records its outcome.
    pub(crate) fn finish(&mut self, id: InstanceId, outcome: Outcome) -> bool {
        let Some(instance) = self.instances.remove(&id) else {
            return false;
        };
        let library = self.library.clone();
        let def = library.ability_at(instance.def);
        if self.outcomes.len() >= MAX_OUTCOMES {
            self.outcomes.pop_front();
        }
        self.outcomes.push_back((id, outcome));
        let (actor, ability) = (instance.actor, def.name.clone());
        let cancelled = match outcome {
            Outcome::Cancelled(reason) => {
                self.push(GameplayEvent::AbilityCancelled {
                    actor,
                    ability,
                    instance: id,
                    reason,
                });
                true
            }
            _ => {
                self.push(GameplayEvent::AbilityEnded {
                    actor,
                    ability,
                    instance: id,
                });
                false
            }
        };
        if def.native.is_some() {
            self.ended_natives.push(EndedNative {
                instance: id,
                actor,
                def: instance.def,
                target: instance.target,
                cancelled,
            });
        }
        self.refresh(actor);
        true
    }

    // ---------------------------------------------------------------- queries

    pub fn instance(&self, id: InstanceId) -> Option<InstanceInfo> {
        let i = self.instances.get(&id)?;
        Some(InstanceInfo {
            id,
            actor: i.actor,
            ability: self.library.ability_at(i.def).name.clone(),
            level: i.level,
            source: i.source,
            target: i.target,
            committed: i.committed,
            predicted: i.key.is_some(),
        })
    }

    /// Active instances of `actor`, oldest first.
    pub fn instances(&self, actor: ActorId) -> Vec<InstanceInfo> {
        self.instances
            .iter()
            .filter(|(_, i)| i.actor == actor)
            .filter_map(|(id, _)| self.instance(*id))
            .collect()
    }

    pub fn instance_ids(&self) -> Vec<InstanceId> {
        self.instances.keys().copied().collect()
    }

    pub fn is_active(&self, actor: ActorId, ability: &str) -> bool {
        self.instances
            .values()
            .any(|i| i.actor == actor && self.library.ability_at(i.def).name == ability)
    }

    /// `Active` while running; the final outcome of one of the last [`MAX_OUTCOMES`]
    /// finished instances; `None` otherwise.
    pub fn outcome(&self, id: InstanceId) -> Option<Outcome> {
        if self.instances.contains_key(&id) {
            return Some(Outcome::Active);
        }
        self.outcomes
            .iter()
            .rev()
            .find(|(k, _)| *k == id)
            .map(|(_, o)| *o)
    }

    // ------------------------------------------------------------------ steps

    /// Runs data-only steps until a wait or the end. `dt` is spent on the current wait.
    pub(crate) fn run_steps(&mut self, id: InstanceId, dt: f32) {
        let library = self.library.clone();
        let mut dt = f64::from(dt);
        loop {
            let Some(instance) = self.instances.get_mut(&id) else {
                return;
            };
            let def = library.ability_at(instance.def);
            let Some(step) = def.steps.get(instance.step) else {
                self.finish(id, Outcome::Ended);
                return;
            };
            match step {
                Step::Wait(wait) => {
                    let left = match instance.wait {
                        // A wait starts counting on the next tick.
                        None => f64::from(wait.seconds),
                        Some(left) => left - std::mem::take(&mut dt),
                    };
                    if left <= TIME_EPSILON {
                        instance.wait = None;
                        instance.step += 1;
                    } else {
                        instance.wait = Some(left);
                        return;
                    }
                }
                Step::Commit => {
                    instance.step += 1;
                    if self.commit(id).is_err() {
                        self.finish(id, Outcome::Cancelled(CancelReason::CommitFailed));
                        return;
                    }
                }
                Step::ApplyEffect(apply) => {
                    instance.step += 1;
                    let (actor, level, key) = (instance.actor, instance.level, instance.key);
                    let to = match apply.to {
                        StepTarget::Owner => Some(actor),
                        StepTarget::Target => instance.target,
                    };
                    let Some(to) = to.filter(|t| self.has_actor(*t)) else {
                        self.finish(id, Outcome::Cancelled(CancelReason::TargetLost));
                        return;
                    };
                    if let Some((index, _)) = library.effect(&apply.effect) {
                        let key = self.pending_key(key);
                        let spec = self.spec_at(index, actor, apply.level.unwrap_or(level), key);
                        // An effect the target refuses is skipped.
                        let _ = self.apply_spec(spec, to);
                    }
                }
                Step::EmitEvent(emit) => {
                    instance.step += 1;
                    let actor = instance.actor;
                    self.push(GameplayEvent::Custom {
                        actor,
                        event: emit.event.clone(),
                        payload: emit.payload.clone(),
                    });
                }
            }
        }
    }
}

// -------------------------------------------------------------- native behaviours

/// Result of a native behaviour callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BehaviourStatus {
    Running,
    Ended,
    Cancelled,
}

/// State handed to a native behaviour. `host` is the game world (a Bevy `World` in
/// `engine-project`, without the gameplay service while the callback runs); use `system`
/// for ability-system calls.
pub struct BehaviourContext<'a, W: ?Sized> {
    pub host: &'a mut W,
    pub system: &'a mut AbilitySystem,
    pub instance: InstanceId,
    pub actor: ActorId,
    pub target: Option<ActorId>,
    /// The ability definition's `params`.
    pub params: &'a Value,
    /// Seconds since the previous callback (0 in `activate` and `end`).
    pub dt: f32,
}

/// Game-implemented ability logic, registered by name and named by an ability's `native`.
///
/// `activate` runs after the activation rules passed (and after the commit with the
/// `on_activate` policy); returning `Running` makes the runtime call `tick` every tick
/// until it returns `Ended` or `Cancelled`. `end` runs once whenever the instance finishes,
/// whatever ended it (prediction undo delivers it on the next runtime call).
pub trait AbilityBehaviour<W: ?Sized>: Send + Sync {
    fn can_activate(
        &mut self,
        _host: &mut W,
        _system: &AbilitySystem,
        _request: &ActivationRequest,
    ) -> bool {
        true
    }
    fn activate(&mut self, cx: &mut BehaviourContext<'_, W>) -> BehaviourStatus;
    fn tick(&mut self, cx: &mut BehaviourContext<'_, W>) -> BehaviourStatus;
    fn end(&mut self, _cx: &mut BehaviourContext<'_, W>, _cancelled: bool) {}
}

/// Builds a behaviour for an actor (once per actor and ability for `per_actor`
/// instancing, once per activation for `per_execution`).
pub type BehaviourFactory<W> = fn(&mut W, ActorId) -> Result<Box<dyn AbilityBehaviour<W>>, String>;

/// Names to behaviour factories.
pub struct BehaviourRegistry<W: ?Sized> {
    factories: BTreeMap<String, BehaviourFactory<W>>,
}

impl<W: ?Sized> Default for BehaviourRegistry<W> {
    fn default() -> Self {
        Self {
            factories: BTreeMap::new(),
        }
    }
}

impl<W: ?Sized> Clone for BehaviourRegistry<W> {
    fn clone(&self) -> Self {
        Self {
            factories: self.factories.clone(),
        }
    }
}

impl<W: ?Sized> BehaviourRegistry<W> {
    /// # Errors
    /// An empty, overlong or repeated name.
    pub fn register(&mut self, name: &str, factory: BehaviourFactory<W>) -> Result<(), String> {
        if name.is_empty() || name.len() > 128 {
            return Err("ability behaviour names are 1-128 characters".to_owned());
        }
        if self.factories.contains_key(name) {
            return Err(format!("ability behaviour {name:?} is already registered"));
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

/// [`AbilitySystem`] plus the native behaviours, driven with the host world `W`.
pub struct AbilityRuntime<W: ?Sized> {
    pub system: AbilitySystem,
    registry: BehaviourRegistry<W>,
    per_actor: BTreeMap<(ActorId, usize), Box<dyn AbilityBehaviour<W>>>,
    per_execution: BTreeMap<InstanceId, Box<dyn AbilityBehaviour<W>>>,
}

impl<W: ?Sized> AbilityRuntime<W> {
    pub fn new(system: AbilitySystem, registry: BehaviourRegistry<W>) -> Self {
        Self {
            system,
            registry,
            per_actor: BTreeMap::new(),
            per_execution: BTreeMap::new(),
        }
    }

    pub fn registry(&self) -> &BehaviourRegistry<W> {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut BehaviourRegistry<W> {
        &mut self.registry
    }

    fn fail(
        &mut self,
        request: &ActivationRequest,
        reason: FailReason,
    ) -> Result<InstanceId, FailReason> {
        self.system.push_failed(request, reason);
        Err(reason)
    }

    /// Checks the rules, activates, commits (`on_activate`) and runs the behaviour's first
    /// step. Failures emit `AbilityFailed`.
    ///
    /// # Errors
    /// The failing rule; nothing changed.
    pub fn try_activate(
        &mut self,
        host: &mut W,
        request: ActivationRequest,
    ) -> Result<InstanceId, FailReason> {
        let def = match self.system.can_activate(&request) {
            Ok(def) => def,
            Err(reason) => return self.fail(&request, reason),
        };
        let library = self.system.library().clone();
        let ability = library.ability_at(def);
        let mut fresh = None;
        if let Some(name) = &ability.native {
            let slot = (request.actor, def);
            let reuse =
                ability.instancing == Instancing::PerActor && self.per_actor.contains_key(&slot);
            if !reuse {
                let Some(factory) = self.registry.factories.get(name).copied() else {
                    return self.fail(&request, FailReason::Behaviour);
                };
                match factory(host, request.actor) {
                    Ok(behaviour) => fresh = Some(behaviour),
                    Err(_) => return self.fail(&request, FailReason::Behaviour),
                }
            }
            let behaviour = match fresh.as_mut() {
                Some(b) => b,
                None => self.per_actor.get_mut(&slot).expect("reused behaviour"),
            };
            let allowed = behaviour.can_activate(host, &self.system, &request);
            if ability.instancing == Instancing::PerActor
                && let Some(b) = fresh.take()
            {
                self.per_actor.insert(slot, b);
            }
            if !allowed {
                return self.fail(&request, FailReason::Behaviour);
            }
        }
        let id = self.system.begin_instance(&request, def);
        self.deliver_ends(host);
        if ability.commit == CommitPolicy::OnActivate
            && let Err(reason) = self.system.commit(id)
        {
            self.system.push_failed(&request, reason);
            self.system
                .finish(id, Outcome::Cancelled(CancelReason::CommitFailed));
            self.deliver_ends(host);
            return Err(reason);
        }
        if ability.native.is_some() {
            if let Some(b) = fresh {
                self.per_execution.insert(id, b);
            }
            self.call_native(host, id, 0.0, true);
        } else {
            self.system.run_steps(id, 0.0);
        }
        self.deliver_ends(host);
        Ok(id)
    }

    fn call_native(&mut self, host: &mut W, id: InstanceId, dt: f32, first: bool) {
        let Some(info) = self.system.instances.get(&id) else {
            return;
        };
        let (actor, def, target) = (info.actor, info.def, info.target);
        let library = self.system.library().clone();
        let behaviour = match self.per_execution.get_mut(&id) {
            Some(b) => b,
            None => match self.per_actor.get_mut(&(actor, def)) {
                Some(b) => b,
                None => return,
            },
        };
        let mut cx = BehaviourContext {
            host,
            system: &mut self.system,
            instance: id,
            actor,
            target,
            params: &library.ability_at(def).params,
            dt,
        };
        let status = if first {
            behaviour.activate(&mut cx)
        } else {
            behaviour.tick(&mut cx)
        };
        match status {
            BehaviourStatus::Running => {}
            BehaviourStatus::Ended => {
                self.system.finish(id, Outcome::Ended);
            }
            BehaviourStatus::Cancelled => {
                self.system
                    .finish(id, Outcome::Cancelled(CancelReason::Behaviour));
            }
        }
    }

    /// Delivers `end` to the behaviours of finished native instances.
    pub fn deliver_ends(&mut self, host: &mut W) {
        while !self.system.ended_natives.is_empty() {
            let ended = std::mem::take(&mut self.system.ended_natives);
            let library = self.system.library().clone();
            for e in ended {
                let mut owned = self.per_execution.remove(&e.instance);
                let behaviour = match owned.as_mut() {
                    Some(b) => b,
                    None => match self.per_actor.get_mut(&(e.actor, e.def)) {
                        Some(b) => b,
                        None => continue,
                    },
                };
                let mut cx = BehaviourContext {
                    host: &mut *host,
                    system: &mut self.system,
                    instance: e.instance,
                    actor: e.actor,
                    target: e.target,
                    params: &library.ability_at(e.def).params,
                    dt: 0.0,
                };
                behaviour.end(&mut cx, e.cancelled);
            }
        }
    }

    /// One tick: pending native ends, effect time (periods, expiry), then every active
    /// instance in id order (data-only steps or native `tick`).
    pub fn step(&mut self, host: &mut W, dt: f32) {
        self.deliver_ends(host);
        self.system.tick_effects(dt);
        for id in self.system.instance_ids() {
            let Some(instance) = self.system.instances.get(&id) else {
                continue;
            };
            if self
                .system
                .library()
                .ability_at(instance.def)
                .native
                .is_some()
            {
                self.call_native(host, id, dt, false);
            } else {
                self.system.run_steps(id, dt);
            }
            self.deliver_ends(host);
        }
    }

    /// Sends a gameplay event: every granted ability it triggers tries to activate (by
    /// name), with `instigator` as source and `target` as target.
    pub fn send_event(
        &mut self,
        host: &mut W,
        actor: ActorId,
        event: &str,
        instigator: Option<ActorId>,
        target: Option<ActorId>,
    ) -> Vec<(String, Result<InstanceId, FailReason>)> {
        let mut results = Vec::new();
        for ability in self.system.triggered_by(actor, event) {
            let request = ActivationRequest {
                actor,
                ability: ability.clone(),
                target,
                source: instigator,
                key: None,
            };
            results.push((ability, self.try_activate(host, request)));
        }
        results
    }

    /// Removes an actor, ending its behaviours.
    pub fn remove_actor(&mut self, host: &mut W, actor: ActorId) -> bool {
        let removed = self.system.remove_actor(actor);
        self.deliver_ends(host);
        self.per_actor.retain(|(a, _), _| *a != actor);
        removed
    }

    /// Cancels every instance (reason `shutdown`) and drops all behaviours.
    pub fn shutdown(&mut self, host: &mut W) {
        for id in self.system.instance_ids() {
            self.system
                .finish(id, Outcome::Cancelled(CancelReason::Shutdown));
        }
        self.deliver_ends(host);
        self.per_actor.clear();
        self.per_execution.clear();
    }
}
