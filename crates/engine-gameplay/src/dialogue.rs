//! Dialogue: the `dialogue` graph kind, its compiler and a conversation runtime.
//!
//! Nodes: `start` (root), `line` (speaker, text key), `choice` with `option` children
//! (text key, conditions), `set_variable`, `emit_event`, `start_quest`, `give_item`,
//! `jump` (to another node by id, so graphs stay acyclic) and `end`. Flow goes through
//! `next` ports; every `next` must be connected. Text is a localisation **key**, never
//! literal prose. Conditions test dialogue variables, quest states and inventory contents.
//!
//! A [`Conversation`] runs until the next line (waits for [`advance`](Conversation::advance)),
//! the next set of offered choices (waits for [`select`](Conversation::select)) or the
//! end; side effects go through the [`DialogueHost`]. Offered choices are the options
//! whose conditions hold, in edge order, so a conversation is deterministic.
use crate::quest::QuestState;
use engine_graph::{
    GraphDiagnostic, GraphDocument, GraphIndex, GraphKind, GraphLimits, NodeTypeDef,
    NodeTypeRegistry, ParamDef, ParamType, PortDef, PortType, Strictness, codes, validate,
    value_conforms,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, OnceLock},
};

/// Name of the graph kind in `GraphDocument::kind`.
pub const KIND: &str = "dialogue";
/// Most nodes one dialogue may have.
pub const MAX_NODES: usize = 512;
/// Most options one choice may offer.
pub const MAX_OPTIONS: usize = 16;
/// Most nodes one `advance` or `select` may execute (a loop of jumps ends the dialogue).
pub const MAX_STEPS: usize = 256;

/// Node type names.
pub mod node {
    pub const START: &str = "start";
    pub const LINE: &str = "line";
    pub const CHOICE: &str = "choice";
    pub const OPTION: &str = "option";
    pub const SET_VARIABLE: &str = "set_variable";
    pub const EMIT_EVENT: &str = "emit_event";
    pub const START_QUEST: &str = "start_quest";
    pub const GIVE_ITEM: &str = "give_item";
    pub const JUMP: &str = "jump";
    pub const END: &str = "end";
}

/// Diagnostic codes added by the dialogue compiler.
pub mod dlg_codes {
    pub const CHOICE_EMPTY: &str = "dlg.choice.empty";
    pub const CHOICE_LIMIT: &str = "dlg.choice.options";
    pub const JUMP_TARGET: &str = "dlg.jump.target";
    pub const CONDITION: &str = "dlg.condition.invalid";
    pub const VALUE_TYPE: &str = "dlg.value.type";
    pub const EVENT_NAME: &str = "dlg.event.name";
    pub const EVENT_PAYLOAD: &str = "dlg.event.payload";
    pub const TEXT: &str = "dlg.text.invalid";
    /// Every code above.
    pub const ALL: &[&str] = &[
        CHOICE_EMPTY,
        CHOICE_LIMIT,
        JUMP_TARGET,
        CONDITION,
        VALUE_TYPE,
        EVENT_NAME,
        EVENT_PAYLOAD,
        TEXT,
    ];
}

/// Comparison of a variable condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    IsSet,
    IsNotSet,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

fn one() -> u32 {
    1
}
fn is_set() -> Compare {
    Compare::IsSet
}

