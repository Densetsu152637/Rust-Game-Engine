//! Inventories, quests and dialogue in the runtime.
//!
//! * `engine.Inventory` ([`InventoryComponent`]): a slot container with weight limit,
//!   initial items and tag-gated equipment slots. Items come from the `gameplay` asset
//!   (`items`); equipping an item with an `equip_effect` applies that ability-system
//!   effect to the entity while it is worn (when an `engine.AbilitySystem` runs).
//! * `engine.QuestLog` ([`QuestLogComponent`]): the entity's quest progress, fed by engine
//!   events, flags and its inventory. Quests come from the same asset (`quests`).
//! * `engine.DialogueParticipant` ([`DialogueParticipantComponent`]): an NPC with a
//!   `dialogue` graph asset; a conversation is between the NPC and a player entity whose
//!   quest log and inventory the conversation tests and changes.
//! * Events `inventory.changed`, `inventory.full`, `inventory.equipped`,
//!   `inventory.unequipped`, `quest.started`, `quest.objective_progressed`,
//!   `quest.stage_advanced`, `quest.completed`, `quest.failed`, `dialogue.line`,
//!   `dialogue.choices`, `dialogue.ended`; and the inputs `dialogue.start`,
//!   `dialogue.advance`, `dialogue.select`.
//!
//! See `docs/gameplay-inventory-quests-dialogue.md`.
use super::load_definitions;
use crate::{
    ComponentDescriptor, EngineEvent, Error, Project, Result, Scene,
    events::DeliveredEvents,
    runtime::{AuthoredId, ProjectResource},
};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;
use engine_gameplay::{
    dialogue::{self, Conversation, Dialogue, DialogueError, DialogueHost, Output},
    inventory::{
        Capacity, Container, Equipment, EquipmentSlotDef, InventoryError, ItemTable, transfer,
    },
    quest::{QuestContext, QuestError, QuestEvent, QuestLog, QuestState, QuestTable},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub const INVENTORY: &str = "engine.Inventory";
pub const QUEST_LOG: &str = "engine.QuestLog";
pub const DIALOGUE_PARTICIPANT: &str = "engine.DialogueParticipant";

/// Engine events one tick may emit from this service; the rest are dropped and counted.
pub const MAX_EVENTS_PER_TICK: usize = 256;
/// Entities listed per kind in telemetry.
pub const STATUS_MAX_ENTITIES: usize = 16;
const STATUS_MAX_ITEMS: usize = 16;
/// Nested quest completions (a reward completing another quest) are followed this deep.
const MAX_CHAIN: usize = 8;

pub const EVENT_CHANGED: &str = "inventory.changed";
pub const EVENT_FULL: &str = "inventory.full";
pub const EVENT_EQUIPPED: &str = "inventory.equipped";
pub const EVENT_UNEQUIPPED: &str = "inventory.unequipped";
pub const EVENT_QUEST_STARTED: &str = "quest.started";
pub const EVENT_QUEST_PROGRESSED: &str = "quest.objective_progressed";
pub const EVENT_QUEST_STAGE: &str = "quest.stage_advanced";
pub const EVENT_QUEST_COMPLETED: &str = "quest.completed";
pub const EVENT_QUEST_FAILED: &str = "quest.failed";
pub const EVENT_DIALOGUE_LINE: &str = "dialogue.line";
pub const EVENT_DIALOGUE_CHOICES: &str = "dialogue.choices";
pub const EVENT_DIALOGUE_ENDED: &str = "dialogue.ended";
pub const EVENT_DIALOGUE_START: &str = "dialogue.start";
pub const EVENT_DIALOGUE_ADVANCE: &str = "dialogue.advance";
pub const EVENT_DIALOGUE_SELECT: &str = "dialogue.select";

/// An item the inventory starts with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitialItem {
    pub item: String,
    pub count: u32,
}

/// Authored inventory.
#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InventoryComponent {
    /// Id of the `gameplay` asset holding `items`.
    pub definitions: String,
    pub capacity: Capacity,
    pub items: Vec<InitialItem>,
    pub equipment: Vec<EquipmentSlotDef>,
}

/// Authored quest log.
#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuestLogComponent {
    /// Id of the `gameplay` asset holding `quests`.
    pub definitions: String,
    /// Quests started when the log is created.
    pub start: Vec<String>,
    /// Flags set when the log is created.
    pub flags: Vec<String>,
}

/// An NPC that can talk.
#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DialogueParticipantComponent {
    /// Id of a `graph` asset of kind `dialogue`.
    pub graph: String,
    /// Optional `gameplay` asset: when set, the quests and items the dialogue names must
    /// exist in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definitions: Option<String>,
}

fn arr(items: Value, max: u32) -> Value {
    json!({"type":"array","maxItems":max,"items":items})
}

