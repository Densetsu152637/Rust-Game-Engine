//! Authoritative entity store: encoded property state, owners, spatial index, `NetId` allocation.
use crate::NetId;
use crate::error::{ReplicationError, Result};
use crate::grid::{Cell, SpatialGrid};
use crate::registry::ComponentId;
use engine_network::PeerId;
use std::collections::{BTreeSet, VecDeque};

/// Free `NetId` indices are reused only after this many others were freed (or when the index
/// space is exhausted), delaying aliasing; the generation changes on every reuse regardless.
const REUSE_DELAY: usize = 64;

/// How an entity decides which peers it is relevant to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EntityRelevance {
    /// Relevant to every peer, regardless of position.
    #[default]
    Always,
    /// Relevant to peers whose view sphere contains the entity position.
    Spatial,
    /// Relevant to the owning peer only.
    OwnerOnly,
}

pub(crate) struct PropState {
    /// Encoded (quantized) value: the server's authoritative representation and the wire bytes.
    pub bytes: Vec<u8>,
    /// Starts at 1; bumped whenever `bytes` changes.
    pub ver: u32,
}

pub(crate) struct EntityComp {
    pub id: ComponentId,
    pub start: usize,
    pub len: usize,
}

pub(crate) struct Entity {
    pub id: NetId,
    pub owner: Option<PeerId>,
    pub mode: EntityRelevance,
    pub position: Option<[f32; 3]>,
    pub tag: u32,
    pub comps: Vec<EntityComp>,
    pub props: Vec<PropState>,
    pub cell: Option<Cell>,
    /// True when any component's visibility depends on ownership.
    pub owner_conditioned: bool,
    /// Encoded component bytes an owner / a non-owner receives in a spawn block.
    pub size_owner: usize,
    pub size_other: usize,
}

impl Entity {
    pub(crate) fn spawn_block_estimate(&self, owned: bool) -> usize {
        6 + self.id.wire_len()
            + if owned {
                self.size_owner
            } else {
                self.size_other
            }
    }
}

struct Slot {
    generation: u16,
    entity: Option<Entity>,
}

pub(crate) struct World {
    slots: Vec<Slot>,
    free: VecDeque<u32>,
    live: usize,
    max_entities: usize,
    pub grid: SpatialGrid,
    pub always: BTreeSet<u32>,
    pub owner_only: BTreeSet<u32>,
}

impl World {
    pub(crate) fn new(max_entities: usize, cell_size: f32) -> Self {
        Self {
            slots: Vec::new(),
            free: VecDeque::new(),
            live: 0,
            max_entities,
            grid: SpatialGrid::new(cell_size),
            always: BTreeSet::new(),
            owner_only: BTreeSet::new(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.live
    }

    pub(crate) fn entity(&self, id: NetId) -> Option<&Entity> {
        let slot = self.slots.get(id.index() as usize)?;
        (slot.generation == id.generation())
            .then_some(slot.entity.as_ref())
            .flatten()
    }

    pub(crate) fn entity_mut(&mut self, id: NetId) -> Option<&mut Entity> {
        let slot = self.slots.get_mut(id.index() as usize)?;
        (slot.generation == id.generation())
            .then_some(slot.entity.as_mut())
            .flatten()
    }

    pub(crate) fn by_index(&self, index: u32) -> Option<&Entity> {
        self.slots.get(index as usize)?.entity.as_ref()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Entity> {
        self.slots.iter().filter_map(|s| s.entity.as_ref())
    }

    /// Reserves the next `NetId`: a fresh index while the space lasts and the free list is
    /// short, otherwise the oldest freed index.
    pub(crate) fn allocate(&mut self) -> Result<NetId> {
        if self.live >= self.max_entities {
            return Err(ReplicationError::Limit("max_entities reached"));
        }
        let reuse = self.free.len() >= REUSE_DELAY || self.slots.len() >= self.max_entities;
        if reuse && let Some(index) = self.free.pop_front() {
            return Ok(NetId::new(index, self.slots[index as usize].generation));
        }
        if self.slots.len() >= self.max_entities {
            return Err(ReplicationError::Limit("no free entity index"));
        }
        let index = self.slots.len() as u32;
        self.slots.push(Slot {
            generation: 0,
            entity: None,
        });
        Ok(NetId::new(index, 0))
    }

    pub(crate) fn insert(&mut self, entity: Entity) {
        let index = entity.id.index();
        match entity.mode {
            EntityRelevance::Always => {
                self.always.insert(index);
            }
            EntityRelevance::OwnerOnly => {
                self.owner_only.insert(index);
            }
            EntityRelevance::Spatial => {}
        }
        let mut entity = entity;
        if entity.mode == EntityRelevance::Spatial
            && let Some(p) = entity.position
        {
            let cell = self.grid.cell_of(p);
            self.grid.insert(index, cell);
            entity.cell = Some(cell);
        }
        self.slots[index as usize].entity = Some(entity);
        self.live += 1;
    }

    pub(crate) fn remove(&mut self, id: NetId) -> Option<Entity> {
        let index = id.index() as usize;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != id.generation() {
            return None;
        }
        let entity = slot.entity.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push_back(id.index());
        self.live -= 1;
        self.always.remove(&id.index());
        self.owner_only.remove(&id.index());
        if let Some(cell) = entity.cell {
            self.grid.remove(id.index(), cell);
        }
        Some(entity)
    }

    pub(crate) fn move_to(&mut self, id: NetId, position: [f32; 3]) -> Result<()> {
        let new_cell = self.grid.cell_of(position);
        let entity = self
            .entity_mut(id)
            .ok_or(ReplicationError::UnknownEntity(id))?;
        entity.position = Some(position);
        if entity.mode != EntityRelevance::Spatial {
            return Ok(());
        }
        let old = entity.cell.replace(new_cell);
        if old != Some(new_cell) {
            if let Some(old) = old {
                self.grid.remove(id.index(), old);
            }
            self.grid.insert(id.index(), new_cell);
        }
        Ok(())
    }
}