/// A condition on an option.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    /// A dialogue variable compared with `value`.
    Variable {
        key: String,
        #[serde(default = "is_set")]
        operator: Compare,
        #[serde(default)]
        value: Option<Value>,
    },
    /// The conversation owner's quest is in `state`.
    Quest {
        quest: String,
        state: QuestStateName,
    },
    /// The conversation owner's inventory holds `count` of `item`.
    Item {
        item: String,
        #[serde(default = "one")]
        count: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestStateName {
    Inactive,
    Active,
    Completed,
    Failed,
}

impl From<QuestStateName> for QuestState {
    fn from(name: QuestStateName) -> Self {
        match name {
            QuestStateName::Inactive => Self::Inactive,
            QuestStateName::Active => Self::Active,
            QuestStateName::Completed => Self::Completed,
            QuestStateName::Failed => Self::Failed,
        }
    }
}

fn registry() -> NodeTypeRegistry {
    let flow_in = || PortDef::input("parent", PortType::Flow).required();
    let next = || PortDef::output("next", PortType::Flow).required();
    let text = |description: &str| {
        ParamDef::new("text", ParamType::String)
            .required()
            .max_len(128)
            .describe(description)
    };
    let option_type = PortType::Custom("dlg.option".to_owned());
    NodeTypeRegistry::new()
        .with(
            NodeTypeDef::new(node::START, "root", "Entry of the conversation.")
                .root()
                .port(next()),
        )
        .with(
            NodeTypeDef::new(
                node::LINE,
                "flow",
                "A spoken line; the conversation waits for advance.",
            )
            .port(flow_in())
            .port(next())
            .param(
                ParamDef::new("speaker", ParamType::String)
                    .max_len(64)
                    .describe("Speaker id (a participant name or role)."),
            )
            .param(text("Localisation key of the line.")),
        )
        .with(
            NodeTypeDef::new(
                node::CHOICE,
                "flow",
                "Offers the options whose conditions hold; the conversation waits for select.",
            )
            .port(flow_in())
            .port(
                PortDef::output("options", option_type.clone())
                    .multiple()
                    .describe("Options in offering order."),
            )
            .param(
                ParamDef::new("prompt", ParamType::String)
                    .max_len(128)
                    .describe("Optional localisation key shown with the choices."),
            ),
        )
        .with(
            NodeTypeDef::new(
                node::OPTION,
                "option",
                "One choice. Hidden when a condition fails.",
            )
            .port(PortDef::input("owner", option_type).required())
            .port(next())
            .param(text("Localisation key of the option."))
            .param(
                ParamDef::new("conditions", ParamType::Json)
                    .describe("Array of {type: variable|quest|item, ...}; all must hold."),
            ),
        )
        .with(
            NodeTypeDef::new(
                node::SET_VARIABLE,
                "flow",
                "Writes a constant to a dialogue variable.",
            )
            .port(flow_in())
            .port(next())
            .param(ParamDef::new("key", ParamType::Variable).required())
            .param(ParamDef::new("value", ParamType::Json).required()),
        )
        .with(
            NodeTypeDef::new(
                node::EMIT_EVENT,
                "flow",
                "Emits an engine event at the conversation owner.",
            )
            .port(flow_in())
            .port(next())
            .param(
                ParamDef::new("event", ParamType::String)
                    .required()
                    .max_len(128),
            )
            .param(ParamDef::new("payload", ParamType::Json).describe("Optional JSON object.")),
        )
        .with(
            NodeTypeDef::new(
                node::START_QUEST,
                "flow",
                "Starts a quest of the owner (ignored when it cannot start).",
            )
            .port(flow_in())
            .port(next())
            .param(
                ParamDef::new("quest", ParamType::String)
                    .required()
                    .max_len(64),
            ),
        )
        .with(
            NodeTypeDef::new(
                node::GIVE_ITEM,
                "flow",
                "Adds items to the owner's inventory (skipped when they do not fit).",
            )
            .port(flow_in())
            .port(next())
            .param(
                ParamDef::new("item", ParamType::String)
                    .required()
                    .max_len(64),
            )
            .param(
                ParamDef::new("count", ParamType::Int)
                    .range(1.0, 100_000.0)
                    .default_value(json!(1)),
            ),
        )
        .with(
            NodeTypeDef::new(
                node::JUMP,
                "flow",
                "Continues at another node (a line, choice or action) by id.",
            )
            .port(flow_in())
            .param(
                ParamDef::new("target", ParamType::String)
                    .required()
                    .max_len(64),
            ),
        )
        .with(
            NodeTypeDef::new(node::END, "flow", "Ends the conversation.")
                .port(flow_in())
                .param(
                    ParamDef::new("reason", ParamType::String)
                        .max_len(64)
                        .describe("Reported with dialogue.ended."),
                ),
        )
}

fn build_kind() -> GraphKind {
    GraphKind::new(
        KIND,
        "Dialogue: lines, conditional choices and side effects over typed variables.",
        registry(),
    )
    .single_root()
    .variable_types(&[
        PortType::Bool,
        PortType::Int,
        PortType::Float,
        PortType::String,
    ])
    .limits(GraphLimits {
        max_nodes: MAX_NODES,
        max_edges: 4 * MAX_NODES,
        max_variables: 64,
    })
    .check(check_document)
}

/// The dialogue graph kind.
pub fn kind() -> GraphKind {
    shared_kind().clone()
}

pub fn shared_kind() -> &'static GraphKind {
    static CELL: OnceLock<GraphKind> = OnceLock::new();
    CELL.get_or_init(build_kind)
}