/// Catalog descriptors of the three components.
pub fn descriptors() -> Vec<ComponentDescriptor> {
    let text = |max: u32| json!({"type":"string","maxLength":max});
    vec![
        ComponentDescriptor {
            name: INVENTORY.into(),
            version: 1,
            schema: json!({"type":"object","additionalProperties":false,"properties":{
                "definitions": {"type":"string","maxLength":128,
                    "description":"Id of the gameplay asset holding the item definitions."},
                "capacity": {"type":"object","additionalProperties":false,"properties":{
                    "slots": {"type":"integer","minimum":1,"maximum":256},
                    "max_weight": {"type":"number","minimum":0,
                        "description":"Largest total weight; 0 is unlimited."}}},
                "items": arr(json!({"type":"object","additionalProperties":false,
                    "required":["item","count"],"properties":{
                        "item": text(64),
                        "count": {"type":"integer","minimum":1}}}), 256),
                "equipment": arr(json!({"type":"object","additionalProperties":false,
                    "required":["name","tag"],"properties":{"name": text(64),"tag": text(64)}}), 16)
            }}),
            defaults: serde_json::to_value(InventoryComponent::default()).expect("defaults"),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
        ComponentDescriptor {
            name: QUEST_LOG.into(),
            version: 1,
            schema: json!({"type":"object","additionalProperties":false,"properties":{
                "definitions": {"type":"string","maxLength":128,
                    "description":"Id of the gameplay asset holding the quest definitions."},
                "start": arr(text(64), 64),
                "flags": arr(text(64), 64)
            }}),
            defaults: serde_json::to_value(QuestLogComponent::default()).expect("defaults"),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
        ComponentDescriptor {
            name: DIALOGUE_PARTICIPANT.into(),
            version: 1,
            schema: json!({"type":"object","additionalProperties":false,"properties":{
                "graph": {"type":"string","maxLength":128,
                    "description":"Id of a graph asset of kind dialogue."},
                "definitions": {"type":"string","maxLength":128,
                    "description":"Optional gameplay asset the dialogue's quests and items are checked against."}
            }}),
            defaults: serde_json::to_value(DialogueParticipantComponent::default())
                .expect("defaults"),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
    ]
}

/// Why an inventory, quest or dialogue call was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgressionError {
    /// No project runtime.
    Unavailable,
    NoInventory,
    NoQuestLog,
    NoConversation,
    Inventory(InventoryError),
    Quest(QuestError),
    Dialogue(DialogueError),
}

impl ProgressionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable => "progression.unavailable",
            Self::NoInventory => "progression.no_inventory",
            Self::NoQuestLog => "progression.no_quest_log",
            Self::NoConversation => "progression.no_conversation",
            Self::Inventory(e) => e.code(),
            Self::Quest(e) => e.code(),
            Self::Dialogue(e) => e.code(),
        }
    }
}

impl std::fmt::Display for ProgressionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ProgressionError {}

impl From<InventoryError> for ProgressionError {
    fn from(e: InventoryError) -> Self {
        Self::Inventory(e)
    }
}
impl From<QuestError> for ProgressionError {
    fn from(e: QuestError) -> Self {
        Self::Quest(e)
    }
}
impl From<DialogueError> for ProgressionError {
    fn from(e: DialogueError) -> Self {
        Self::Dialogue(e)
    }
}

type Outcome<T> = std::result::Result<T, ProgressionError>;

struct InvState {
    config: InventoryComponent,
    table: Arc<ItemTable>,
    container: Container,
    equipment: Equipment,
    /// Effect handle per equipment slot.
    effects: BTreeMap<String, engine_gameplay::ability::EffectHandle>,
}

struct QuestLogState {
    config: QuestLogComponent,
    table: Arc<QuestTable>,
    listened: BTreeSet<String>,
    log: QuestLog,
}

struct ConvState {
    config: DialogueParticipantComponent,
    dialogue: Arc<Dialogue>,
    active: Option<(Entity, Conversation)>,
}

/// The progression service.
#[derive(Resource, Default)]
pub struct Progression {
    inv: BTreeMap<Entity, InvState>,
    quests: BTreeMap<Entity, QuestLogState>,
    convs: BTreeMap<Entity, ConvState>,
    libraries: BTreeMap<String, Arc<engine_gameplay::ability::Library>>,
    outbox: Vec<EngineEvent>,
    emitted: usize,
    dropped_events: u64,
    dropped_rewards: u64,
}

pub(crate) fn register(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<InventoryComponent>(app, INVENTORY, 1)?;
    crate::register_component::<QuestLogComponent>(app, QUEST_LOG, 1)?;
    crate::register_component::<DialogueParticipantComponent>(app, DIALOGUE_PARTICIPANT, 1)?;
    app.world_mut().insert_resource(Progression::default());
    Ok(())
}

fn with_prog<T>(
    world: &mut World,
    run: impl FnOnce(&mut World, &mut Progression) -> T,
) -> Option<T> {
    let mut service = world.remove_resource::<Progression>()?;
    let out = run(world, &mut service);
    world.insert_resource(service);
    Some(out)
}

fn authored(world: &World, entity: Entity) -> Option<String> {
    world.get_entity(entity).ok()?;
    Some(
        world
            .get::<AuthoredId>(entity)
            .map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone()),
    )
}

fn push(p: &mut Progression, name: &str, source: Option<Entity>, target: Entity, payload: Value) {
    p.outbox.push(EngineEvent {
        name: name.to_owned(),
        source,
        target: Some(target),
        payload,
    });
}

/// Emits the queued events (bounded per tick, dropped when the queue is full).
fn flush(world: &mut World, p: &mut Progression) {
    let queued = std::mem::take(&mut p.outbox);
    let room = MAX_EVENTS_PER_TICK.saturating_sub(p.emitted);
    let mut out: Vec<EngineEvent> = queued
        .into_iter()
        .filter(|e| e.target.is_none_or(|t| world.get_entity(t).is_ok()))
        .collect();
    if out.len() > room {
        p.dropped_events += (out.len() - room) as u64;
        out.truncate(room);
    }
    p.emitted += out.len();
    let count = out.len() as u64;
    if count > 0 && crate::emit_events(world, out).is_err() {
        p.dropped_events += count;
    }
}

