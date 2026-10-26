//! Smart objects: world objects that offer slots (places and ways to use them) which
//! agents claim, occupy and release with deterministic arbitration.
//!
//! A slot is **free**, **claimed** (reserved by one user, expires after the object's
//! `claim_timeout` unless occupied) or **occupied** (in use). All state lives in
//! [`SmartObjects`], keyed by object id; nothing here reads a clock or a random number
//! generator, and every listing is ordered by `(object, slot)`.
use crate::perception::Id;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Most slots one object may define.
pub const MAX_SLOTS: usize = 16;
/// Most tags one slot may carry.
pub const MAX_TAGS: usize = 8;
/// Most objects one registry holds.
pub const MAX_OBJECTS: usize = 1024;
const EPS: f64 = 1e-6;

/// One place to use an object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlotDef {
    pub name: String,
    /// Position relative to the object's transform.
    pub offset: [f32; 3],
    pub tags: Vec<String>,
    /// Id of the behaviour the user runs at the slot (resolved by the game).
    pub behavior: String,
}

impl Default for SlotDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            offset: [0.0; 3],
            tags: Vec::new(),
            behavior: String::new(),
        }
    }
}

/// The slots and claim rules of one object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObjectDef {
    pub slots: Vec<SlotDef>,
    /// Seconds a claim may wait for its occupation before it expires; 0 never expires.
    pub claim_timeout: f32,
}

impl Default for ObjectDef {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            claim_timeout: 10.0,
        }
    }
}

fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

impl ObjectDef {
    /// # Errors
    /// A bound that is violated, with a message naming the slot.
    pub fn validate(&self) -> Result<(), String> {
        if self.slots.len() > MAX_SLOTS {
            return Err(format!("more than {MAX_SLOTS} slots"));
        }
        if !self.claim_timeout.is_finite() || !(0.0..=3600.0).contains(&self.claim_timeout) {
            return Err("claim_timeout must be 0 to 3600 seconds".into());
        }
        let mut names = std::collections::BTreeSet::new();
        for (index, slot) in self.slots.iter().enumerate() {
            if !valid_tag(&slot.name) {
                return Err(format!(
                    "slot {index}: names are 1-64 characters of letters, digits, '_', '-', '.', ':'"
                ));
            }
            if !names.insert(slot.name.as_str()) {
                return Err(format!("slot name {:?} repeats", slot.name));
            }
            if slot
                .offset
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 1.0e6)
            {
                return Err(format!("slot {:?}: offset must be finite", slot.name));
            }
            if slot.tags.len() > MAX_TAGS || !slot.tags.iter().all(|t| valid_tag(t)) {
                return Err(format!(
                    "slot {:?}: at most {MAX_TAGS} tags of letters, digits, '_', '-', '.', ':'",
                    slot.name
                ));
            }
            if slot.behavior.len() > 128 {
                return Err(format!(
                    "slot {:?}: behavior is at most 128 characters",
                    slot.name
                ));
            }
        }
        Ok(())
    }
}

/// Address of one slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct SlotRef {
    pub object: Id,
    pub slot: u32,
}

/// Who holds a slot.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotState {
    Free,
    /// Reserved by `user`; `remaining` seconds until the claim expires (`None`: never).
    Claimed {
        user: Id,
        remaining: Option<f64>,
    },
    Occupied {
        user: Id,
    },
}

impl SlotState {
    pub fn user(&self) -> Option<Id> {
        match self {
            Self::Free => None,
            Self::Claimed { user, .. } | Self::Occupied { user } => Some(*user),
        }
    }
    pub fn is_free(&self) -> bool {
        matches!(self, Self::Free)
    }
}

/// Why a slot was released.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    /// The user finished.
    Completed,
    /// The user gave up (its task was aborted).
    Aborted,
    /// A claim was not occupied in time.
    Timeout,
    /// The user or the object no longer exists.
    Removed,
}

impl ReleaseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::Timeout => "timeout",
            Self::Removed => "removed",
        }
    }
}

/// A change of slot state worth telling the game about.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartEvent {
    Claimed {
        slot: SlotRef,
        user: Id,
    },
    /// The user began using the slot.
    Activated {
        slot: SlotRef,
        user: Id,
    },
    Released {
        slot: SlotRef,
        user: Id,
        reason: ReleaseReason,
    },
}

/// Why a claim or occupation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotError {
    UnknownSlot,
    /// Another user holds the slot.
    Taken(Id),
    /// The slot is not claimed by this user.
    NotClaimed,
}

impl std::fmt::Display for SlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSlot => f.write_str("no such smart object slot"),
            Self::Taken(user) => write!(f, "slot is held by {user}"),
            Self::NotClaimed => f.write_str("slot is not claimed by this user"),
        }
    }
}
impl std::error::Error for SlotError {}

