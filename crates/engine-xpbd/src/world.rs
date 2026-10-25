//! The XPBD world: objects keyed by stable `u64` ids, settings and the fixed step.
use std::collections::BTreeMap;

use glam::Vec3;

use crate::particles::StepContext;
use crate::proxy::{ImpulseAccum, Prepared, ProxyImpulse, prepare};
use crate::{
    Cloth, ClothDesc, ColliderProxy, Error, MAX_ITERATIONS, MAX_OBJECTS, MAX_SPEED, MAX_STEP,
    MAX_SUBSTEPS, MAX_WORLD_PARTICLES, Result, SoftBody, SoftBodyDesc, finite_bounded, in_range,
};

/// Largest accepted air density in kg/m^3.
const MAX_AIR_DENSITY: f32 = 100.0;

/// World-wide solver settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XpbdSettings {
    /// Gravity acceleration in m/s^2.
    pub gravity: Vec3,
    /// Uniform wind velocity in m/s, seen by cloth aerodynamics.
    pub wind: Vec3,
    /// Air density in kg/m^3 (0 disables aerodynamics, including still-air drag).
    pub air_density: f32,
    /// Fixed substeps per step (1..=[`MAX_SUBSTEPS`]).
    pub substeps: u32,
    /// Constraint passes per substep (1..=[`MAX_ITERATIONS`]).
    pub iterations: u32,
}

impl Default for XpbdSettings {
    fn default() -> Self {
        Self {
            gravity: Vec3::new(0.0, -9.81, 0.0),
            wind: Vec3::ZERO,
            air_density: 1.2,
            substeps: 10,
            iterations: 1,
        }
    }
}

impl XpbdSettings {
    pub fn validate(&self) -> Result<()> {
        let ok = finite_bounded(self.gravity, MAX_SPEED)
            && finite_bounded(self.wind, MAX_SPEED)
            && in_range(self.air_density, 0.0, MAX_AIR_DENSITY)
            && (1..=MAX_SUBSTEPS).contains(&self.substeps)
            && (1..=MAX_ITERATIONS).contains(&self.iterations);
        if ok {
            Ok(())
        } else {
            Err(Error("invalid xpbd settings"))
        }
    }
}

/// What kind of object an id refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Cloth,
    SoftBody,
}

#[derive(Clone, Debug)]
enum Object {
    Cloth(Box<Cloth>),
    SoftBody(Box<SoftBody>),
}

impl Object {
    fn particle_count(&self) -> usize {
        match self {
            Self::Cloth(cloth) => cloth.particle_count(),
            Self::SoftBody(body) => body.particle_count(),
        }
    }
}

/// Cloths and soft bodies sharing settings, a particle budget and collider proxies.
#[derive(Clone, Debug)]
pub struct XpbdWorld {
    settings: XpbdSettings,
    max_particles: usize,
    particle_count: usize,
    objects: BTreeMap<u64, Object>,
    prepared: Vec<Prepared>,
    accum: Vec<ImpulseAccum>,
    impulses: Vec<ProxyImpulse>,
}

impl XpbdWorld {
    /// Creates an empty world that holds at most `max_particles` particles in total
    /// (1..=[`MAX_WORLD_PARTICLES`]).
    pub fn new(settings: XpbdSettings, max_particles: usize) -> Result<Self> {
        settings.validate()?;
        if !(1..=MAX_WORLD_PARTICLES).contains(&max_particles) {
            return Err(Error("world particle capacity out of range"));
        }
        Ok(Self {
            settings,
            max_particles,
            particle_count: 0,
            objects: BTreeMap::new(),
            prepared: Vec::new(),
            accum: Vec::new(),
            impulses: Vec::new(),
        })
    }

    pub fn settings(&self) -> XpbdSettings {
        self.settings
    }

    pub fn set_settings(&mut self, settings: XpbdSettings) -> Result<()> {
        settings.validate()?;
        self.settings = settings;
        Ok(())
    }

    /// Total particles over all objects.
    pub fn particle_count(&self) -> usize {
        self.particle_count
    }

    pub fn max_particles(&self) -> usize {
        self.max_particles
    }

    /// Validates `desc` and inserts a cloth under a new id.
    pub fn insert_cloth(&mut self, id: u64, desc: ClothDesc) -> Result<()> {
        self.check_insert(id, desc.positions.len())?;
        let cloth = Cloth::new(desc)?;
        self.particle_count += cloth.particle_count();
        self.objects.insert(id, Object::Cloth(Box::new(cloth)));
        Ok(())
    }

