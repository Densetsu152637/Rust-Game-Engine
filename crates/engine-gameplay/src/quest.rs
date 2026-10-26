//! Quests: staged objectives fed by named events, flags and inventory contents.
//!
//! A [`QuestDef`] has prerequisites (quests that must be completed), ordered stages and
//! rewards. A stage's objectives all have to be done to advance; finishing the last stage
//! completes the quest and hands out its [`Rewards`] exactly once (the state change is the
//! only way to receive them). A quest is `Inactive`, `Active`, `Completed` or `Failed`.
//! Everything is pure and deterministic: progress is ordered by quest id and objective id.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Most quests one document may define.
pub const MAX_QUESTS: usize = 256;
/// Most stages one quest may have.
pub const MAX_STAGES: usize = 16;
/// Most objectives one stage may have.
pub const MAX_OBJECTIVES: usize = 16;
/// Most quests whose progress one log keeps.
pub const MAX_TRACKED: usize = 256;

/// Payload constraints of an event: every `path` (dotted, `a.b`) must hold the value.
pub type Filter = BTreeMap<String, Value>;

/// What an objective waits for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectiveDef {
    /// Counts events named `event` whose payload matches `filter` up to `target`.
    Counter {
        id: String,
        event: String,
        #[serde(default)]
        filter: Filter,
        target: u32,
    },
    /// Done when the flag is set (`QuestLog::set_flag`).
    Flag { id: String, flag: String },
    /// Done when the inventory holds `count` of `item` (checked when it changes).
    HasItem {
        id: String,
        item: String,
        count: u32,
    },
}