/// A user asking for any slot with all of `tags`.
#[derive(Clone, Debug)]
pub struct ClaimRequest {
    pub user: Id,
    pub tags: Vec<String>,
    /// Where the user is; nearer slots win.
    pub position: [f32; 3],
    /// Higher priorities are served first.
    pub priority: i32,
}

#[derive(Clone, Debug)]
struct Object {
    def: ObjectDef,
    states: Vec<SlotState>,
}

/// The slot registry.
#[derive(Clone, Debug, Default)]
pub struct SmartObjects {
    objects: BTreeMap<Id, Object>,
}

impl SmartObjects {
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Registers or redefines an object. Redefining keeps the state of slots with the
    /// same index and name; other slots start free (their users are released as
    /// [`ReleaseReason::Removed`], returned).
    ///
    /// # Errors
    /// An invalid definition, or too many objects.
    pub fn set_object(&mut self, id: Id, def: ObjectDef) -> Result<Vec<SmartEvent>, String> {
        def.validate()?;
        if !self.objects.contains_key(&id) && self.objects.len() >= MAX_OBJECTS {
            return Err(format!("more than {MAX_OBJECTS} smart objects"));
        }
        let mut events = Vec::new();
        let mut states = vec![SlotState::Free; def.slots.len()];
        if let Some(old) = self.objects.get(&id) {
            for (index, state) in old.states.iter().enumerate() {
                let kept = def.slots.get(index).is_some_and(|slot| {
                    old.def
                        .slots
                        .get(index)
                        .is_some_and(|o| o.name == slot.name)
                });
                if kept {
                    states[index] = *state;
                } else if let Some(user) = state.user() {
                    events.push(SmartEvent::Released {
                        slot: SlotRef {
                            object: id,
                            slot: index as u32,
                        },
                        user,
                        reason: ReleaseReason::Removed,
                    });
                }
            }
        }
        self.objects.insert(id, Object { def, states });
        Ok(events)
    }

    /// Forgets an object, releasing everyone who held one of its slots.
    pub fn remove_object(&mut self, id: Id) -> Vec<SmartEvent> {
        let Some(object) = self.objects.remove(&id) else {
            return Vec::new();
        };
        object
            .states
            .iter()
            .enumerate()
            .filter_map(|(index, state)| {
                state.user().map(|user| SmartEvent::Released {
                    slot: SlotRef {
                        object: id,
                        slot: index as u32,
                    },
                    user,
                    reason: ReleaseReason::Removed,
                })
            })
            .collect()
    }