    /// Validates `desc` and inserts a soft body under a new id.
    pub fn insert_soft_body(&mut self, id: u64, desc: SoftBodyDesc) -> Result<()> {
        self.check_insert(id, desc.positions.len())?;
        let body = SoftBody::new(desc)?;
        self.particle_count += body.particle_count();
        self.objects.insert(id, Object::SoftBody(Box::new(body)));
        Ok(())
    }

    fn check_insert(&self, id: u64, particles: usize) -> Result<()> {
        if self.objects.contains_key(&id) {
            return Err(Error("xpbd object id already exists"));
        }
        if self.objects.len() >= MAX_OBJECTS {
            return Err(Error("xpbd object capacity exceeded"));
        }
        if particles > self.max_particles - self.particle_count {
            return Err(Error("xpbd world particle capacity exceeded"));
        }
        Ok(())
    }

    /// Removes an object; returns whether it existed.
    pub fn remove(&mut self, id: u64) -> bool {
        match self.objects.remove(&id) {
            Some(object) => {
                self.particle_count -= object.particle_count();
                true
            }
            None => false,
        }
    }

    pub fn kind(&self, id: u64) -> Option<ObjectKind> {
        self.objects.get(&id).map(|object| match object {
            Object::Cloth(_) => ObjectKind::Cloth,
            Object::SoftBody(_) => ObjectKind::SoftBody,
        })
    }

    /// Object ids in ascending order (the step order).
    pub fn ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.objects.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn cloth(&self, id: u64) -> Option<&Cloth> {
        match self.objects.get(&id)? {
            Object::Cloth(cloth) => Some(cloth),
            Object::SoftBody(_) => None,
        }
    }

    pub fn cloth_mut(&mut self, id: u64) -> Option<&mut Cloth> {
        match self.objects.get_mut(&id)? {
            Object::Cloth(cloth) => Some(cloth),
            Object::SoftBody(_) => None,
        }
    }

    pub fn soft_body(&self, id: u64) -> Option<&SoftBody> {
        match self.objects.get(&id)? {
            Object::SoftBody(body) => Some(body),
            Object::Cloth(_) => None,
        }
    }

    pub fn soft_body_mut(&mut self, id: u64) -> Option<&mut SoftBody> {
        match self.objects.get_mut(&id)? {
            Object::SoftBody(body) => Some(body),
            Object::Cloth(_) => None,
        }
    }

    /// Advances every object by `dt` seconds (0 < dt <= [`MAX_STEP`]) against `proxies`.
    ///
    /// Proxies are validated and sorted by id; duplicate ids are rejected. Objects step in
    /// ascending id order. Afterwards [`Self::proxy_impulses`] holds the reaction impulse
    /// of every proxy that had contacts. Invalid input leaves the world unchanged; a
    /// non-finite result (not expected for validated input) returns an error.
    pub fn step(&mut self, dt: f32, proxies: &[ColliderProxy]) -> Result<()> {
        if !in_range(dt, f32::MIN_POSITIVE, MAX_STEP) {
            return Err(Error("xpbd step must be in (0, MAX_STEP] seconds"));
        }
        prepare(proxies, &mut self.prepared)?;
        self.accum.clear();
        self.accum
            .resize(self.prepared.len(), ImpulseAccum::default());
        let ctx = StepContext {
            dt,
            substeps: self.settings.substeps,
            iterations: self.settings.iterations,
            gravity: self.settings.gravity,
            wind: self.settings.wind,
            air_density: self.settings.air_density,
        };
        let mut finite = true;
        for object in self.objects.values_mut() {
            finite &= match object {
                Object::Cloth(cloth) => {
                    cloth.step(&ctx, &self.prepared, &mut self.accum);
                    cloth.all_finite()
                }
                Object::SoftBody(body) => {
                    body.step(&ctx, &self.prepared, &mut self.accum);
                    body.all_finite()
                }
            };
        }
        self.impulses.clear();
        self.impulses.extend(
            self.prepared
                .iter()
                .zip(&self.accum)
                .filter_map(|(proxy, sum)| sum.report(proxy.id)),
        );
        if finite {
            Ok(())
        } else {
            Err(Error("xpbd simulation produced non-finite state"))
        }
    }

    /// Reaction impulses of the last step, sorted by proxy id; proxies without contacts
    /// are omitted.
    pub fn proxy_impulses(&self) -> &[ProxyImpulse] {
        &self.impulses
    }
}