// ---------------------------------------------------------------------- sync

fn library(
    world: &World,
    p: &mut Progression,
    id: &str,
) -> Result<Arc<engine_gameplay::ability::Library>> {
    if let Some(lib) = p.libraries.get(id) {
        return Ok(lib.clone());
    }
    let project = world.resource::<ProjectResource>().0.clone();
    let lib = load_definitions(&project, id)?;
    p.libraries.insert(id.to_owned(), lib.clone());
    Ok(lib)
}

fn invalid(entity: &str, message: impl std::fmt::Display) -> Error {
    Error::new(
        "progression.component.invalid",
        format!("entity {entity}: {message}"),
    )
}

fn build_inventory(
    project: &Project,
    entity: &str,
    config: &InventoryComponent,
) -> Result<InvState> {
    if config.definitions.is_empty() {
        return Err(invalid(entity, "definitions must name a gameplay asset"));
    }
    config.capacity.validate().map_err(|m| invalid(entity, m))?;
    let library = load_definitions(project, &config.definitions)?;
    let table =
        Arc::new(ItemTable::new(&library.definitions().items).map_err(|m| invalid(entity, m))?);
    let mut container = Container::new(config.capacity);
    for initial in &config.items {
        container
            .add(&table, &initial.item, initial.count)
            .map_err(|e| {
                invalid(
                    entity,
                    format!("initial item {:?}: {}", initial.item, e.code()),
                )
            })?;
    }
    let equipment = Equipment::new(config.equipment.clone()).map_err(|m| invalid(entity, m))?;
    Ok(InvState {
        config: config.clone(),
        table,
        container,
        equipment,
        effects: BTreeMap::new(),
    })
}

fn sync(world: &mut World, p: &mut Progression) -> Result<()> {
    let project = world.resource::<ProjectResource>().0.clone();
    let name = |world: &World, e: Entity| authored(world, e).unwrap_or_default();

    let configs: BTreeMap<Entity, InventoryComponent> = world
        .query::<(Entity, &InventoryComponent)>()
        .iter(world)
        .map(|(e, c)| (e, c.clone()))
        .collect();
    let stale: Vec<Entity> = p
        .inv
        .iter()
        .filter(|(e, s)| configs.get(e) != Some(&s.config))
        .map(|(e, _)| *e)
        .collect();
    for entity in stale {
        if let Some(state) = p.inv.remove(&entity) {
            for (_, handle) in state.effects {
                let _ = super::remove_effect(world, entity, handle);
            }
        }
    }
    for (entity, config) in configs {
        if let std::collections::btree_map::Entry::Vacant(slot) = p.inv.entry(entity) {
            slot.insert(build_inventory(&project, &name(world, entity), &config)?);
        }
    }

    let configs: BTreeMap<Entity, QuestLogComponent> = world
        .query::<(Entity, &QuestLogComponent)>()
        .iter(world)
        .map(|(e, c)| (e, c.clone()))
        .collect();
    p.quests.retain(|e, s| configs.get(e) == Some(&s.config));
    for (entity, config) in configs {
        if p.quests.contains_key(&entity) {
            continue;
        }
        let label = name(world, entity);
        if config.definitions.is_empty() {
            return Err(invalid(&label, "definitions must name a gameplay asset"));
        }
        let lib = library(world, p, &config.definitions)?;
        let table = Arc::new(QuestTable::new(&lib.definitions().quests));
        let listened = table
            .listened_events()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut state = QuestLogState {
            config: config.clone(),
            table,
            listened,
            log: QuestLog::default(),
        };
        let mut events = Vec::new();
        {
            let ctx = InvCtx(p.inv.get(&entity).map(|i| &i.container));
            for flag in &config.flags {
                events.extend(state.log.set_flag(&state.table, flag, &ctx));
            }
            for quest in &config.start {
                events.extend(state.log.start(&state.table, quest, &ctx).map_err(|e| {
                    invalid(&label, format!("start quest {quest:?}: {}", e.code()))
                })?);
            }
        }
        p.quests.insert(entity, state);
        quest_events(world, p, entity, events, 0);
    }

    let configs: BTreeMap<Entity, DialogueParticipantComponent> = world
        .query::<(Entity, &DialogueParticipantComponent)>()
        .iter(world)
        .map(|(e, c)| (e, c.clone()))
        .collect();
    p.convs.retain(|e, s| configs.get(e) == Some(&s.config));
    for (entity, config) in configs {
        if p.convs.contains_key(&entity) {
            continue;
        }
        let dialogue = compile_dialogue(&project, &config.graph)?;
        p.convs.insert(
            entity,
            ConvState {
                config,
                dialogue: Arc::new(dialogue),
                active: None,
            },
        );
    }
    flush(world, p);
    Ok(())
}

fn compile_dialogue(project: &Project, id: &str) -> Result<Dialogue> {
    let document = crate::graphs::load_graph(project, id)?;
    if document.kind != dialogue::KIND {
        return Err(Error::new(
            "dialogue.graph.kind",
            format!(
                "graph asset {id} is a {} graph, not a dialogue",
                document.kind
            ),
        ));
    }
    dialogue::compile(&document).map_err(|d| crate::graphs::into_error(d, &project.assets[id].path))
}