    pub fn def(&self, object: Id) -> Option<&ObjectDef> {
        self.objects.get(&object).map(|o| &o.def)
    }
    pub fn slot_def(&self, slot: SlotRef) -> Option<&SlotDef> {
        self.objects
            .get(&slot.object)?
            .def
            .slots
            .get(slot.slot as usize)
    }
    pub fn state(&self, slot: SlotRef) -> Option<SlotState> {
        self.objects
            .get(&slot.object)?
            .states
            .get(slot.slot as usize)
            .copied()
    }
    /// Ids of every registered object.
    pub fn object_ids(&self) -> impl Iterator<Item = Id> + '_ {
        self.objects.keys().copied()
    }
    /// Every slot with its state, ordered by `(object, slot)`.
    pub fn slots(&self) -> impl Iterator<Item = (SlotRef, &SlotDef, SlotState)> + '_ {
        self.objects.iter().flat_map(|(object, o)| {
            o.def
                .slots
                .iter()
                .zip(&o.states)
                .enumerate()
                .map(move |(index, (def, state))| {
                    (
                        SlotRef {
                            object: *object,
                            slot: index as u32,
                        },
                        def,
                        *state,
                    )
                })
        })
    }

    /// Slots that carry every tag in `tags` (any slot for an empty list), optionally
    /// only free ones, ordered by `(object, slot)`.
    pub fn find(&self, tags: &[String], free_only: bool) -> Vec<SlotRef> {
        self.slots()
            .filter(|(_, def, state)| {
                (!free_only || state.is_free()) && tags.iter().all(|t| def.tags.contains(t))
            })
            .map(|(slot, _, _)| slot)
            .collect()
    }

    /// Reserves a free slot for `user`.
    ///
    /// # Errors
    /// [`SlotError::UnknownSlot`], or [`SlotError::Taken`] when anyone (the same user
    /// included) holds it.
    pub fn claim(&mut self, slot: SlotRef, user: Id) -> Result<SmartEvent, SlotError> {
        let object = self
            .objects
            .get_mut(&slot.object)
            .ok_or(SlotError::UnknownSlot)?;
        let timeout = f64::from(object.def.claim_timeout);
        let state = object
            .states
            .get_mut(slot.slot as usize)
            .ok_or(SlotError::UnknownSlot)?;
        if let Some(holder) = state.user() {
            return Err(SlotError::Taken(holder));
        }
        *state = SlotState::Claimed {
            user,
            remaining: (timeout > 0.0).then_some(timeout),
        };
        Ok(SmartEvent::Claimed { slot, user })
    }

    /// Starts the use of a slot the user claimed.
    ///
    /// # Errors
    /// [`SlotError::NotClaimed`] unless `user` holds a claim on the slot.
    pub fn occupy(&mut self, slot: SlotRef, user: Id) -> Result<SmartEvent, SlotError> {
        let object = self
            .objects
            .get_mut(&slot.object)
            .ok_or(SlotError::UnknownSlot)?;
        let state = object
            .states
            .get_mut(slot.slot as usize)
            .ok_or(SlotError::UnknownSlot)?;
        match *state {
            SlotState::Claimed { user: holder, .. } if holder == user => {
                *state = SlotState::Occupied { user };
                Ok(SmartEvent::Activated { slot, user })
            }
            _ => Err(SlotError::NotClaimed),
        }
    }

    /// Frees a slot `user` holds (claimed or occupied).
    ///
    /// # Errors
    /// [`SlotError::NotClaimed`] when `user` does not hold it.
    pub fn release(
        &mut self,
        slot: SlotRef,
        user: Id,
        reason: ReleaseReason,
    ) -> Result<SmartEvent, SlotError> {
        let object = self
            .objects
            .get_mut(&slot.object)
            .ok_or(SlotError::UnknownSlot)?;
        let state = object
            .states
            .get_mut(slot.slot as usize)
            .ok_or(SlotError::UnknownSlot)?;
        if state.user() != Some(user) {
            return Err(SlotError::NotClaimed);
        }
        *state = SlotState::Free;
        Ok(SmartEvent::Released { slot, user, reason })
    }

    /// Frees everything `user` holds (the user was removed).
    pub fn purge_user(&mut self, user: Id) -> Vec<SmartEvent> {
        let mut events = Vec::new();
        for (object, o) in &mut self.objects {
            for (index, state) in o.states.iter_mut().enumerate() {
                if state.user() == Some(user) {
                    *state = SlotState::Free;
                    events.push(SmartEvent::Released {
                        slot: SlotRef {
                            object: *object,
                            slot: index as u32,
                        },
                        user,
                        reason: ReleaseReason::Removed,
                    });
                }
            }
        }
        events
    }

    /// Advances claim timers; expired claims are released.
    pub fn tick(&mut self, dt: f32) -> Vec<SmartEvent> {
        let mut events = Vec::new();
        for (object, o) in &mut self.objects {
            for (index, state) in o.states.iter_mut().enumerate() {
                let SlotState::Claimed { user, remaining } = state else {
                    continue;
                };
                let Some(left) = remaining else {
                    continue;
                };
                *left -= f64::from(dt);
                if *left <= EPS {
                    let user = *user;
                    *state = SlotState::Free;
                    events.push(SmartEvent::Released {
                        slot: SlotRef {
                            object: *object,
                            slot: index as u32,
                        },
                        user,
                        reason: ReleaseReason::Timeout,
                    });
                }
            }
        }
        events
    }

    /// Serves claim requests in a fixed order and returns the claimed slot per
    /// request, in the order served: requests sort by priority (high first), then by
    /// user id; each takes the nearest free slot carrying its tags (ties: lowest
    /// `(object, slot)`). `position` maps a slot to its world position. A user whose
    /// request finds nothing is answered with `None`.
    pub fn arbitrate(
        &mut self,
        requests: &[ClaimRequest],
        position: impl Fn(SlotRef) -> [f32; 3],
    ) -> Vec<(Id, Option<(SlotRef, SmartEvent)>)> {
        let mut order: Vec<&ClaimRequest> = requests.iter().collect();
        order.sort_by_key(|r| (std::cmp::Reverse(r.priority), r.user));
        let mut served = Vec::with_capacity(order.len());
        for request in order {
            let best = self
                .find(&request.tags, true)
                .into_iter()
                .map(|slot| {
                    let p = position(slot);
                    let d = [
                        p[0] - request.position[0],
                        p[1] - request.position[1],
                        p[2] - request.position[2],
                    ];
                    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2], slot)
                })
                .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
                .map(|(_, slot)| slot);
            let outcome = best.and_then(|slot| {
                self.claim(slot, request.user)
                    .ok()
                    .map(|event| (slot, event))
            });
            served.push((request.user, outcome));
        }
        served
    }
}
