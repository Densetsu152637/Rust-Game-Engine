//! Item definitions, slot containers and equipment.
//!
//! A [`Container`] has a fixed number of slots, each empty or holding a [`Stack`], and an
//! optional weight limit. Every operation is **atomic**: it either succeeds completely or
//! returns an [`InventoryError`] and leaves every container exactly as it was. Order is
//! deterministic: adding fills existing stacks of the item in ascending slot order, then
//! empty slots in ascending order; removing takes from the highest slot first.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Most item definitions one document may hold.
pub const MAX_ITEMS: usize = 1024;
/// Most slots one container may have.
pub const MAX_SLOTS: u32 = 256;
/// Largest stack size an item may declare.
pub const MAX_STACK: u32 = 100_000;
/// Most custom properties one item may carry.
pub const MAX_PROPERTIES: usize = 16;
/// Most equipment slots one equipment set may have.
pub const MAX_EQUIPMENT_SLOTS: usize = 16;

/// One item type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ItemDef {
    pub id: String,
    /// Most units one stack holds.
    pub stack: u32,
    /// Weight of one unit.
    pub weight: f32,
    pub tags: Vec<String>,
    /// Free-form data for the game (damage, description key, ...).
    pub properties: BTreeMap<String, Value>,
    /// Ability-system effect applied while the item is equipped.
    pub equip_effect: Option<String>,
}

impl Default for ItemDef {
    fn default() -> Self {
        Self {
            id: String::new(),
            stack: 1,
            weight: 0.0,
            tags: Vec::new(),
            properties: BTreeMap::new(),
            equip_effect: None,
        }
    }
}

fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// Checks a list of item definitions.
///
/// # Errors
/// The first problem, naming the item.
pub fn validate_items(items: &[ItemDef]) -> Result<(), String> {
    if items.len() > MAX_ITEMS {
        return Err(format!("more than {MAX_ITEMS} items"));
    }
    let mut ids = BTreeSet::new();
    for item in items {
        if !name_ok(&item.id) || !ids.insert(item.id.as_str()) {
            return Err(format!("item id {:?} is invalid or repeated", item.id));
        }
        if item.stack == 0 || item.stack > MAX_STACK {
            return Err(format!("item {}: stack must be 1..={MAX_STACK}", item.id));
        }
        if !item.weight.is_finite() || !(0.0..=1.0e6).contains(&item.weight) {
            return Err(format!("item {}: weight must be 0..=1e6", item.id));
        }
        if item.tags.len() > 16 || !item.tags.iter().all(|t| name_ok(t)) {
            return Err(format!("item {}: at most 16 valid tags", item.id));
        }
        if item.properties.len() > MAX_PROPERTIES
            || item.properties.keys().any(|k| !name_ok(k))
            || item.properties.values().any(|v| v.to_string().len() > 1024)
        {
            return Err(format!(
                "item {}: at most {MAX_PROPERTIES} small properties with valid names",
                item.id
            ));
        }
        if item.equip_effect.as_ref().is_some_and(|e| !name_ok(e)) {
            return Err(format!("item {}: invalid equip_effect", item.id));
        }
    }
    Ok(())
}

/// Item definitions indexed by id.
#[derive(Clone, Debug, Default)]
pub struct ItemTable {
    items: BTreeMap<String, ItemDef>,
}

impl ItemTable {
    /// # Errors
    /// Invalid definitions (see [`validate_items`]).
    pub fn new(items: &[ItemDef]) -> Result<Self, String> {
        validate_items(items)?;
        Ok(Self {
            items: items.iter().map(|i| (i.id.clone(), i.clone())).collect(),
        })
    }
    pub fn get(&self, id: &str) -> Option<&ItemDef> {
        self.items.get(id)
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Why an operation was refused. Nothing changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventoryError {
    UnknownItem,
    /// A count of 0 or a split that leaves nothing.
    InvalidCount,
    /// Not enough slots (or stack room) for everything.
    Full,
    /// The container's weight limit would be exceeded.
    Overweight,
    /// Fewer units than asked for.
    NotEnough,
    BadSlot,
    EmptySlot,
    /// The destination holds a different item and the move is partial.
    Mismatch,
    /// The item does not fit the equipment slot's tag.
    NotEquippable,
    SlotOccupied,
}

impl InventoryError {
    /// Stable code for events and diagnostics.
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownItem => "inventory.unknown_item",
            Self::InvalidCount => "inventory.invalid_count",
            Self::Full => "inventory.full",
            Self::Overweight => "inventory.overweight",
            Self::NotEnough => "inventory.not_enough",
            Self::BadSlot => "inventory.bad_slot",
            Self::EmptySlot => "inventory.empty_slot",
            Self::Mismatch => "inventory.mismatch",
            Self::NotEquippable => "inventory.not_equippable",
            Self::SlotOccupied => "inventory.slot_occupied",
        }
    }
}