struct InvCtx<'a>(Option<&'a Container>);
impl QuestContext for InvCtx<'_> {
    fn has_item(&self, item: &str, count: u32) -> bool {
        self.0.is_some_and(|c| c.has(item, count))
    }
}

/// Scene checks: assets exist and compile, initial items fit, quests start, dialogues
/// reference known quests and items.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    for entity in &scene.entities {
        let custom = &entity.components.custom;
        if let Some(value) = custom.get(INVENTORY) {
            let config: InventoryComponent = serde_json::from_value(value.data.clone())?;
            build_inventory(project, &entity.id, &config)?;
        }
        if let Some(value) = custom.get(QUEST_LOG) {
            let config: QuestLogComponent = serde_json::from_value(value.data.clone())?;
            if config.definitions.is_empty() {
                return Err(invalid(
                    &entity.id,
                    "definitions must name a gameplay asset",
                ));
            }
            let lib = load_definitions(project, &config.definitions)?;
            let table = QuestTable::new(&lib.definitions().quests);
            for quest in &config.start {
                if table.get(quest).is_none() {
                    return Err(invalid(&entity.id, format!("unknown quest {quest:?}")));
                }
            }
        }
        if let Some(value) = custom.get(DIALOGUE_PARTICIPANT) {
            let config: DialogueParticipantComponent = serde_json::from_value(value.data.clone())?;
            if config.graph.is_empty() {
                return Err(invalid(&entity.id, "graph must name a dialogue asset"));
            }
            project.require_asset(&config.graph, "graph")?;
            let dialogue = compile_dialogue(project, &config.graph)?;
            if let Some(id) = &config.definitions {
                let lib = load_definitions(project, id)?;
                let defs = lib.definitions();
                for quest in dialogue.quests() {
                    if !defs.quests.iter().any(|q| &q.id == quest) {
                        return Err(Error::new(
                            "dialogue.reference.unknown",
                            format!(
                                "entity {}: dialogue {} names unknown quest {quest:?}",
                                entity.id, config.graph
                            ),
                        ));
                    }
                }
                for item in dialogue.items() {
                    if !defs.items.iter().any(|i| &i.id == item) {
                        return Err(Error::new(
                            "dialogue.reference.unknown",
                            format!(
                                "entity {}: dialogue {} names unknown item {item:?}",
                                entity.id, config.graph
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- inventory ops

fn changed(
    world: &World,
    p: &mut Progression,
    entity: Entity,
    deltas: &[engine_gameplay::inventory::Delta],
) {
    for delta in deltas {
        let total = p
            .inv
            .get(&entity)
            .map_or(0, |s| s.container.count(&delta.item));
        push(
            p,
            EVENT_CHANGED,
            None,
            entity,
            json!({"item":delta.item,"delta":delta.delta,"count":total,
                "entity":authored(world, entity)}),
        );
    }
}

fn full(p: &mut Progression, entity: Entity, item: &str, count: u32, error: InventoryError) {
    push(
        p,
        EVENT_FULL,
        None,
        entity,
        json!({"item":item,"count":count,"reason":error.code()}),
    );
}

/// Re-checks the quest objectives that look at the inventory.
fn after_inventory(world: &mut World, p: &mut Progression, entity: Entity, depth: usize) {
    let events = {
        let Progression { inv, quests, .. } = p;
        let Some(quest) = quests.get_mut(&entity) else {
            return;
        };
        let ctx = InvCtx(inv.get(&entity).map(|i| &i.container));
        quest.log.refresh(&quest.table, &ctx)
    };
    quest_events(world, p, entity, events, depth);
}

fn inv_add(
    world: &mut World,
    p: &mut Progression,
    entity: Entity,
    item: &str,
    count: u32,
    depth: usize,
) -> Outcome<()> {
    let state = p
        .inv
        .get_mut(&entity)
        .ok_or(ProgressionError::NoInventory)?;
    match state.container.add(&state.table, item, count) {
        Ok(deltas) => {
            changed(world, p, entity, &deltas);
            after_inventory(world, p, entity, depth);
            Ok(())
        }
        Err(error) => {
            if matches!(error, InventoryError::Full | InventoryError::Overweight) {
                full(p, entity, item, count, error);
            }
            Err(error.into())
        }
    }
}

/// Turns quest events into engine events and delivers completion rewards.
fn quest_events(
    world: &mut World,
    p: &mut Progression,
    entity: Entity,
    events: Vec<QuestEvent>,
    depth: usize,
) {
    for event in events {
        match event {
            QuestEvent::Started { quest } => {
                push(p, EVENT_QUEST_STARTED, None, entity, json!({"quest":quest}));
            }
            QuestEvent::ObjectiveProgressed {
                quest,
                objective,
                value,
                target,
            } => push(
                p,
                EVENT_QUEST_PROGRESSED,
                None,
                entity,
                json!({"quest":quest,"objective":objective,"value":value,"target":target}),
            ),
            QuestEvent::StageAdvanced { quest, stage } => {
                push(
                    p,
                    EVENT_QUEST_STAGE,
                    None,
                    entity,
                    json!({"quest":quest,"stage":stage}),
                );
            }
            QuestEvent::Failed { quest } => {
                push(p, EVENT_QUEST_FAILED, None, entity, json!({"quest":quest}));
            }
            QuestEvent::Completed { quest, rewards } => {
                push(
                    p,
                    EVENT_QUEST_COMPLETED,
                    None,
                    entity,
                    json!({"quest":quest}),
                );
                if depth >= MAX_CHAIN {
                    p.dropped_rewards += 1;
                    continue;
                }
                for reward in &rewards.items {
                    if inv_add(world, p, entity, &reward.item, reward.count, depth + 1).is_err() {
                        p.dropped_rewards += 1;
                    }
                }
                if let Some(effect) = &rewards.effect {
                    let _ = super::apply_effect(world, entity, entity, effect, 1);
                }
                if let Some(event) = &rewards.event {
                    push(p, &event.name, None, entity, event.payload.clone());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------- natives

fn run<T>(
    world: &mut World,
    f: impl FnOnce(&mut World, &mut Progression) -> Outcome<T>,
) -> Outcome<T> {
    let result = with_prog(world, |w, p| {
        let out = f(w, p);
        flush(w, p);
        out
    });
    result.unwrap_or(Err(ProgressionError::Unavailable))
}

/// Adds items (stacking first). A refusal leaves the inventory unchanged; a full or
/// overweight inventory also emits `inventory.full`.
///
/// # Errors
/// [`ProgressionError`]: no inventory, or the pure operation's error.
pub fn add_item(world: &mut World, entity: Entity, item: &str, count: u32) -> Outcome<()> {
    run(world, |w, p| inv_add(w, p, entity, item, count, 0))
}

/// Removes items (highest slot first).
///
/// # Errors
/// As [`add_item`].
pub fn remove_item(world: &mut World, entity: Entity, item: &str, count: u32) -> Outcome<()> {
    run(world, |w, p| {
        let state = p
            .inv
            .get_mut(&entity)
            .ok_or(ProgressionError::NoInventory)?;
        let deltas = state.container.remove(item, count)?;
        changed(w, p, entity, &deltas);
        after_inventory(w, p, entity, 0);
        Ok(())
    })
}

/// Moves (or merges, or swaps) a stack inside one inventory.
///
/// # Errors
/// As [`add_item`].
pub fn move_item(
    world: &mut World,
    entity: Entity,
    from: usize,
    to: usize,
    count: Option<u32>,
) -> Outcome<()> {
    run(world, |_, p| {
        let state = p
            .inv
            .get_mut(&entity)
            .ok_or(ProgressionError::NoInventory)?;
        state.container.move_stack(&state.table, from, to, count)?;
        Ok(())
    })
}

/// Splits `count` units off a stack into the first empty slot; returns that slot.
///
/// # Errors
/// As [`add_item`].
pub fn split_item(world: &mut World, entity: Entity, from: usize, count: u32) -> Outcome<usize> {
    run(world, |_, p| {
        let state = p
            .inv
            .get_mut(&entity)
            .ok_or(ProgressionError::NoInventory)?;
        Ok(state.container.split(from, count)?)
    })
}

/// Moves items between two entities' inventories, atomically.
///
/// # Errors
/// As [`add_item`]; neither inventory changes on error.
pub fn transfer_item(
    world: &mut World,
    from: Entity,
    to: Entity,
    item: &str,
    count: u32,
) -> Outcome<()> {
    run(world, |w, p| {
        if from == to {
            return Err(InventoryError::Mismatch.into());
        }
        let table = p
            .inv
            .get(&from)
            .ok_or(ProgressionError::NoInventory)?
            .table
            .clone();
        let mut source = p
            .inv
            .get(&from)
            .ok_or(ProgressionError::NoInventory)?
            .container
            .clone();
        let mut destination = p
            .inv
            .get(&to)
            .ok_or(ProgressionError::NoInventory)?
            .container
            .clone();
        match transfer(&table, &mut source, &mut destination, item, count) {
            Ok(_) => {
                p.inv.get_mut(&from).expect("checked").container = source;
                p.inv.get_mut(&to).expect("checked").container = destination;
                for (entity, delta) in [(from, -i64::from(count)), (to, i64::from(count))] {
                    changed(
                        w,
                        p,
                        entity,
                        &[engine_gameplay::inventory::Delta {
                            item: item.to_owned(),
                            delta,
                        }],
                    );
                    after_inventory(w, p, entity, 0);
                }
                Ok(())
            }
            Err(error) => {
                if matches!(error, InventoryError::Full | InventoryError::Overweight) {
                    full(p, to, item, count, error);
                }
                Err(error.into())
            }
        }
    })
}

/// Wears one unit of the item in inventory slot `from` in equipment slot `slot`. When the
/// item has an `equip_effect` and an ability system runs, the effect is applied to the
/// entity until the item is taken off.
///
/// # Errors
/// As [`add_item`].
pub fn equip_item(world: &mut World, entity: Entity, from: usize, slot: &str) -> Outcome<()> {
    run(world, |w, p| {
        let state = p
            .inv
            .get_mut(&entity)
            .ok_or(ProgressionError::NoInventory)?;
        let item = state
            .equipment
            .equip(&state.table, &mut state.container, from, slot)?;
        let effect = state.table.get(&item).and_then(|d| d.equip_effect.clone());
        let mut handle = None;
        if let Some(effect) = effect
            && let Ok(Ok(Some(h))) = super::apply_effect(w, entity, entity, &effect, 1)
        {
            handle = Some(h);
        }
        if let Some(h) = handle {
            p.inv
                .get_mut(&entity)
                .expect("present")
                .effects
                .insert(slot.to_owned(), h);
        }
        push(
            p,
            EVENT_EQUIPPED,
            None,
            entity,
            json!({"slot":slot,"item":item}),
        );
        changed(
            w,
            p,
            entity,
            &[engine_gameplay::inventory::Delta { item, delta: -1 }],
        );
        after_inventory(w, p, entity, 0);
        Ok(())
    })
}

/// Takes the item off and returns it to the inventory (removing its equip effect).
///
/// # Errors
/// As [`add_item`]; a full inventory keeps the item worn.
pub fn unequip_item(world: &mut World, entity: Entity, slot: &str) -> Outcome<()> {
    run(world, |w, p| {
        let state = p
            .inv
            .get_mut(&entity)
            .ok_or(ProgressionError::NoInventory)?;
        let item = state
            .equipment
            .unequip(&state.table, &mut state.container, slot)?;
        if let Some(handle) = state.effects.remove(slot) {
            let _ = super::remove_effect(w, entity, handle);
        }
        push(
            p,
            EVENT_UNEQUIPPED,
            None,
            entity,
            json!({"slot":slot,"item":item}),
        );
        changed(
            w,
            p,
            entity,
            &[engine_gameplay::inventory::Delta { item, delta: 1 }],
        );
        after_inventory(w, p, entity, 0);
        Ok(())
    })
}

/// Units of `item` the entity holds (0 without an inventory).
pub fn item_count(world: &World, entity: Entity, item: &str) -> u32 {
    world
        .get_resource::<Progression>()
        .and_then(|p| p.inv.get(&entity))
        .map_or(0, |s| s.container.count(item))
}

/// The item in inventory slot `slot`.
pub fn slot_item(world: &World, entity: Entity, slot: usize) -> Option<String> {
    world
        .get_resource::<Progression>()?
        .inv
        .get(&entity)?
        .container
        .slot(slot)
        .map(|s| s.item.clone())
}

/// The item worn in an equipment slot.
pub fn equipped_item(world: &World, entity: Entity, slot: &str) -> Option<String> {
    world
        .get_resource::<Progression>()?
        .inv
        .get(&entity)?
        .equipment
        .worn()
        .get(slot)
        .cloned()
}

/// Starts a quest of the entity.
///
/// # Errors
/// No quest log, or the [`QuestError`].
pub fn start_quest(world: &mut World, entity: Entity, quest: &str) -> Outcome<()> {
    run(world, |w, p| start_quest_in(w, p, entity, quest))
}

fn start_quest_in(
    world: &mut World,
    p: &mut Progression,
    entity: Entity,
    quest: &str,
) -> Outcome<()> {
    let events = {
        let Progression { inv, quests, .. } = p;
        let state = quests
            .get_mut(&entity)
            .ok_or(ProgressionError::NoQuestLog)?;
        let ctx = InvCtx(inv.get(&entity).map(|i| &i.container));
        state.log.start(&state.table, quest, &ctx)?
    };
    quest_events(world, p, entity, events, 0);
    Ok(())
}

/// Drops an active quest's progress.
///
/// # Errors
/// No quest log.
pub fn abandon_quest(world: &mut World, entity: Entity, quest: &str) -> Outcome<bool> {
    run(world, |_, p| {
        Ok(p.quests
            .get_mut(&entity)
            .ok_or(ProgressionError::NoQuestLog)?
            .log
            .abandon(quest))
    })
}

/// Fails an active quest.
///
/// # Errors
/// No quest log.
pub fn fail_quest(world: &mut World, entity: Entity, quest: &str) -> Outcome<()> {
    run(world, |w, p| {
        let events = p
            .quests
            .get_mut(&entity)
            .ok_or(ProgressionError::NoQuestLog)?
            .log
            .fail(quest);
        quest_events(w, p, entity, events, 0);
        Ok(())
    })
}

/// Sets a quest flag (flag objectives complete).
///
/// # Errors
/// No quest log.
pub fn set_quest_flag(world: &mut World, entity: Entity, flag: &str) -> Outcome<()> {
    run(world, |w, p| {
        let events = {
            let Progression { inv, quests, .. } = p;
            let state = quests
                .get_mut(&entity)
                .ok_or(ProgressionError::NoQuestLog)?;
            let ctx = InvCtx(inv.get(&entity).map(|i| &i.container));
            state.log.set_flag(&state.table, flag, &ctx)
        };
        quest_events(w, p, entity, events, 0);
        Ok(())
    })
}

/// State of a quest (`Inactive` without a log).
pub fn quest_state(world: &World, entity: Entity, quest: &str) -> QuestState {
    world
        .get_resource::<Progression>()
        .and_then(|p| p.quests.get(&entity))
        .map_or(QuestState::Inactive, |s| s.log.state(quest))
}

/// `(objective, value, target)` of an active quest's current stage.
pub fn quest_objectives(world: &World, entity: Entity, quest: &str) -> Vec<(String, u32, u32)> {
    world
        .get_resource::<Progression>()
        .and_then(|p| p.quests.get(&entity))
        .map_or_else(Vec::new, |s| s.log.objectives(&s.table, quest))
}

// --------------------------------------------------------------------- dialogue

struct WorldHost<'a> {
    world: &'a mut World,
    p: &'a mut Progression,
    player: Entity,
    npc: Entity,
}

impl DialogueHost for WorldHost<'_> {
    fn quest_state(&mut self, quest: &str) -> QuestState {
        self.p
            .quests
            .get(&self.player)
            .map_or(QuestState::Inactive, |s| s.log.state(quest))
    }
    fn has_item(&mut self, item: &str, count: u32) -> bool {
        self.p
            .inv
            .get(&self.player)
            .is_some_and(|s| s.container.has(item, count))
    }
    fn emit_event(&mut self, name: &str, payload: &Value) {
        push(self.p, name, Some(self.npc), self.player, payload.clone());
    }
    fn start_quest(&mut self, quest: &str) {
        let _ = start_quest_in(self.world, self.p, self.player, quest);
    }
    fn give_item(&mut self, item: &str, count: u32) {
        let _ = inv_add(self.world, self.p, self.player, item, count, 0);
    }
}

fn present(world: &World, p: &mut Progression, npc: Entity, player: Entity, output: &Output) {
    let who = json!({"npc":authored(world, npc),"player":authored(world, player)});
    let (name, mut payload) = match output {
        Output::Line {
            speaker,
            text,
            node,
        } => (
            EVENT_DIALOGUE_LINE,
            json!({"speaker":speaker,"text":text,"node":node}),
        ),
        Output::Choices {
            prompt,
            options,
            node,
        } => (
            EVENT_DIALOGUE_CHOICES,
            json!({"prompt":prompt,"options":options,"node":node}),
        ),
        Output::Ended { reason } => (EVENT_DIALOGUE_ENDED, json!({"reason":reason})),
    };
    for key in ["npc", "player"] {
        payload[key] = who[key].clone();
    }
    push(p, name, Some(npc), player, payload);
}

/// Runs one conversation call: takes the conversation out of the service so the host can
/// change inventories and quests, then puts it back (unless it ended).
fn converse(
    world: &mut World,
    p: &mut Progression,
    npc: Entity,
    call: impl FnOnce(
        &mut Conversation,
        &mut dyn DialogueHost,
    ) -> std::result::Result<Output, DialogueError>,
) -> Outcome<Output> {
    let state = p
        .convs
        .get_mut(&npc)
        .ok_or(ProgressionError::NoConversation)?;
    let (player, mut conversation) = state
        .active
        .take()
        .ok_or(ProgressionError::NoConversation)?;
    let result = {
        let mut host = WorldHost {
            world,
            p,
            player,
            npc,
        };
        call(&mut conversation, &mut host)
    };
    let state = p
        .convs
        .get_mut(&npc)
        .ok_or(ProgressionError::NoConversation)?;
    match result {
        Ok(output) => {
            if !matches!(output, Output::Ended { .. }) {
                state.active = Some((player, conversation));
            }
            present(world, p, npc, player, &output);
            Ok(output)
        }
        Err(error) => {
            state.active = Some((player, conversation));
            Err(error.into())
        }
    }
}

fn start_dialogue_in(
    world: &mut World,
    p: &mut Progression,
    npc: Entity,
    player: Entity,
) -> Outcome<Output> {
    let state = p
        .convs
        .get_mut(&npc)
        .ok_or(ProgressionError::NoConversation)?;
    state.active = Some((player, Conversation::new(state.dialogue.clone())));
    converse(world, p, npc, |c, host| c.begin(host))
}

/// Starts a conversation between `npc` (which carries the dialogue) and `player` (whose
/// quests and inventory it tests and changes). Replaces a conversation in progress.
///
/// # Errors
/// [`ProgressionError::NoConversation`] when `npc` is no dialogue participant.
pub fn start_dialogue(world: &mut World, npc: Entity, player: Entity) -> Outcome<Output> {
    run(world, |w, p| start_dialogue_in(w, p, npc, player))
}

/// Continues after a line.
///
/// # Errors
/// No conversation, or the conversation is not waiting on a line.
pub fn advance_dialogue(world: &mut World, npc: Entity) -> Outcome<Output> {
    run(world, |w, p| converse(w, p, npc, |c, host| c.advance(host)))
}

/// Picks an offered option.
///
/// # Errors
/// No conversation, not waiting on a choice, or an index that was not offered.
pub fn select_choice(world: &mut World, npc: Entity, index: usize) -> Outcome<Output> {
    run(world, |w, p| {
        converse(w, p, npc, |c, host| c.select(index, host))
    })
}

/// Drops a conversation without running its end.
pub fn end_dialogue(world: &mut World, npc: Entity) -> bool {
    with_prog(world, |_, p| {
        p.convs
            .get_mut(&npc)
            .is_some_and(|s| s.active.take().is_some())
    })
    .unwrap_or(false)
}

// -------------------------------------------------------------------- the step

/// Syncs components, then feeds this tick's delivered events to quests and dialogue
/// inputs. Runs once per tick before the trees.
pub fn step(world: &mut World, _dt: f32) -> Result<()> {
    with_prog(world, |world, p| -> Result<()> {
        p.emitted = 0;
        sync(world, p)?;
        if p.quests.is_empty() && p.convs.is_empty() {
            return Ok(());
        }
        let delivered: Vec<(String, Option<Entity>, Option<Entity>, Value)> = world
            .get_resource::<DeliveredEvents>()
            .map(|d| {
                d.0.iter()
                    .map(|e| (e.name.clone(), e.source, e.target, e.payload.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (name, source, target, payload) in delivered {
            match name.as_str() {
                EVENT_DIALOGUE_START => {
                    if let (Some(npc), Some(player)) = (target, source) {
                        let _ = start_dialogue_in(world, p, npc, player);
                    }
                }
                EVENT_DIALOGUE_ADVANCE => {
                    if let Some(npc) = target {
                        let _ = converse(world, p, npc, |c, host| c.advance(host));
                    }
                }
                EVENT_DIALOGUE_SELECT => {
                    if let Some(npc) = target {
                        let index = payload
                            .get("index")
                            .and_then(Value::as_u64)
                            .unwrap_or(u64::MAX);
                        let _ = converse(world, p, npc, |c, host| c.select(index as usize, host));
                    }
                }
                _ => {}
            }
            let owners: Vec<Entity> = p
                .quests
                .iter()
                .filter(|(e, s)| s.listened.contains(&name) && target.is_none_or(|t| t == **e))
                .map(|(e, _)| *e)
                .collect();
            for entity in owners {
                let events = {
                    let Progression { inv, quests, .. } = &mut *p;
                    let state = quests.get_mut(&entity).expect("listed");
                    let ctx = InvCtx(inv.get(&entity).map(|i| &i.container));
                    state.log.on_event(&state.table, &name, &payload, &ctx)
                };
                quest_events(world, p, entity, events, 0);
            }
        }
        flush(world, p);
        Ok(())
    })
    .transpose()?;
    Ok(())
}

/// Releases equip effects without events; called when the runtime stops.
pub fn shutdown(world: &mut World) {
    with_prog(world, |w, p| {
        for (entity, state) in &mut p.inv {
            for (_, handle) in std::mem::take(&mut state.effects) {
                let _ = super::remove_effect(w, *entity, handle);
            }
        }
        p.outbox.clear();
    });
}

/// Brings the service in line with the world (runtime start, scene transitions).
pub fn prepare(world: &mut World) -> Result<()> {
    with_prog(world, |world, p| {
        p.emitted = 0;
        sync(world, p)
    })
    .transpose()?;
    Ok(())
}

// -------------------------------------------------------------------- telemetry

#[derive(Clone, Debug, Serialize)]
pub struct InventoryStatus {
    pub entity: String,
    pub slots: usize,
    pub used: usize,
    pub weight: f32,
    /// Units per item, the first 16 items by id.
    pub items: BTreeMap<String, u32>,
    pub equipped: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuestEntry {
    pub quest: String,
    pub state: &'static str,
    pub stage: Option<String>,
    /// `[objective, value, target]` of the current stage.
    pub objectives: Vec<(String, u32, u32)>,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuestLogStatus {
    pub entity: String,
    /// The first 16 tracked quests by id.
    pub quests: Vec<QuestEntry>,
    pub flags: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DialogueStatus {
    pub npc: String,
    pub player: Option<String>,
    /// `idle`, `line`, `choice` or `over` (absent when no conversation runs).
    pub waiting: Option<&'static str>,
}

/// The `progression` entry of the runtime snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct ProgressionStatus {
    pub inventories: Vec<InventoryStatus>,
    pub quest_logs: Vec<QuestLogStatus>,
    pub dialogues: Vec<DialogueStatus>,
    pub dropped_events: u64,
    pub dropped_rewards: u64,
}

/// Bounded telemetry; `None` without any of the three components.
pub fn status(world: &World) -> Option<ProgressionStatus> {
    let p = world.get_resource::<Progression>()?;
    if p.inv.is_empty() && p.quests.is_empty() && p.convs.is_empty() {
        return None;
    }
    let by_name = |it: &mut dyn Iterator<Item = Entity>| -> Vec<(String, Entity)> {
        let mut named: Vec<(String, Entity)> = it
            .filter_map(|e| authored(world, e).map(|n| (n, e)))
            .collect();
        named.sort();
        named.truncate(STATUS_MAX_ENTITIES);
        named
    };
    let inventories = by_name(&mut p.inv.keys().copied())
        .into_iter()
        .map(|(entity, e)| {
            let s = &p.inv[&e];
            InventoryStatus {
                entity,
                slots: s.container.slots().len(),
                used: s.container.slots().iter().flatten().count(),
                weight: s.container.weight(&s.table),
                items: s
                    .container
                    .totals()
                    .into_iter()
                    .take(STATUS_MAX_ITEMS)
                    .collect(),
                equipped: s.equipment.worn().clone(),
            }
        })
        .collect();
    let quest_logs = by_name(&mut p.quests.keys().copied())
        .into_iter()
        .map(|(entity, e)| {
            let s = &p.quests[&e];
            QuestLogStatus {
                entity,
                quests: s
                    .log
                    .states()
                    .into_iter()
                    .take(STATUS_MAX_ITEMS)
                    .map(|(quest, state)| QuestEntry {
                        stage: s.log.stage(&s.table, &quest),
                        objectives: if state == QuestState::Active {
                            s.log.objectives(&s.table, &quest)
                        } else {
                            Vec::new()
                        },
                        state: state.as_str(),
                        quest,
                    })
                    .collect(),
                flags: s
                    .log
                    .flags()
                    .take(STATUS_MAX_ITEMS)
                    .map(str::to_owned)
                    .collect(),
            }
        })
        .collect();
    let dialogues = by_name(&mut p.convs.keys().copied())
        .into_iter()
        .map(|(npc, e)| {
            let s = &p.convs[&e];
            DialogueStatus {
                npc,
                player: s
                    .active
                    .as_ref()
                    .and_then(|(player, _)| authored(world, *player)),
                waiting: s.active.as_ref().map(|(_, c)| c.waiting_for()),
            }
        })
        .collect();
    Some(ProgressionStatus {
        inventories,
        quest_logs,
        dialogues,
        dropped_events: p.dropped_events,
        dropped_rewards: p.dropped_rewards,
    })
}
