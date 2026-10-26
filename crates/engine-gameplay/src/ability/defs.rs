//! Authored definitions: attribute sets, gameplay effects and abilities.
//!
//! One [`Definitions`] document (the `gameplay` asset in `engine-project`) holds every
//! definition of a game. [`Definitions::compile`] validates it and indexes it by name into a
//! [`Library`] that the runtime shares.
use super::tags::{Requirements, valid_tag};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

pub const DOCUMENT_VERSION: u32 = 1;
pub const MAX_ATTRIBUTE_SETS: usize = 64;
pub const MAX_ATTRIBUTES_PER_SET: usize = 64;
pub const MAX_EFFECTS: usize = 1024;
pub const MAX_ABILITIES: usize = 512;
pub const MAX_MODIFIERS: usize = 16;
pub const MAX_TAGS: usize = 32;
pub const MAX_STEPS: usize = 32;
pub const MAX_STACKS: u32 = 1024;
pub const MAX_NAME_LEN: usize = 64;
/// Shortest period of a periodic effect, in seconds.
pub const MIN_PERIOD: f32 = 0.01;
/// Longest duration, period or wait, in seconds.
pub const MAX_SECONDS: f32 = 86_400.0;
/// Largest absolute authored number.
pub const MAX_ABS_VALUE: f32 = 1.0e9;

/// The whole gameplay definition document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Definitions {
    pub version: u32,
    pub attribute_sets: Vec<AttributeSetDef>,
    pub effects: Vec<EffectDef>,
    pub abilities: Vec<AbilityDef>,
}