impl std::fmt::Display for InventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for InventoryError {}

/// Units of one item in one slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stack {
    pub item: String,
    pub count: u32,
}

/// Net change of one item caused by an operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delta {
    pub item: String,
    pub delta: i64,
}

/// Size limits of a container.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capacity {
    pub slots: u32,
    /// Largest total weight; 0 means unlimited.
    pub max_weight: f32,
}

impl Default for Capacity {
    fn default() -> Self {
        Self {
            slots: 16,
            max_weight: 0.0,
        }
    }
}

impl Capacity {
    /// # Errors
    /// Out-of-range limits.
    pub fn validate(&self) -> Result<(), String> {
        if self.slots == 0 || self.slots > MAX_SLOTS {
            return Err(format!("slots must be 1..={MAX_SLOTS}"));
        }
        if !self.max_weight.is_finite() || !(0.0..=1.0e9).contains(&self.max_weight) {
            return Err("max_weight must be 0..=1e9".into());
        }
        Ok(())
    }
}

/// A fixed-slot container.
#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    capacity: Capacity,
    slots: Vec<Option<Stack>>,
}

impl Container {
    pub fn new(capacity: Capacity) -> Self {
        Self {
            capacity,
            slots: vec![None; capacity.slots.min(MAX_SLOTS) as usize],
        }
    }
    pub fn capacity(&self) -> Capacity {
        self.capacity
    }
    pub fn slots(&self) -> &[Option<Stack>] {
        &self.slots
    }
    pub fn slot(&self, index: usize) -> Option<&Stack> {
        self.slots.get(index)?.as_ref()
    }
    /// Total units of `item`.
    pub fn count(&self, item: &str) -> u32 {
        self.slots
            .iter()
            .flatten()
            .filter(|s| s.item == item)
            .map(|s| s.count)
            .sum()
    }
    pub fn has(&self, item: &str, count: u32) -> bool {
        self.count(item) >= count
    }
    /// Total weight.
    pub fn weight(&self, table: &ItemTable) -> f32 {
        self.slots
            .iter()
            .flatten()
            .map(|s| table.get(&s.item).map_or(0.0, |d| d.weight) * s.count as f32)
            .sum()
    }
    /// Units per item, ordered by item id.
    pub fn totals(&self) -> BTreeMap<String, u32> {
        let mut totals = BTreeMap::new();
        for stack in self.slots.iter().flatten() {
            *totals.entry(stack.item.clone()).or_default() += stack.count;
        }
        totals
    }

    fn fits_weight(&self, table: &ItemTable, extra: f32) -> bool {
        self.capacity.max_weight <= 0.0
            || self.weight(table) + extra <= self.capacity.max_weight + 1e-4
    }

