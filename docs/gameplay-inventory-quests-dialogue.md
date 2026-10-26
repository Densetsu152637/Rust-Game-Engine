# Inventory, quests and dialogue

Three data-driven gameplay frameworks next to the [ability system](gameplay-abilities.md).
Pure logic lives in `engine-gameplay` (`inventory`, `quest`, `dialogue`: no ECS, deterministic);
`engine-project` wraps it as components, a service, events, native functions and telemetry
(`crate::gameplay::progression`).

**Definitions** reuse the `gameplay` asset kind: the same JSON document that holds attribute sets,
effects and abilities gains two optional lists, `items` and `quests`, validated with the rest
(`gameplay.definitions.invalid`). Dialogues are `graph` assets of the new kind `dialogue`. No new
asset kind and no new MCP tool: everything is authored with `graph_schema` / `graph_validate` /
`graph_edit` and `component_set`.

```json
{"version": 1,
 "effects": [{"name": "Might", "policy": "infinite",
              "modifiers": [{"attribute": "Strength", "magnitude": {"scalable": {"value": 5}}}]}],
 "items": [
   {"id": "potion", "stack": 5, "weight": 0.5, "tags": ["consumable"]},
   {"id": "sword", "stack": 1, "weight": 4, "tags": ["weapon"], "equip_effect": "Might",
    "properties": {"damage": 12}}],
 "quests": [{"id": "wolves", "prerequisites": ["intro"],
   "stages": [
     {"id": "hunt", "objectives": [{"type": "counter", "id": "kills", "event": "enemy.killed",
                                    "filter": {"kind": "wolf"}, "target": 3}]},
     {"id": "collect", "objectives": [{"type": "has_item", "id": "pelts", "item": "pelt", "count": 2}]}],
   "rewards": {"items": [{"item": "coin", "count": 10}], "effect": "Blessing",
               "event": {"name": "quest.reward", "payload": {"big": true}}},
   "fail_on": [{"event": "player.died"}]}]}
```

## Inventory

**Items** (`items[]`): `id`, `stack` (most units per slot, default 1), `weight` per unit, `tags`,
free-form `properties` (at most 16 small values), and `equip_effect` (an ability-system effect,
applied while the item is worn).

**Containers** have `slots` (1-256) and an optional `max_weight` (0 = unlimited). Every operation is
**atomic**: it succeeds completely or fails with an error code and changes nothing.

| Operation | Rule |
| --- | --- |
| `add` | Fills existing stacks of the item in ascending slot order, then empty slots in ascending order. Weight is checked first (`inventory.overweight`), then room (`inventory.full`). |
| `remove` | Takes from the highest slot first; `inventory.not_enough` otherwise. |
| `move_stack` | To an empty slot moves; onto the same item merges up to the stack size; a *full* move onto another item swaps; a partial one is `inventory.mismatch`. |
| `split` | Moves `count` (less than the stack) into the first empty slot. |
| `transfer` | Between two containers; the destination's `add` and the source's `remove` are both checked before anything changes. |
| equipment | `engine.Inventory.equipment` lists slots `{name, tag}`; an item fits when it carries the slot's tag (`inventory.not_equippable`). Equipping takes one unit out of the container; unequipping returns it (a full container keeps the item worn). |

Error codes: `inventory.unknown_item`, `.invalid_count`, `.full`, `.overweight`, `.not_enough`,
`.bad_slot`, `.empty_slot`, `.mismatch`, `.not_equippable`, `.slot_occupied`.

### `engine.Inventory`

```json
{"definitions": "defs", "capacity": {"slots": 16, "max_weight": 40},
 "items": [{"item": "potion", "count": 3}],
 "equipment": [{"name": "main_hand", "tag": "weapon"}]}
```

Project validation loads the definitions, checks the capacity and that the initial items fit
(`progression.component.invalid`). Equipping an item with an `equip_effect` applies the effect to the
entity when an `engine.AbilitySystem` runs and removes it again on unequip (or when the component is
replaced or the runtime stops); without an ability system the equipment still works.

Natives (`engine_project::gameplay::progression`): `add_item`, `remove_item`, `move_item`,
`split_item`, `transfer_item`, `equip_item`, `unequip_item`, `item_count`, `slot_item`,
`equipped_item`. Errors are `ProgressionError` with a stable `code()`.

