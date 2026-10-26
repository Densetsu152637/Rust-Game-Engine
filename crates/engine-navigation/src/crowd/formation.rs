//! Formation slot geometry and deterministic slot assignment.
use super::AgentId;
use glam::Vec2;

pub const MAX_FORMATION_SLOTS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FormationAnchor {
    /// A fixed world position.
    Fixed([f32; 3]),
    /// Follow a crowd agent (the leader). The leader itself never takes a slot.
    Leader(AgentId),
}
#[derive(Clone, Debug, PartialEq)]
pub struct FormationDesc {
    /// Slot offsets in formation space: `[right, forward]` metres from the anchor.
    pub slots: Vec<[f32; 2]>,
    pub anchor: FormationAnchor,
    /// Heading in radians; forward is `(sin yaw, cos yaw)` on `(x, z)`.
    pub yaw: f32,
    /// When anchored to a leader, rotate the formation to the leader's travel direction.
    pub follow_heading: bool,
}
impl FormationDesc {
    pub fn slot_world(&self, anchor: [f32; 3], yaw: f32, slot: usize) -> Option<[f32; 3]> {
        let [right, forward] = *self.slots.get(slot)?;
        let (s, c) = yaw.sin_cos();
        Some([
            anchor[0] + right * c + forward * s,
            anchor[1],
            anchor[2] - right * s + forward * c,
        ])
    }
}
/// Greedy nearest-pair matching: all (member, slot) pairs ordered by squared distance, then
/// member id, then slot index; each member and slot is used once. Deterministic and stable under
/// identical inputs. Members beyond the slot count get no slot.
pub(super) fn assign_slots(members: &[(AgentId, Vec2)], slots: &[Vec2]) -> Vec<(AgentId, usize)> {
    let mut pairs: Vec<(u32, AgentId, usize)> = Vec::with_capacity(members.len() * slots.len());
    for &(id, p) in members {
        for (s, q) in slots.iter().enumerate() {
            pairs.push((p.distance_squared(*q).to_bits(), id, s));
        }
    }
    pairs.sort_unstable();
    let mut used_slot = vec![false; slots.len()];
    let mut out: Vec<(AgentId, usize)> = Vec::new();
    for (_, id, s) in pairs {
        if used_slot[s] || out.iter().any(|(m, _)| *m == id) {
            continue;
        }
        used_slot[s] = true;
        out.push((id, s));
    }
    out.sort_unstable();
    out
}