    /// Adds `count` units: existing stacks first (ascending slot), then empty slots.
    ///
    /// # Errors
    /// [`InventoryError::UnknownItem`], [`InvalidCount`](InventoryError::InvalidCount),
    /// [`Overweight`](InventoryError::Overweight) or [`Full`](InventoryError::Full); the
    /// container is unchanged.
    pub fn add(
        &mut self,
        table: &ItemTable,
        item: &str,
        count: u32,
    ) -> Result<Vec<Delta>, InventoryError> {
        let def = table.get(item).ok_or(InventoryError::UnknownItem)?;
        if count == 0 {
            return Err(InventoryError::InvalidCount);
        }
        if !self.fits_weight(table, def.weight * count as f32) {
            return Err(InventoryError::Overweight);
        }
        let mut plan: Vec<(usize, u32)> = Vec::new();
        let mut left = count;
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(stack) = slot
                && stack.item == item
                && stack.count < def.stack
            {
                let room = (def.stack - stack.count).min(left);
                plan.push((index, room));
                left -= room;
                if left == 0 {
                    break;
                }
            }
        }
        if left > 0 {
            for (index, slot) in self.slots.iter().enumerate() {
                if slot.is_none() {
                    let put = def.stack.min(left);
                    plan.push((index, put));
                    left -= put;
                    if left == 0 {
                        break;
                    }
                }
            }
        }
        if left > 0 {
            return Err(InventoryError::Full);
        }
        for (index, put) in plan {
            match &mut self.slots[index] {
                Some(stack) => stack.count += put,
                slot @ None => {
                    *slot = Some(Stack {
                        item: item.to_owned(),
                        count: put,
                    });
                }
            }
        }
        Ok(vec![Delta {
            item: item.to_owned(),
            delta: i64::from(count),
        }])
    }

    /// Removes `count` units, from the highest slot first.
    ///
    /// # Errors
    /// [`InventoryError::InvalidCount`] or [`NotEnough`](InventoryError::NotEnough).
    pub fn remove(&mut self, item: &str, count: u32) -> Result<Vec<Delta>, InventoryError> {
        if count == 0 {
            return Err(InventoryError::InvalidCount);
        }
        if self.count(item) < count {
            return Err(InventoryError::NotEnough);
        }
        let mut left = count;
        for slot in self.slots.iter_mut().rev() {
            let Some(stack) = slot else {
                continue;
            };
            if stack.item != item {
                continue;
            }
            let take = stack.count.min(left);
            stack.count -= take;
            left -= take;
            if stack.count == 0 {
                *slot = None;
            }
            if left == 0 {
                break;
            }
        }
        Ok(vec![Delta {
            item: item.to_owned(),
            delta: -i64::from(count),
        }])
    }

    /// Moves `count` units (all when `None`) from slot `from` to slot `to` of this
    /// container. Into an empty slot the stack moves; onto the same item it merges up to
    /// the stack size; a full move onto a different item swaps the two stacks.
    ///
    /// # Errors
    /// Bad or empty slots, [`Mismatch`](InventoryError::Mismatch) for a partial move onto
    /// another item, or [`Full`](InventoryError::Full) when nothing fits in the merge.
    pub fn move_stack(
        &mut self,
        table: &ItemTable,
        from: usize,
        to: usize,
        count: Option<u32>,
    ) -> Result<(), InventoryError> {
        if from >= self.slots.len() || to >= self.slots.len() {
            return Err(InventoryError::BadSlot);
        }
        let source = self.slots[from].clone().ok_or(InventoryError::EmptySlot)?;
        let count = count.unwrap_or(source.count);
        if count == 0 || count > source.count {
            return Err(InventoryError::InvalidCount);
        }
        if from == to {
            return Ok(());
        }
        match self.slots[to].clone() {
            None => {
                self.slots[to] = Some(Stack {
                    item: source.item.clone(),
                    count,
                });
                self.shrink(from, count);
            }
            Some(target) if target.item == source.item => {
                let max = table
                    .get(&source.item)
                    .ok_or(InventoryError::UnknownItem)?
                    .stack;
                let room = max.saturating_sub(target.count).min(count);
                if room == 0 {
                    return Err(InventoryError::Full);
                }
                self.slots[to].as_mut().expect("occupied").count += room;
                self.shrink(from, room);
            }
            Some(_) if count == source.count => self.slots.swap(from, to),
            Some(_) => return Err(InventoryError::Mismatch),
        }
        Ok(())
    }

    fn shrink(&mut self, slot: usize, by: u32) {
        let empty = {
            let stack = self.slots[slot].as_mut().expect("occupied");
            stack.count -= by;
            stack.count == 0
        };
        if empty {
            self.slots[slot] = None;
        }
    }

    /// Splits `count` units off slot `from` into the first empty slot; returns it.
    ///
    /// # Errors
    /// Bad or empty slot, a count that is 0 or not smaller than the stack, or no empty slot.
    pub fn split(&mut self, from: usize, count: u32) -> Result<usize, InventoryError> {
        let source = self
            .slots
            .get(from)
            .ok_or(InventoryError::BadSlot)?
            .clone()
            .ok_or(InventoryError::EmptySlot)?;
        if count == 0 || count >= source.count {
            return Err(InventoryError::InvalidCount);
        }
        let to = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(InventoryError::Full)?;
        self.slots[to] = Some(Stack {
            item: source.item,
            count,
        });
        self.shrink(from, count);
        Ok(to)
    }

    /// Replaces the contents (used when an operation was planned on a copy).
    pub fn restore(&mut self, other: Container) {
        *self = other;
    }
}