impl Default for Definitions {
    fn default() -> Self {
        Self {
            version: DOCUMENT_VERSION,
            attribute_sets: Vec::new(),
            effects: Vec::new(),
            abilities: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AttributeSetDef {
    pub name: String,
    /// Attributes in evaluation order: a clamp bound may only name an earlier attribute of
    /// the same set.
    pub attributes: Vec<AttributeDef>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AttributeDef {
    /// Unique across every set of the document.
    pub name: String,
    /// Initial base value.
    pub base: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<Bound>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<Bound>,
}

/// A clamp bound: a number or the current value of another attribute.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Bound {
    Value(f32),
    Attribute(String),
}

/// How a modifier combines with the base value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModOp {
    #[default]
    Add,
    Multiply,
    Divide,
    Override,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModifierDef {
    pub attribute: String,
    #[serde(default)]
    pub op: ModOp,
    pub magnitude: Magnitude,
}

/// How a modifier's magnitude is computed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Magnitude {
    /// `value + per_level * (level - 1)`.
    Scalable(ScalableFloat),
    /// `coefficient * (attribute + pre_add) + post_add`.
    AttributeBased(AttributeMagnitude),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScalableFloat {
    pub value: f32,
    pub per_level: f32,
}

impl ScalableFloat {
    pub fn at(&self, level: u32) -> f32 {
        self.value + self.per_level * level.saturating_sub(1) as f32
    }
}

/// Whose attribute an attribute-based magnitude reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFrom {
    #[default]
    Source,
    Target,
}

/// When an attribute-based magnitude reads its attribute.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capture {
    /// Source attributes when the spec is made, target attributes when it is applied.
    #[default]
    Snapshot,
    /// Re-read on every evaluation (each aggregation or periodic execution).
    Live,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AttributeMagnitude {
    pub attribute: String,
    pub from: CaptureFrom,
    pub capture: Capture,
    pub coefficient: f32,
    pub pre_add: f32,
    pub post_add: f32,
}

impl Default for AttributeMagnitude {
    fn default() -> Self {
        Self {
            attribute: String::new(),
            from: CaptureFrom::Source,
            capture: Capture::Snapshot,
            coefficient: 1.0,
            pre_add: 0.0,
            post_add: 0.0,
        }
    }
}

impl AttributeMagnitude {
    pub fn from_value(&self, attribute: f32) -> f32 {
        self.coefficient * (attribute + self.pre_add) + self.post_add
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationPolicy {
    /// Executes once on the base value.
    #[default]
    Instant,
    /// Active for `duration` seconds, modifying current values.
    Duration,
    /// Active until removed.
    Infinite,
}

/// What happens to the period schedule when an inhibited effect resumes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InhibitionPolicy {
    /// The schedule kept running; executions due while inhibited were skipped.
    #[default]
    NeverReset,
    /// The next execution is one full period after resuming.
    ResetPeriod,
    /// Execute on resuming, then one full period later.
    ExecuteAndResetPeriod,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackKind {
    /// Every application is a separate active effect.
    #[default]
    None,
    /// One stack per source on the target.
    BySource,
    /// One stack per target, whoever applied it.
    ByTarget,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationRefresh {
    /// A successful stack application restarts the duration.
    #[default]
    RefreshOnSuccess,
    NeverRefresh,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodReset {
    /// A successful stack application restarts the period schedule.
    #[default]
    ResetOnSuccess,
    NeverReset,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackExpiration {
    /// The whole stack is removed when the duration ends.
    #[default]
    ClearEntireStack,
    /// One stack is removed and the duration restarts for the rest.
    RemoveSingleStack,
    /// The duration restarts and the stack stays (removal is up to the game).
    RefreshDuration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Stacking {
    pub kind: StackKind,
    /// Most stacks (1..=1024).
    pub limit: u32,
    pub duration_refresh: DurationRefresh,
    pub period_reset: PeriodReset,
    pub expiration: StackExpiration,
    /// Reject applications to a full stack instead of refreshing it.
    pub deny_overflow: bool,
}

impl Default for Stacking {
    fn default() -> Self {
        Self {
            kind: StackKind::None,
            limit: 1,
            duration_refresh: DurationRefresh::RefreshOnSuccess,
            period_reset: PeriodReset::ResetOnSuccess,
            expiration: StackExpiration::ClearEntireStack,
            deny_overflow: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EffectDef {
    pub name: String,
    pub policy: DurationPolicy,
    /// Seconds, for the `duration` policy.
    pub duration: f32,
    /// Seconds between executions; 0 is not periodic. A periodic effect executes its
    /// modifiers on the base value instead of modifying the current value.
    pub period: f32,
    pub execute_on_application: bool,
    pub inhibition: InhibitionPolicy,
    pub modifiers: Vec<ModifierDef>,
    pub stacking: Stacking,
    /// Granted to the target while active (not instant).
    pub granted_tags: Vec<String>,
    /// Describe the effect; matched by `remove_effects_with_tags`.
    pub asset_tags: Vec<String>,
    /// Target tags needed to apply.
    pub application: Requirements,
    /// Target tags needed while active; otherwise the effect is inhibited (no modifiers,
    /// granted tags or executions).
    pub ongoing: Requirements,
    /// Active effects on the target whose asset or granted tags match are removed.
    pub remove_effects_with_tags: Vec<String>,
}

impl EffectDef {
    pub fn is_periodic(&self) -> bool {
        self.policy != DurationPolicy::Instant && self.period > 0.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Instancing {
    /// One behaviour per actor and ability, reused; at most one activation at a time.
    #[default]
    PerActor,
    /// A new behaviour per activation; activations may overlap.
    PerExecution,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitPolicy {
    /// Cost and cooldown are committed right after activation.
    #[default]
    OnActivate,
    /// A `commit` step or native call commits.
    Manual,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepTarget {
    #[default]
    Owner,
    Target,
}

/// One data-only behaviour step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Commits cost and cooldown; failure cancels the ability.
    Commit,
    ApplyEffect(ApplyEffectStep),
    Wait(WaitStep),
    EmitEvent(EmitEventStep),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApplyEffectStep {
    pub effect: String,
    pub to: StepTarget,
    /// Effect level; the ability level when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WaitStep {
    pub seconds: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmitEventStep {
    pub event: String,
    pub payload: Value,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AbilityDef {
    pub name: String,
    /// Describe the ability; matched by cancel/block lists.
    pub tags: Vec<String>,
    /// Instant effect applied to the owner on commit; activation needs every modified
    /// attribute to stay at or above 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<String>,
    /// Duration effect applied to the owner on commit; its granted tags block activation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<String>,
    /// Granted to the owner while the ability is active.
    pub activation_owned_tags: Vec<String>,
    pub owner: Requirements,
    pub source: Requirements,
    pub target: Requirements,
    /// Active abilities of the owner with a matching tag are cancelled on activation.
    pub cancel_abilities_with_tags: Vec<String>,
    /// Abilities with a matching tag cannot activate while this one is active.
    pub block_abilities_with_tags: Vec<String>,
    pub instancing: Instancing,
    pub commit: CommitPolicy,
    /// Registered native behaviour; exclusive with `steps`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
    /// Parameters handed to the native behaviour.
    pub params: Value,
    /// Data-only behaviour, run in order; the ability ends after the last step.
    pub steps: Vec<Step>,
    /// Gameplay event names that activate the ability.
    pub triggers: Vec<String>,
}

impl AbilityDef {
    pub fn needs_target(&self) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s, Step::ApplyEffect(a) if a.to == StepTarget::Target))
    }
}

/// Validated, name-indexed definitions shared by the runtime.
#[derive(Debug)]
pub struct Library {
    doc: Definitions,
    sets: BTreeMap<String, usize>,
    attributes: BTreeMap<String, (usize, usize)>,
    effects: BTreeMap<String, usize>,
    abilities: BTreeMap<String, usize>,
}

impl Library {
    pub fn definitions(&self) -> &Definitions {
        &self.doc
    }
    pub fn set(&self, name: &str) -> Option<(usize, &AttributeSetDef)> {
        self.sets
            .get(name)
            .map(|&i| (i, &self.doc.attribute_sets[i]))
    }
    pub fn set_at(&self, index: usize) -> &AttributeSetDef {
        &self.doc.attribute_sets[index]
    }
    pub fn attribute(&self, name: &str) -> Option<&AttributeDef> {
        self.attributes
            .get(name)
            .map(|&(s, a)| &self.doc.attribute_sets[s].attributes[a])
    }
    pub fn has_attribute(&self, name: &str) -> bool {
        self.attributes.contains_key(name)
    }
    pub fn effect(&self, name: &str) -> Option<(usize, &EffectDef)> {
        self.effects.get(name).map(|&i| (i, &self.doc.effects[i]))
    }
    pub fn effect_at(&self, index: usize) -> &EffectDef {
        &self.doc.effects[index]
    }
    pub fn ability(&self, name: &str) -> Option<(usize, &AbilityDef)> {
        self.abilities
            .get(name)
            .map(|&i| (i, &self.doc.abilities[i]))
    }
    pub fn ability_at(&self, index: usize) -> &AbilityDef {
        &self.doc.abilities[index]
    }
}

fn name_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME_LEN && !name.chars().any(char::is_control)
}

fn finite(v: f32, what: &str) -> Result<(), String> {
    if v.is_finite() && v.abs() <= MAX_ABS_VALUE {
        Ok(())
    } else {
        Err(format!("{what} must be finite and within ±{MAX_ABS_VALUE}"))
    }
}

fn tags_ok<'a>(tags: impl IntoIterator<Item = &'a String>, what: &str) -> Result<(), String> {
    let mut n = 0;
    for tag in tags {
        n += 1;
        if !valid_tag(tag) {
            return Err(format!(
                "{what}: invalid tag {tag:?} (dotted segments of letters, digits and _)"
            ));
        }
    }
    if n > MAX_TAGS * 2 {
        return Err(format!("{what}: at most {MAX_TAGS} tags per list"));
    }
    Ok(())
}

fn list_ok(tags: &[String], what: &str) -> Result<(), String> {
    if tags.len() > MAX_TAGS {
        return Err(format!("{what}: at most {MAX_TAGS} tags"));
    }
    tags_ok(tags, what)
}

fn seconds_ok(v: f32, what: &str) -> Result<(), String> {
    if v.is_finite() && (0.0..=MAX_SECONDS).contains(&v) {
        Ok(())
    } else {
        Err(format!("{what} must be 0..={MAX_SECONDS} seconds"))
    }
}

impl Definitions {
    /// Parses and validates a JSON document.
    pub fn from_json(bytes: &[u8]) -> Result<Arc<Library>, String> {
        let doc: Definitions = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        doc.compile()
    }

    /// Validates the document and indexes it by name.
    pub fn compile(self) -> Result<Arc<Library>, String> {
        if self.version != DOCUMENT_VERSION {
            return Err(format!(
                "gameplay definitions version must be {DOCUMENT_VERSION}"
            ));
        }
        if self.attribute_sets.len() > MAX_ATTRIBUTE_SETS
            || self.effects.len() > MAX_EFFECTS
            || self.abilities.len() > MAX_ABILITIES
        {
            return Err(format!(
                "at most {MAX_ATTRIBUTE_SETS} attribute sets, {MAX_EFFECTS} effects and {MAX_ABILITIES} abilities"
            ));
        }
        let mut sets = BTreeMap::new();
        let mut attributes = BTreeMap::new();
        for (si, set) in self.attribute_sets.iter().enumerate() {
            if !name_ok(&set.name) || sets.insert(set.name.clone(), si).is_some() {
                return Err(format!(
                    "attribute set name {:?} is empty, too long or repeated",
                    set.name
                ));
            }
            if set.attributes.len() > MAX_ATTRIBUTES_PER_SET {
                return Err(format!(
                    "attribute set {}: at most {MAX_ATTRIBUTES_PER_SET} attributes",
                    set.name
                ));
            }
            let mut earlier: Vec<&str> = Vec::new();
            for (ai, attribute) in set.attributes.iter().enumerate() {
                if !name_ok(&attribute.name)
                    || attributes
                        .insert(attribute.name.clone(), (si, ai))
                        .is_some()
                {
                    return Err(format!(
                        "attribute name {:?} is empty, too long or repeated across sets",
                        attribute.name
                    ));
                }
                let what = format!("attribute {}", attribute.name);
                finite(attribute.base, &what)?;
                for bound in [&attribute.min, &attribute.max].into_iter().flatten() {
                    match bound {
                        Bound::Value(v) => finite(*v, &what)?,
                        Bound::Attribute(other) if !earlier.contains(&other.as_str()) => {
                            return Err(format!(
                                "{what}: bound {other:?} must be an earlier attribute of set {}",
                                set.name
                            ));
                        }
                        Bound::Attribute(_) => {}
                    }
                }
                if let (Some(Bound::Value(lo)), Some(Bound::Value(hi))) =
                    (&attribute.min, &attribute.max)
                    && lo > hi
                {
                    return Err(format!("{what}: min is above max"));
                }
                earlier.push(&attribute.name);
            }
        }
        let mut effects = BTreeMap::new();
        for (i, effect) in self.effects.iter().enumerate() {
            if !name_ok(&effect.name) || effects.insert(effect.name.clone(), i).is_some() {
                return Err(format!(
                    "effect name {:?} is empty, too long or repeated",
                    effect.name
                ));
            }
            check_effect(effect, &attributes)?;
        }
        let mut abilities = BTreeMap::new();
        for (i, ability) in self.abilities.iter().enumerate() {
            if !name_ok(&ability.name) || abilities.insert(ability.name.clone(), i).is_some() {
                return Err(format!(
                    "ability name {:?} is empty, too long or repeated",
                    ability.name
                ));
            }
            check_ability(ability, &self.effects, &effects)?;
        }
        Ok(Arc::new(Library {
            doc: self,
            sets,
            attributes,
            effects,
            abilities,
        }))
    }
}

fn check_effect(
    effect: &EffectDef,
    attributes: &BTreeMap<String, (usize, usize)>,
) -> Result<(), String> {
    let what = format!("effect {}", effect.name);
    seconds_ok(effect.duration, &format!("{what}: duration"))?;
    seconds_ok(effect.period, &format!("{what}: period"))?;
    match effect.policy {
        DurationPolicy::Instant => {
            if effect.period != 0.0
                || !effect.granted_tags.is_empty()
                || effect.stacking.kind != StackKind::None
                || !effect.ongoing.is_empty()
            {
                return Err(format!(
                    "{what}: instant effects cannot be periodic, stack, grant tags or have ongoing requirements"
                ));
            }
        }
        DurationPolicy::Duration if effect.duration <= 0.0 => {
            return Err(format!("{what}: the duration policy needs duration > 0"));
        }
        _ => {}
    }
    if effect.period != 0.0 && effect.period < MIN_PERIOD {
        return Err(format!("{what}: period must be 0 or at least {MIN_PERIOD}"));
    }
    if effect.modifiers.len() > MAX_MODIFIERS {
        return Err(format!("{what}: at most {MAX_MODIFIERS} modifiers"));
    }
    for modifier in &effect.modifiers {
        if !attributes.contains_key(&modifier.attribute) {
            return Err(format!(
                "{what}: unknown attribute {:?}",
                modifier.attribute
            ));
        }
        match &modifier.magnitude {
            Magnitude::Scalable(s) => {
                finite(s.value, &what)?;
                finite(s.per_level, &what)?;
            }
            Magnitude::AttributeBased(a) => {
                if !attributes.contains_key(&a.attribute) {
                    return Err(format!(
                        "{what}: magnitude reads unknown attribute {:?}",
                        a.attribute
                    ));
                }
                for v in [a.coefficient, a.pre_add, a.post_add] {
                    finite(v, &what)?;
                }
            }
        }
    }
    let stacking = &effect.stacking;
    if !(1..=MAX_STACKS).contains(&stacking.limit) {
        return Err(format!("{what}: stack limit must be 1..={MAX_STACKS}"));
    }
    list_ok(&effect.granted_tags, &what)?;
    list_ok(&effect.asset_tags, &what)?;
    list_ok(&effect.remove_effects_with_tags, &what)?;
    tags_ok(effect.application.tags(), &what)?;
    tags_ok(effect.ongoing.tags(), &what)?;
    Ok(())
}

fn check_ability(
    ability: &AbilityDef,
    effect_defs: &[EffectDef],
    effects: &BTreeMap<String, usize>,
) -> Result<(), String> {
    let what = format!("ability {}", ability.name);
    let effect = |name: &str| {
        effects
            .get(name)
            .map(|&i| &effect_defs[i])
            .ok_or_else(|| format!("{what}: unknown effect {name:?}"))
    };
    if let Some(cost) = &ability.cost
        && effect(cost)?.policy != DurationPolicy::Instant
    {
        return Err(format!("{what}: cost effect {cost} must be instant"));
    }
    if let Some(cooldown) = &ability.cooldown {
        let def = effect(cooldown)?;
        if def.policy != DurationPolicy::Duration || def.granted_tags.is_empty() {
            return Err(format!(
                "{what}: cooldown effect {cooldown} must use the duration policy and grant a tag"
            ));
        }
    }
    for list in [
        &ability.tags,
        &ability.activation_owned_tags,
        &ability.cancel_abilities_with_tags,
        &ability.block_abilities_with_tags,
    ] {
        list_ok(list, &what)?;
    }
    for rule in [&ability.owner, &ability.source, &ability.target] {
        tags_ok(rule.tags(), &what)?;
    }
    if ability.native.is_some() && !ability.steps.is_empty() {
        return Err(format!("{what}: native and steps are exclusive"));
    }
    if let Some(native) = &ability.native
        && (native.is_empty() || native.len() > 128)
    {
        return Err(format!(
            "{what}: native behaviour names are 1-128 characters"
        ));
    }
    if ability.steps.len() > MAX_STEPS {
        return Err(format!("{what}: at most {MAX_STEPS} steps"));
    }
    let mut commits = 0;
    for step in &ability.steps {
        match step {
            Step::Commit => commits += 1,
            Step::ApplyEffect(a) => {
                effect(&a.effect)?;
                if a.level == Some(0) {
                    return Err(format!("{what}: effect levels start at 1"));
                }
            }
            Step::Wait(w) => seconds_ok(w.seconds, &format!("{what}: wait"))?,
            Step::EmitEvent(e) => {
                if e.event.is_empty()
                    || e.event.len() > 128
                    || e.event.chars().any(char::is_control)
                {
                    return Err(format!("{what}: event names are 1-128 characters"));
                }
            }
        }
    }
    if ability.commit == CommitPolicy::Manual && ability.native.is_none() && commits == 0 {
        return Err(format!(
            "{what}: a manual commit policy needs a commit step"
        ));
    }
    if ability.triggers.len() > MAX_TAGS
        || ability
            .triggers
            .iter()
            .any(|t| t.is_empty() || t.len() > 128 || t.chars().any(char::is_control))
    {
        return Err(format!(
            "{what}: at most {MAX_TAGS} triggers of 1-128 characters"
        ));
    }
    Ok(())
}