#[derive(Clone, Debug)]
pub(crate) enum Step {
    Line {
        speaker: String,
        text: String,
        next: usize,
    },
    Choice {
        prompt: Option<String>,
        options: Vec<usize>,
    },
    Option {
        text: String,
        conditions: Vec<Condition>,
        next: usize,
    },
    SetVariable {
        key: String,
        value: Value,
        next: usize,
    },
    Emit {
        event: String,
        payload: Value,
        next: usize,
    },
    StartQuest {
        quest: String,
        next: usize,
    },
    GiveItem {
        item: String,
        count: u32,
        next: usize,
    },
    Jump(usize),
    End(String),
}

/// A compiled dialogue.
#[derive(Clone, Debug)]
pub struct Dialogue {
    steps: Vec<Step>,
    ids: Vec<String>,
    first: usize,
    variables: BTreeMap<String, (PortType, Option<Value>)>,
    quests: BTreeSet<String>,
    items: BTreeSet<String>,
}

impl Dialogue {
    pub fn node_count(&self) -> usize {
        self.steps.len()
    }
    /// Quest ids the dialogue starts or tests.
    pub fn quests(&self) -> &BTreeSet<String> {
        &self.quests
    }
    /// Item ids the dialogue gives or tests.
    pub fn items(&self) -> &BTreeSet<String> {
        &self.items
    }
    pub fn variable_names(&self) -> impl Iterator<Item = &str> {
        self.variables.keys().map(String::as_str)
    }
}

/// Validates `document` completely, then compiles it.
///
/// # Errors
/// Every graph and dialogue diagnostic.
pub fn compile(document: &GraphDocument) -> Result<Dialogue, Vec<GraphDiagnostic>> {
    let diagnostics = validate(document, shared_kind(), Strictness::Complete);
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    build(document)
}

fn check_document(document: &GraphDocument) -> Vec<GraphDiagnostic> {
    build(document).err().unwrap_or_default()
}

fn text_ok(text: &str) -> bool {
    !text.is_empty() && text.len() <= 128 && !text.chars().any(char::is_control)
}