/// Moves `count` units of `item` from `from` to `to`. Atomic across both containers: when
/// the destination cannot take everything, neither container changes.
///
/// # Errors
/// The error of the failing half ([`NotEnough`](InventoryError::NotEnough) for the source,
/// [`Full`](InventoryError::Full) or [`Overweight`](InventoryError::Overweight) for the
/// destination).
pub fn transfer(
    table: &ItemTable,
    from: &mut Container,
    to: &mut Container,
    item: &str,
    count: u32,
) -> Result<Vec<Delta>, InventoryError> {
    let mut source = from.clone();
    let mut destination = to.clone();
    source.remove(item, count)?;
    destination.add(table, item, count)?;
    *from = source;
    *to = destination;
    Ok(vec![Delta {
        item: item.to_owned(),
        delta: i64::from(count),
    }])
}

/// One equipment slot: only items carrying `tag` fit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentSlotDef {
    pub name: String,
    pub tag: String,
}

/// Worn items by equipment slot name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Equipment {
    defs: Vec<EquipmentSlotDef>,
    worn: BTreeMap<String, String>,
}

impl Equipment {
    /// # Errors
    /// Too many, invalid or repeated slots.
    pub fn new(defs: Vec<EquipmentSlotDef>) -> Result<Self, String> {
        if defs.len() > MAX_EQUIPMENT_SLOTS {
            return Err(format!("more than {MAX_EQUIPMENT_SLOTS} equipment slots"));
        }
        let mut names = BTreeSet::new();
        for def in &defs {
            if !name_ok(&def.name) || !name_ok(&def.tag) || !names.insert(def.name.as_str()) {
                return Err(format!(
                    "equipment slot {:?} is invalid or repeated",
                    def.name
                ));
            }
        }
        Ok(Self {
            defs,
            worn: BTreeMap::new(),
        })
    }
    pub fn worn(&self) -> &BTreeMap<String, String> {
        &self.worn
    }
    pub fn slot_names(&self) -> impl Iterator<Item = &str> {
        self.defs.iter().map(|d| d.name.as_str())
    }

    /// Wears one unit of the item in `container` slot `from` in equipment slot `slot`.
    /// Returns the equipped item id.
    ///
    /// # Errors
    /// [`BadSlot`](InventoryError::BadSlot) for an unknown equipment slot,
    /// [`SlotOccupied`](InventoryError::SlotOccupied), [`EmptySlot`](InventoryError::EmptySlot)
    /// or [`NotEquippable`](InventoryError::NotEquippable) (the item lacks the slot's tag).
    pub fn equip(
        &mut self,
        table: &ItemTable,
        container: &mut Container,
        from: usize,
        slot: &str,
    ) -> Result<String, InventoryError> {
        let def = self
            .defs
            .iter()
            .find(|d| d.name == slot)
            .ok_or(InventoryError::BadSlot)?;
        if self.worn.contains_key(slot) {
            return Err(InventoryError::SlotOccupied);
        }
        let item = container
            .slot(from)
            .ok_or(InventoryError::EmptySlot)?
            .item
            .clone();
        let item_def = table.get(&item).ok_or(InventoryError::UnknownItem)?;
        if !item_def.tags.contains(&def.tag) {
            return Err(InventoryError::NotEquippable);
        }
        container.remove_from_slot(from, 1)?;
        self.worn.insert(slot.to_owned(), item.clone());
        Ok(item)
    }

    /// Returns the worn item of `slot` to the container.
    ///
    /// # Errors
    /// [`EmptySlot`](InventoryError::EmptySlot) when nothing is worn there, or the
    /// container's `add` error (the item stays worn).
    pub fn unequip(
        &mut self,
        table: &ItemTable,
        container: &mut Container,
        slot: &str,
    ) -> Result<String, InventoryError> {
        let item = self
            .worn
            .get(slot)
            .ok_or(InventoryError::EmptySlot)?
            .clone();
        container.add(table, &item, 1)?;
        self.worn.remove(slot);
        Ok(item)
    }
}

impl Container {
    fn remove_from_slot(&mut self, slot: usize, count: u32) -> Result<(), InventoryError> {
        let stack = self
            .slots
            .get(slot)
            .ok_or(InventoryError::BadSlot)?
            .as_ref()
            .ok_or(InventoryError::EmptySlot)?;
        if count == 0 || count > stack.count {
            return Err(InventoryError::InvalidCount);
        }
        self.shrink(slot, count);
        Ok(())
    }
}