Events (target: the inventory's entity): `inventory.changed {item, delta, count}`,
`inventory.full {item, count, reason}` (an add or transfer refused for room or weight),
`inventory.equipped` / `inventory.unequipped {slot, item}`.

## Quests

A quest is `inactive`, `active`, `completed` or `failed`. `prerequisites` are quests that must be
completed first (`quest.prerequisite`; cycles are rejected). Stages run in order; a stage advances when
all its objectives are done, and completing the last stage completes the quest.

| Objective `type` | Done when |
| --- | --- |
| `counter` | `target` events named `event` arrived whose payload matches `filter` (every dotted path equals its value; numbers compare as numbers). |
| `flag` | the log's flag is set (`set_quest_flag`, or `flags` on the component). |
| `has_item` | the owner's inventory holds `count` of `item` (re-checked whenever the inventory changes). |

**Rewards** (`items`, an ability `effect`, an engine `event`) are returned by the single state change
to completed, so they are delivered **exactly once**; reward items that do not fit are dropped and
counted (`dropped_rewards`). `fail_on` lists events (with filters) that fail an active quest; a failed
quest can be started again with fresh progress. `abandon` returns an active quest to inactive.

`engine.QuestLog`: `{definitions, start: [quest ids started at creation], flags: [...]}`. Counters are
fed by engine events delivered to the log's entity (or untargeted ones) whose names some objective or
`fail_on` listens for. Natives: `start_quest`, `abandon_quest`, `fail_quest`, `set_quest_flag`,
`quest_state`, `quest_objectives`.

Events (target: the log's entity): `quest.started {quest}`, `quest.objective_progressed {quest,
objective, value, target}`, `quest.stage_advanced {quest, stage}`, `quest.completed {quest}`,
`quest.failed {quest}`, plus the reward event.

## Dialogue

A `dialogue` graph (single root `start`, no cycles; node `jump` continues at another node by id):

| Node | Params | Meaning |
| --- | --- | --- |
| `start` | | Root; its `next` is the first node. |
| `line` | `speaker`, `text` (a localisation **key**) | Shown; the conversation waits for `advance`. |
| `choice` | `prompt` (key, optional) | Offers its `options`. |
| `option` | `text` (key), `conditions` | One choice (connected from the choice's `options` port); hidden when a condition fails. |
| `set_variable` | `key`, `value` | Writes a dialogue variable (graph `variables`: bool, int, float, string). |
| `emit_event` | `event`, `payload` | Emits an engine event at the player. |
| `start_quest` | `quest` | Starts the player's quest (ignored when it cannot start). |
| `give_item` | `item`, `count` | Adds to the player's inventory (skipped when it does not fit). |
| `jump` | `target` | Continues at that node (a line, choice or action). |
| `end` | `reason` | Ends the conversation. |

Every `next` port must be connected. **Conditions** are an array (all must hold) of
`{"type": "variable", "key", "operator", "value"}` (`is_set`, `is_not_set`, `eq`, `ne`, `lt`, `le`,
`gt`, `ge`), `{"type": "quest", "quest", "state"}` and `{"type": "item", "item", "count"}`. Offered
choices are the options whose conditions hold, in edge order, numbered from 0 in the order offered; if
none is offered the conversation ends with reason `no_choices`. A run of more than 256 nodes without
a line or choice ends with `loop`.

Compiler diagnostics (besides the generic `graph.*` codes): `dlg.choice.empty`, `dlg.choice.options`
(more than 16), `dlg.jump.target`, `dlg.condition.invalid`, `dlg.value.type`, `dlg.event.name`,
`dlg.event.payload`, `dlg.text.invalid`. Limits: 512 nodes, 64 variables.

### `engine.DialogueParticipant`

`{graph, definitions?}` on the NPC. A conversation runs between the NPC and a **player** entity: the
player's `engine.QuestLog` and `engine.Inventory` are what conditions test and `start_quest` /
`give_item` change. With `definitions` set, project validation checks that every quest and item the
dialogue names exists (`dialogue.reference.unknown`); a graph of another kind is `dialogue.graph.kind`.

Drive it with natives (`start_dialogue(world, npc, player)`, `advance_dialogue`, `select_choice`,
`end_dialogue`, each returning the next `Output`) or with engine events delivered to the NPC:
`dialogue.start` (source: the player), `dialogue.advance`, `dialogue.select {index}`. One conversation
runs per NPC at a time; starting another replaces it.

Events (target: the player, source: the NPC; payloads carry `npc` and `player` authored ids):
`dialogue.line {speaker, text, node}`, `dialogue.choices {prompt, options: [{index, text}], node}`,
`dialogue.ended {reason}`. Text values are keys for the game's localisation.

## Service, bounds and telemetry

The service steps once per tick before the ability service and the trees, so trees and abilities see
this tick's inventory and quest changes. It emits at most 256 events per tick (the rest, and events
the full queue refuses, are counted in `dropped_events`). Limits: 1024 items, 256 quests of up to 16
stages and 16 objectives, 256 slots per container, 16 equipment slots, 16 dialogue options per choice.

`RuntimeSnapshot.progression` (and the `progression` member of the `project_run_headless` summary) is
absent without these components:

```json
{"inventories": [{"entity": "hero", "slots": 16, "used": 3, "weight": 6.5,
                  "items": {"potion": 3}, "equipped": {"main_hand": "sword"}}],
 "quest_logs": [{"entity": "hero", "flags": [], "quests": [
    {"quest": "wolves", "state": "active", "stage": "hunt", "objectives": [["kills", 1, 3]]}]}],
 "dialogues": [{"npc": "elder", "player": "hero", "waiting": "choice"}],
 "dropped_events": 0, "dropped_rewards": 0}
```

At most 16 entities per kind and 16 items or quests each.

## Determinism

Containers, quest logs and conversations are pure functions of their inputs: iteration is ordered by
slot, quest id, objective id and entity, and nothing reads a clock or a random number generator. Two
runs of the same headless project give identical events and telemetry (tested).

Focused validation: `cargo test -p engine-gameplay --test progression`,
`cargo test -p engine-project --test progression`, `cargo test -p engine-mcp --test progression`.