fn build(doc: &GraphDocument) -> Result<Dialogue, Vec<GraphDiagnostic>> {
    let index = GraphIndex::new(doc);
    let registry = &shared_kind().registry;
    let mut order: Vec<&str> = Vec::new();
    let mut slot: BTreeMap<&str, usize> = BTreeMap::new();
    // Depth-first from the root so ids and steps are in a stable order.
    let Some(root) = doc.nodes.iter().find(|n| n.node_type == node::START) else {
        return Err(vec![GraphDiagnostic::new(
            codes::ROOT_MISSING,
            "",
            "no start node",
        )]);
    };
    let mut stack = vec![root.id.as_str()];
    while let Some(id) = stack.pop() {
        if slot.contains_key(id) {
            continue;
        }
        slot.insert(id, order.len());
        order.push(id);
        if index.node(id).is_none() {
            continue;
        }
        let mut children: Vec<&str> = Vec::new();
        for port in ["next", "options"] {
            children.extend(index.targets(id, port));
        }
        stack.extend(children.into_iter().rev());
    }
    let mut diagnostics = Vec::new();
    let mut error = |code: &str, id: &str, message: String| {
        diagnostics.push(GraphDiagnostic::new(code, format!("/nodes/{id}"), message));
    };
    let variables: BTreeMap<String, (PortType, Option<Value>)> = doc
        .variables
        .iter()
        .map(|v| (v.name.clone(), (v.ty.clone(), v.default.clone())))
        .collect();
    let mut steps = Vec::new();
    let mut quests = BTreeSet::new();
    let mut items = BTreeSet::new();
    for &id in &order {
        let node = index.node(id).expect("ordered ids exist");
        let Some(def) = registry.get(&node.node_type) else {
            continue;
        };
        let params = def.effective_params(node);
        let text = |name: &str| params.get(name).and_then(Value::as_str).unwrap_or_default();
        let next_of = |port: &str| {
            index
                .targets(id, port)
                .first()
                .and_then(|t| slot.get(t).copied())
                .unwrap_or(0)
        };
        let step = match node.node_type.as_str() {
            node::START => Step::Jump(next_of("next")),
            node::LINE => {
                if !text_ok(text("text")) {
                    error(
                        dlg_codes::TEXT,
                        id,
                        "text must be a non-empty key of at most 128 characters".into(),
                    );
                }
                Step::Line {
                    speaker: text("speaker").to_owned(),
                    text: text("text").to_owned(),
                    next: next_of("next"),
                }
            }
            node::CHOICE => {
                let options: Vec<usize> = index
                    .targets(id, "options")
                    .into_iter()
                    .filter_map(|t| slot.get(t).copied())
                    .collect();
                if options.is_empty() {
                    error(
                        dlg_codes::CHOICE_EMPTY,
                        id,
                        "a choice needs at least one option".into(),
                    );
                }
                if options.len() > MAX_OPTIONS {
                    error(
                        dlg_codes::CHOICE_LIMIT,
                        id,
                        format!("more than {MAX_OPTIONS} options"),
                    );
                }
                let prompt = text("prompt");
                if !prompt.is_empty() && !text_ok(prompt) {
                    error(dlg_codes::TEXT, id, "prompt must be a valid key".into());
                }
                Step::Choice {
                    prompt: (!prompt.is_empty()).then(|| prompt.to_owned()),
                    options,
                }
            }
            node::OPTION => {
                if !text_ok(text("text")) {
                    error(
                        dlg_codes::TEXT,
                        id,
                        "text must be a non-empty key of at most 128 characters".into(),
                    );
                }
                let mut conditions = Vec::new();
                match params.get("conditions") {
                    None | Some(Value::Null) => {}
                    Some(Value::Array(list)) if list.len() <= 8 => {
                        for raw in list {
                            match serde_json::from_value::<Condition>(raw.clone()) {
                                Ok(condition) => {
                                    match &condition {
                                        Condition::Variable {
                                            key,
                                            operator,
                                            value,
                                        } => {
                                            check_variable(
                                                &variables,
                                                key,
                                                *operator,
                                                value.as_ref(),
                                            )
                                            .unwrap_or_else(|m| error(dlg_codes::CONDITION, id, m));
                                        }
                                        Condition::Quest { quest, .. } => {
                                            quests.insert(quest.clone());
                                        }
                                        Condition::Item { item, count } => {
                                            if *count == 0 || !text_ok(item) {
                                                error(dlg_codes::CONDITION, id, "item conditions need an item and a count of at least 1".into());
                                            }
                                            items.insert(item.clone());
                                        }
                                    }
                                    conditions.push(condition);
                                }
                                Err(e) => error(dlg_codes::CONDITION, id, e.to_string()),
                            }
                        }
                    }
                    Some(_) => error(
                        dlg_codes::CONDITION,
                        id,
                        "conditions must be an array of at most 8 objects".into(),
                    ),
                }
                Step::Option {
                    text: text("text").to_owned(),
                    conditions,
                    next: next_of("next"),
                }
            }
            node::SET_VARIABLE => {
                let key = text("key").to_owned();
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                match variables.get(&key) {
                    Some((ty, _)) if value_conforms(ty, &value) => {}
                    _ => error(
                        dlg_codes::VALUE_TYPE,
                        id,
                        format!("value does not fit variable {key:?}"),
                    ),
                }
                Step::SetVariable {
                    key,
                    value,
                    next: next_of("next"),
                }
            }
            node::EMIT_EVENT => {
                let event = text("event");
                if !text_ok(event) {
                    error(
                        dlg_codes::EVENT_NAME,
                        id,
                        "event names are 1-128 characters".into(),
                    );
                }
                let payload = params.get("payload").cloned().unwrap_or(Value::Null);
                if !(payload.is_null() || payload.is_object()) {
                    error(
                        dlg_codes::EVENT_PAYLOAD,
                        id,
                        "payload must be a JSON object".into(),
                    );
                }
                Step::Emit {
                    event: event.to_owned(),
                    payload,
                    next: next_of("next"),
                }
            }
            node::START_QUEST => {
                quests.insert(text("quest").to_owned());
                Step::StartQuest {
                    quest: text("quest").to_owned(),
                    next: next_of("next"),
                }
            }
            node::GIVE_ITEM => {
                items.insert(text("item").to_owned());
                Step::GiveItem {
                    item: text("item").to_owned(),
                    count: params.get("count").and_then(Value::as_u64).unwrap_or(1) as u32,
                    next: next_of("next"),
                }
            }
            node::JUMP => {
                let target = text("target");
                let ok = index
                    .node(target)
                    .is_some_and(|t| !matches!(t.node_type.as_str(), node::START | node::OPTION));
                if !ok || !slot.contains_key(target) {
                    error(
                        dlg_codes::JUMP_TARGET,
                        id,
                        format!("{target:?} is not a line, choice or action node of this dialogue"),
                    );
                }
                Step::Jump(slot.get(target).copied().unwrap_or(0))
            }
            _ => Step::End(text("reason").to_owned()),
        };
        steps.push(step);
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    let first = match steps.first() {
        Some(Step::Jump(next)) => *next,
        _ => 0,
    };
    Ok(Dialogue {
        steps,
        ids: order.iter().map(|s| (*s).to_owned()).collect(),
        first,
        variables,
        quests,
        items,
    })
}

fn check_variable(
    variables: &BTreeMap<String, (PortType, Option<Value>)>,
    key: &str,
    operator: Compare,
    value: Option<&Value>,
) -> Result<(), String> {
    let (ty, _) = variables
        .get(key)
        .ok_or_else(|| format!("unknown variable {key:?}"))?;
    if matches!(operator, Compare::IsSet | Compare::IsNotSet) {
        return Ok(());
    }
    let value = value.ok_or("this operator compares against a value")?;
    if !value_conforms(ty, value) {
        return Err(format!("value does not fit variable {key:?}"));
    }
    let ordering = matches!(
        operator,
        Compare::Lt | Compare::Le | Compare::Gt | Compare::Ge
    );
    if ordering && !matches!(ty, PortType::Int | PortType::Float) {
        return Err("ordering needs an int or float variable".into());
    }
    Ok(())
}

/// What a conversation needs from the game.
pub trait DialogueHost {
    /// State of a quest of the conversation owner.
    fn quest_state(&mut self, quest: &str) -> QuestState;
    /// Whether the owner holds `count` of `item`.
    fn has_item(&mut self, item: &str, count: u32) -> bool;
    fn emit_event(&mut self, name: &str, payload: &Value);
    fn start_quest(&mut self, quest: &str);
    fn give_item(&mut self, item: &str, count: u32);
}

/// An option as offered.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OfferedOption {
    /// Index to pass to [`Conversation::select`].
    pub index: usize,
    /// Localisation key.
    pub text: String,
}

