//! Contact-impulse read-back and forced sleep, the engine-physics support destruction needs
//! (`docs/physics-destruction.md`).
//!
//! Dimension-generic: `dim2.rs` and `dim3.rs` each include this file, so [`ContactImpulse`]
//! and the `World` methods exist once per dimension. No per-world state is kept: impulses are
//! read from Rapier's narrow phase, which holds the solver result of the last `step`.
use super::*;
use crate::{Error, Result};

/// Contact load a body received in the last `step`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ContactImpulse {
    /// Sum of the normal contact impulse magnitudes applied to the body (N*s), over every
    /// solid contact it has. Zero when it touched nothing.
    pub impulse: f32,
    /// Impulse-weighted contact point in world space (the origin when `impulse` is zero).
    pub point: Position,
    /// Body that contributed the largest share of `impulse`, if it is an application body.
    pub other: Option<u64>,
}
impl World {
    /// Contact impulse the solver applied to body `id` during the last `step`.
    ///
    /// The value is the magnitude along contact normals, summed over all contact points and
    /// pairs, so a hit shared across several substeps must be read after each one. Sensor
    /// colliders produce no contacts and contribute nothing.
    pub fn contact_impulse(&self, id: u64) -> Result<ContactImpulse> {
        let collider = self.core.entry(id)?.collider;
        let mut total = 0.;
        let mut weighted = Vector::ZERO;
        let mut best: Option<(f32, u64)> = None;
        for pair in self.core.rapier.contact_pairs_with(collider) {
            let first = pair.collider1 == collider;
            let other = if first {
                pair.collider2
            } else {
                pair.collider1
            };
            let Some(owner) = self.core.rapier.colliders.get(pair.collider1) else {
                continue;
            };
            let pose = *owner.position();
            let mut share = 0.;
            for manifold in pair.solver_manifolds() {
                for point in &manifold.points {
                    let impulse = point.data.impulse.max(0.);
                    share += impulse;
                    weighted += (pose * point.local_p1) * impulse;
                }
            }
            total += share;
            if share > 0. && best.is_none_or(|(b, _)| share > b) {
                best = self.core.collider_body_id(other).map(|b| (share, b));
            }
        }
        let point = if total > 0. {
            (weighted / total).to_array()
        } else {
            Position::default()
        };
        Ok(ContactImpulse {
            impulse: total,
            point,
            other: best.map(|(_, b)| b),
        })
    }
    /// Whether Rapier has put the body to sleep. Fixed bodies never sleep.
    pub fn is_sleeping(&self, id: u64) -> Result<bool> {
        Ok(self.core.body(id)?.is_sleeping())
    }
    /// Puts a dynamic body to sleep at once, zeroing its velocity. Any contact or impulse
    /// wakes it again.
    pub fn sleep(&mut self, id: u64) -> Result<()> {
        let body = self.core.body_mut(id)?;
        if !body.is_dynamic() {
            return Err(Error("sleep requires dynamic body"));
        }
        body.sleep();
        Ok(())
    }
}