impl ObjectiveDef {
    pub fn id(&self) -> &str {
        match self {
            Self::Counter { id, .. } | Self::Flag { id, .. } | Self::HasItem { id, .. } => id,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StageDef {
    pub id: String,
    pub objectives: Vec<ObjectiveDef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventMatch {
    pub event: String,
    #[serde(default)]
    pub filter: Filter,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewardItem {
    pub item: String,
    pub count: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RewardEvent {
    pub name: String,
    pub payload: Value,
}

impl Default for RewardEvent {
    fn default() -> Self {
        Self {
            name: String::new(),
            payload: Value::Null,
        }
    }
}

/// What completing a quest gives.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rewards {
    pub items: Vec<RewardItem>,
    /// Ability-system effect applied to the quest owner.
    pub effect: Option<String>,
    /// Engine event emitted at the quest owner.
    pub event: Option<RewardEvent>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuestDef {
    pub id: String,
    /// Quests that must be completed before this one can start.
    pub prerequisites: Vec<String>,
    pub stages: Vec<StageDef>,
    pub rewards: Rewards,
    /// Events that fail the quest while it is active.
    pub fail_on: Vec<EventMatch>,
}

fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

fn event_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

/// Checks the quests of a document against its items and effects.
///
/// # Errors
/// The first problem, naming the quest.
pub fn validate_quests(
    quests: &[QuestDef],
    items: &BTreeSet<&str>,
    effects: &BTreeSet<&str>,
) -> Result<(), String> {
    if quests.len() > MAX_QUESTS {
        return Err(format!("more than {MAX_QUESTS} quests"));
    }
    let mut ids = BTreeSet::new();
    for quest in quests {
        if !name_ok(&quest.id) || !ids.insert(quest.id.as_str()) {
            return Err(format!("quest id {:?} is invalid or repeated", quest.id));
        }
    }
    for quest in quests {
        let q = &quest.id;
        if quest.stages.is_empty() || quest.stages.len() > MAX_STAGES {
            return Err(format!("quest {q}: 1..={MAX_STAGES} stages"));
        }
        for p in &quest.prerequisites {
            if !ids.contains(p.as_str()) || p == q {
                return Err(format!("quest {q}: unknown or self prerequisite {p:?}"));
            }
        }
        let mut objective_ids = BTreeSet::new();
        for stage in &quest.stages {
            if !name_ok(&stage.id) {
                return Err(format!("quest {q}: invalid stage id {:?}", stage.id));
            }
            if stage.objectives.is_empty() || stage.objectives.len() > MAX_OBJECTIVES {
                return Err(format!(
                    "quest {q}: stage {} needs 1..={MAX_OBJECTIVES} objectives",
                    stage.id
                ));
            }
            for objective in &stage.objectives {
                let oid = objective.id();
                if !name_ok(oid) || !objective_ids.insert(oid) {
                    return Err(format!(
                        "quest {q}: objective id {oid:?} is invalid or repeated"
                    ));
                }
                match objective {
                    ObjectiveDef::Counter { event, target, .. } => {
                        if !event_ok(event) || *target == 0 || *target > 1_000_000 {
                            return Err(format!(
                                "quest {q}: objective {oid} needs a valid event and target 1..=1000000"
                            ));
                        }
                    }
                    ObjectiveDef::Flag { flag, .. } => {
                        if !name_ok(flag) {
                            return Err(format!("quest {q}: objective {oid} has an invalid flag"));
                        }
                    }
                    ObjectiveDef::HasItem { item, count, .. } => {
                        if !items.contains(item.as_str()) || *count == 0 {
                            return Err(format!(
                                "quest {q}: objective {oid} needs a known item and count >= 1"
                            ));
                        }
                    }
                }
            }
        }
        for reward in &quest.rewards.items {
            if !items.contains(reward.item.as_str()) || reward.count == 0 {
                return Err(format!(
                    "quest {q}: reward item {:?} is unknown or count is 0",
                    reward.item
                ));
            }
        }
        if let Some(effect) = &quest.rewards.effect
            && !effects.contains(effect.as_str())
        {
            return Err(format!("quest {q}: unknown reward effect {effect:?}"));
        }
        if let Some(event) = &quest.rewards.event
            && (!event_ok(&event.name) || !(event.payload.is_null() || event.payload.is_object()))
        {
            return Err(format!(
                "quest {q}: reward event needs a valid name and an object payload"
            ));
        }
        if quest.fail_on.len() > 8 || quest.fail_on.iter().any(|m| !event_ok(&m.event)) {
            return Err(format!("quest {q}: at most 8 valid fail_on events"));
        }
    }
    // Prerequisite cycles would make a quest unstartable.
    for quest in quests {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<&str> = quest.prerequisites.iter().map(String::as_str).collect();
        while let Some(next) = stack.pop() {
            if next == quest.id {
                return Err(format!("quest {}: prerequisites form a cycle", quest.id));
            }
            if seen.insert(next)
                && let Some(def) = quests.iter().find(|q| q.id == next)
            {
                stack.extend(def.prerequisites.iter().map(String::as_str));
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestState {
    Inactive,
    Active,
    Completed,
    Failed,
}

impl QuestState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// Why a quest could not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestError {
    Unknown,
    /// Already active or completed.
    NotStartable(QuestState),
    /// This prerequisite is not completed.
    Prerequisite(String),
    /// The log tracks too many quests.
    Capacity,
}

impl QuestError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unknown => "quest.unknown",
            Self::NotStartable(_) => "quest.not_startable",
            Self::Prerequisite(_) => "quest.prerequisite",
            Self::Capacity => "quest.capacity",
        }
    }
}

impl std::fmt::Display for QuestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotStartable(state) => write!(f, "quest.not_startable: {}", state.as_str()),
            Self::Prerequisite(p) => write!(f, "quest.prerequisite: {p}"),
            other => f.write_str(other.code()),
        }
    }
}
impl std::error::Error for QuestError {}

/// Something that happened to a quest.
#[derive(Clone, Debug, PartialEq)]
pub enum QuestEvent {
    Started {
        quest: String,
    },
    ObjectiveProgressed {
        quest: String,
        objective: String,
        value: u32,
        target: u32,
    },
    StageAdvanced {
        quest: String,
        stage: String,
    },
    /// Completed: deliver the rewards now (this is the only time they are returned).
    Completed {
        quest: String,
        rewards: Rewards,
    },
    Failed {
        quest: String,
    },
}

/// What the quest log may ask the game.
pub trait QuestContext {
    /// Whether the owner's inventory holds `count` units of `item`.
    fn has_item(&self, item: &str, count: u32) -> bool;
}

/// Context without an inventory.
pub struct NoContext;
impl QuestContext for NoContext {
    fn has_item(&self, _: &str, _: u32) -> bool {
        false
    }
}

/// Quest definitions indexed by id.
#[derive(Clone, Debug, Default)]
pub struct QuestTable {
    quests: BTreeMap<String, QuestDef>,
}

impl QuestTable {
    pub fn new(quests: &[QuestDef]) -> Self {
        Self {
            quests: quests.iter().map(|q| (q.id.clone(), q.clone())).collect(),
        }
    }
    pub fn get(&self, id: &str) -> Option<&QuestDef> {
        self.quests.get(id)
    }
    pub fn len(&self) -> usize {
        self.quests.len()
    }
    pub fn is_empty(&self) -> bool {
        self.quests.is_empty()
    }
    /// Event names some objective or failure condition listens for.
    pub fn listened_events(&self) -> BTreeSet<&str> {
        let mut names = BTreeSet::new();
        for quest in self.quests.values() {
            for m in &quest.fail_on {
                names.insert(m.event.as_str());
            }
            for stage in &quest.stages {
                for o in &stage.objectives {
                    if let ObjectiveDef::Counter { event, .. } = o {
                        names.insert(event.as_str());
                    }
                }
            }
        }
        names
    }
}

#[derive(Clone, Debug)]
struct Progress {
    state: QuestState,
    stage: usize,
    counters: BTreeMap<String, u32>,
    done: BTreeSet<String>,
}

/// Progress of one owner's quests.
#[derive(Clone, Debug, Default)]
pub struct QuestLog {
    progress: BTreeMap<String, Progress>,
    flags: BTreeSet<String>,
}

fn lookup<'a>(payload: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(payload, |v, key| v.get(key))
}

fn loosely_equal(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Whether `payload` satisfies every constraint of `filter`.
pub fn filter_matches(filter: &Filter, payload: &Value) -> bool {
    filter
        .iter()
        .all(|(path, expected)| lookup(payload, path).is_some_and(|v| loosely_equal(v, expected)))
}

impl QuestLog {
    pub fn state(&self, quest: &str) -> QuestState {
        self.progress
            .get(quest)
            .map_or(QuestState::Inactive, |p| p.state)
    }
    pub fn flag(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }
    pub fn flags(&self) -> impl Iterator<Item = &str> {
        self.flags.iter().map(String::as_str)
    }
    /// `(quest, state)` of every tracked quest, ordered by id.
    pub fn states(&self) -> Vec<(String, QuestState)> {
        self.progress
            .iter()
            .map(|(id, p)| (id.clone(), p.state))
            .collect()
    }
    /// Current stage id and per-objective `(id, value, target)` of an active quest.
    pub fn objectives(&self, table: &QuestTable, quest: &str) -> Vec<(String, u32, u32)> {
        let (Some(def), Some(p)) = (table.get(quest), self.progress.get(quest)) else {
            return Vec::new();
        };
        let Some(stage) = def.stages.get(p.stage) else {
            return Vec::new();
        };
        stage
            .objectives
            .iter()
            .map(|o| {
                let target = match o {
                    ObjectiveDef::Counter { target, .. } => *target,
                    _ => 1,
                };
                let value = if p.done.contains(o.id()) {
                    target
                } else {
                    p.counters.get(o.id()).copied().unwrap_or(0)
                };
                (o.id().to_owned(), value, target)
            })
            .collect()
    }
    pub fn stage(&self, table: &QuestTable, quest: &str) -> Option<String> {
        let p = self.progress.get(quest)?;
        table.get(quest)?.stages.get(p.stage).map(|s| s.id.clone())
    }

    /// Starts a quest that is inactive or failed and whose prerequisites are completed.
    ///
    /// # Errors
    /// See [`QuestError`].
    pub fn start(
        &mut self,
        table: &QuestTable,
        quest: &str,
        ctx: &dyn QuestContext,
    ) -> Result<Vec<QuestEvent>, QuestError> {
        let def = table.get(quest).ok_or(QuestError::Unknown)?;
        let state = self.state(quest);
        if matches!(state, QuestState::Active | QuestState::Completed) {
            return Err(QuestError::NotStartable(state));
        }
        if let Some(missing) = def
            .prerequisites
            .iter()
            .find(|p| self.state(p) != QuestState::Completed)
        {
            return Err(QuestError::Prerequisite(missing.clone()));
        }
        if !self.progress.contains_key(quest) && self.progress.len() >= MAX_TRACKED {
            return Err(QuestError::Capacity);
        }
        self.progress.insert(
            quest.to_owned(),
            Progress {
                state: QuestState::Active,
                stage: 0,
                counters: BTreeMap::new(),
                done: BTreeSet::new(),
            },
        );
        let mut events = vec![QuestEvent::Started {
            quest: quest.to_owned(),
        }];
        self.settle(table, quest, ctx, &mut events);
        Ok(events)
    }

    /// Drops an active quest's progress; it becomes inactive again. Returns whether it was
    /// active.
    pub fn abandon(&mut self, quest: &str) -> bool {
        if self.state(quest) == QuestState::Active {
            self.progress.remove(quest);
            true
        } else {
            false
        }
    }

    /// Fails an active quest.
    pub fn fail(&mut self, quest: &str) -> Vec<QuestEvent> {
        match self.progress.get_mut(quest) {
            Some(p) if p.state == QuestState::Active => {
                p.state = QuestState::Failed;
                vec![QuestEvent::Failed {
                    quest: quest.to_owned(),
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Feeds an event: fails quests whose `fail_on` matches, then advances counters.
    pub fn on_event(
        &mut self,
        table: &QuestTable,
        name: &str,
        payload: &Value,
        ctx: &dyn QuestContext,
    ) -> Vec<QuestEvent> {
        let mut events = Vec::new();
        let active: Vec<String> = self
            .progress
            .iter()
            .filter(|(_, p)| p.state == QuestState::Active)
            .map(|(id, _)| id.clone())
            .collect();
        for id in active {
            let Some(def) = table.get(&id) else {
                continue;
            };
            if def
                .fail_on
                .iter()
                .any(|m| m.event == name && filter_matches(&m.filter, payload))
            {
                events.extend(self.fail(&id));
                continue;
            }
            let p = self.progress.get_mut(&id).expect("active quest tracked");
            let Some(stage) = def.stages.get(p.stage) else {
                continue;
            };
            for objective in &stage.objectives {
                let ObjectiveDef::Counter {
                    id: oid,
                    event,
                    filter,
                    target,
                } = objective
                else {
                    continue;
                };
                if event != name || p.done.contains(oid) || !filter_matches(filter, payload) {
                    continue;
                }
                let value = p.counters.entry(oid.clone()).or_default();
                *value += 1;
                let now = *value;
                events.push(QuestEvent::ObjectiveProgressed {
                    quest: id.clone(),
                    objective: oid.clone(),
                    value: now.min(*target),
                    target: *target,
                });
                if now >= *target {
                    p.done.insert(oid.clone());
                }
            }
            self.settle(table, &id, ctx, &mut events);
        }
        events
    }

    /// Sets a flag and re-checks flag objectives.
    pub fn set_flag(
        &mut self,
        table: &QuestTable,
        flag: &str,
        ctx: &dyn QuestContext,
    ) -> Vec<QuestEvent> {
        if !self.flags.insert(flag.to_owned()) {
            return Vec::new();
        }
        self.refresh(table, ctx)
    }

    /// Re-checks flag and has-item objectives of every active quest (call after the
    /// inventory changed).
    pub fn refresh(&mut self, table: &QuestTable, ctx: &dyn QuestContext) -> Vec<QuestEvent> {
        let mut events = Vec::new();
        let active: Vec<String> = self
            .progress
            .iter()
            .filter(|(_, p)| p.state == QuestState::Active)
            .map(|(id, _)| id.clone())
            .collect();
        for id in active {
            self.settle(table, &id, ctx, &mut events);
        }
        events
    }

    /// Marks flag/has-item objectives of the current stage, then advances stages and
    /// completes the quest while the stage is done.
    fn settle(
        &mut self,
        table: &QuestTable,
        id: &str,
        ctx: &dyn QuestContext,
        events: &mut Vec<QuestEvent>,
    ) {
        let Some(def) = table.get(id) else {
            return;
        };
        loop {
            let flags = &self.flags;
            let Some(p) = self.progress.get_mut(id) else {
                return;
            };
            if p.state != QuestState::Active {
                return;
            }
            let Some(stage) = def.stages.get(p.stage) else {
                return;
            };
            for objective in &stage.objectives {
                if p.done.contains(objective.id()) {
                    continue;
                }
                let reached = match objective {
                    ObjectiveDef::Flag { flag, .. } => flags.contains(flag),
                    ObjectiveDef::HasItem { item, count, .. } => ctx.has_item(item, *count),
                    ObjectiveDef::Counter { .. } => false,
                };
                if reached {
                    p.done.insert(objective.id().to_owned());
                    events.push(QuestEvent::ObjectiveProgressed {
                        quest: id.to_owned(),
                        objective: objective.id().to_owned(),
                        value: 1,
                        target: 1,
                    });
                }
            }
            if !stage.objectives.iter().all(|o| p.done.contains(o.id())) {
                return;
            }
            p.stage += 1;
            p.counters.clear();
            p.done.clear();
            if p.stage >= def.stages.len() {
                p.state = QuestState::Completed;
                events.push(QuestEvent::Completed {
                    quest: id.to_owned(),
                    rewards: def.rewards.clone(),
                });
                return;
            }
            events.push(QuestEvent::StageAdvanced {
                quest: id.to_owned(),
                stage: def.stages[p.stage].id.clone(),
            });
        }
    }
}