/// What a conversation presents next.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Output {
    Line {
        speaker: String,
        text: String,
        node: String,
    },
    Choices {
        prompt: Option<String>,
        options: Vec<OfferedOption>,
        node: String,
    },
    Ended {
        reason: String,
    },
}

/// Why a call was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogueError {
    /// The conversation waits for something else (or is over).
    WrongState,
    BadChoice,
}

impl DialogueError {
    pub fn code(self) -> &'static str {
        match self {
            Self::WrongState => "dialogue.wrong_state",
            Self::BadChoice => "dialogue.bad_choice",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Wait {
    /// Not started.
    Idle,
    /// A line was shown; continue at the node.
    Line(usize),
    /// Choices were offered: option step indices in offering order.
    Choice(Vec<usize>),
    Over,
}

/// One running conversation.
#[derive(Clone, Debug)]
pub struct Conversation {
    dialogue: Arc<Dialogue>,
    variables: BTreeMap<String, Value>,
    wait: Wait,
}

impl Conversation {
    pub fn new(dialogue: Arc<Dialogue>) -> Self {
        let variables = dialogue
            .variables
            .iter()
            .filter_map(|(k, (_, d))| d.clone().map(|d| (k.clone(), d)))
            .collect();
        Self {
            dialogue,
            variables,
            wait: Wait::Idle,
        }
    }
    pub fn is_over(&self) -> bool {
        self.wait == Wait::Over
    }
    pub fn variables(&self) -> &BTreeMap<String, Value> {
        &self.variables
    }
    /// The current wait as text: `idle`, `line`, `choice` or `over`.
    pub fn waiting_for(&self) -> &'static str {
        match self.wait {
            Wait::Idle => "idle",
            Wait::Line(_) => "line",
            Wait::Choice(_) => "choice",
            Wait::Over => "over",
        }
    }

    /// Starts a new conversation (only valid before the first output).
    ///
    /// # Errors
    /// [`DialogueError::WrongState`] when already started.
    pub fn begin(&mut self, host: &mut dyn DialogueHost) -> Result<Output, DialogueError> {
        if self.wait != Wait::Idle {
            return Err(DialogueError::WrongState);
        }
        Ok(self.run(self.dialogue.first, host))
    }

    /// Continues after a line.
    ///
    /// # Errors
    /// [`DialogueError::WrongState`] unless a line is showing.
    pub fn advance(&mut self, host: &mut dyn DialogueHost) -> Result<Output, DialogueError> {
        match self.wait {
            Wait::Line(next) => Ok(self.run(next, host)),
            _ => Err(DialogueError::WrongState),
        }
    }

    /// Picks an offered option by its `index`.
    ///
    /// # Errors
    /// [`DialogueError::WrongState`] unless choices are offered, or
    /// [`DialogueError::BadChoice`] for an index that was not offered.
    pub fn select(
        &mut self,
        index: usize,
        host: &mut dyn DialogueHost,
    ) -> Result<Output, DialogueError> {
        let Wait::Choice(offered) = &self.wait else {
            return Err(DialogueError::WrongState);
        };
        let step = *offered.get(index).ok_or(DialogueError::BadChoice)?;
        let Step::Option { next, .. } = self.dialogue.steps[step] else {
            return Err(DialogueError::BadChoice);
        };
        Ok(self.run(next, host))
    }

    fn holds(&self, condition: &Condition, host: &mut dyn DialogueHost) -> bool {
        match condition {
            Condition::Quest { quest, state } => {
                host.quest_state(quest) == QuestState::from(*state)
            }
            Condition::Item { item, count } => host.has_item(item, *count),
            Condition::Variable {
                key,
                operator,
                value,
            } => {
                let current = self.variables.get(key);
                match operator {
                    Compare::IsSet => current.is_some(),
                    Compare::IsNotSet => current.is_none(),
                    op => {
                        let (Some(left), Some(right)) = (current, value.as_ref()) else {
                            return false;
                        };
                        match (left.as_f64(), right.as_f64()) {
                            (Some(a), Some(b)) => match op {
                                Compare::Eq => a == b,
                                Compare::Ne => a != b,
                                Compare::Lt => a < b,
                                Compare::Le => a <= b,
                                Compare::Gt => a > b,
                                _ => a >= b,
                            },
                            _ => match op {
                                Compare::Eq => left == right,
                                Compare::Ne => left != right,
                                _ => false,
                            },
                        }
                    }
                }
            }
        }
    }

    fn run(&mut self, mut at: usize, host: &mut dyn DialogueHost) -> Output {
        let dialogue = Arc::clone(&self.dialogue);
        for _ in 0..MAX_STEPS {
            match &dialogue.steps[at] {
                Step::Line {
                    speaker,
                    text,
                    next,
                } => {
                    self.wait = Wait::Line(*next);
                    return Output::Line {
                        speaker: speaker.clone(),
                        text: text.clone(),
                        node: dialogue.ids[at].clone(),
                    };
                }
                Step::Choice { prompt, options } => {
                    let mut offered = Vec::new();
                    let mut views = Vec::new();
                    for &option in options {
                        let Step::Option {
                            text, conditions, ..
                        } = &dialogue.steps[option]
                        else {
                            continue;
                        };
                        if conditions.iter().all(|c| self.holds(c, host)) {
                            views.push(OfferedOption {
                                index: offered.len(),
                                text: text.clone(),
                            });
                            offered.push(option);
                        }
                    }
                    if offered.is_empty() {
                        self.wait = Wait::Over;
                        return Output::Ended {
                            reason: "no_choices".into(),
                        };
                    }
                    self.wait = Wait::Choice(offered);
                    return Output::Choices {
                        prompt: prompt.clone(),
                        options: views,
                        node: dialogue.ids[at].clone(),
                    };
                }
                Step::SetVariable { key, value, next } => {
                    self.variables.insert(key.clone(), value.clone());
                    at = *next;
                }
                Step::Emit {
                    event,
                    payload,
                    next,
                } => {
                    host.emit_event(event, payload);
                    at = *next;
                }
                Step::StartQuest { quest, next } => {
                    host.start_quest(quest);
                    at = *next;
                }
                Step::GiveItem { item, count, next } => {
                    host.give_item(item, *count);
                    at = *next;
                }
                Step::Jump(target) => at = *target,
                Step::Option { next, .. } => at = *next,
                Step::End(reason) => {
                    self.wait = Wait::Over;
                    return Output::Ended {
                        reason: if reason.is_empty() {
                            "end".into()
                        } else {
                            reason.clone()
                        },
                    };
                }
            }
        }
        self.wait = Wait::Over;
        Output::Ended {
            reason: "loop".into(),
        }
    }
}
