//! Dimension-generic joint side table and `World` methods; the reference feature for the
//! extension seam (see `src/world.rs`). `super` is the dimension's joint descriptor module
//! and `super::super` the dimension module.
use super::super::{Core, Extension, World};
use super::JointDesc;
use crate::{Error, Result};
use std::collections::BTreeMap;

/// Joint ID -> (Rapier handle, first body ID, second body ID).
#[derive(Default)]
struct Joints {
    joints: BTreeMap<u64, (super::ImpulseJointHandle, u64, u64)>,
}
impl Extension for Joints {
    fn body_removed(&mut self, _core: &mut Core, id: u64) {
        // Rapier already removed the attached impulse joints with the body.
        self.joints
            .retain(|_, (_, first, second)| *first != id && *second != id);
    }
}
impl World {
    /// Joint IDs have a separate namespace from body IDs. At most four joints per
    /// configured body-capacity slot may be allocated in the world.
    pub fn insert_joint(
        &mut self,
        id: u64,
        first: u64,
        second: u64,
        desc: JointDesc,
    ) -> Result<()> {
        if first == second {
            return Err(Error("joint requires distinct bodies"));
        }
        let (core, table) = self.extension_mut::<Joints>();
        if table.joints.contains_key(&id) {
            return Err(Error("duplicate joint ID"));
        }
        if table.joints.len() >= core.capacity() * 4 {
            return Err(Error("joint capacity exceeded"));
        }
        let a = core
            .entry(first)
            .map_err(|_| Error("unknown first joint body"))?
            .body;
        let b = core
            .entry(second)
            .map_err(|_| Error("unknown second joint body"))?
            .body;
        let joint = desc.build()?;
        let handle = core.rapier.insert_impulse_joint(a, b, joint);
        table.joints.insert(id, (handle, first, second));
        Ok(())
    }
    pub fn remove_joint(&mut self, id: u64) -> bool {
        let (core, table) = self.extension_mut::<Joints>();
        if let Some((handle, _, _)) = table.joints.remove(&id) {
            core.rapier.remove_impulse_joint(handle);
            true
        } else {
            false
        }
    }
    pub fn joint_count(&self) -> usize {
        self.extension::<Joints>().map_or(0, |t| t.joints.len())
    }
    pub fn has_joint(&self, id: u64) -> bool {
        self.extension::<Joints>()
            .is_some_and(|t| t.joints.contains_key(&id))
    }
}
