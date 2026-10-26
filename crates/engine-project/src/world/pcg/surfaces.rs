//! Resources a game or another world module uses to customise generation.
use bevy_ecs::prelude::*;
use engine_pcg::{FlatSurface, Registry, Surface};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Named ground providers for [`PcgVolume::surface`](super::PcgVolume).
///
/// The empty name selects the default surface: flat ground at `y = 0` unless replaced with
/// [`set_default`](Self::set_default). A terrain module registers an adapter implementing
/// [`Surface`] under a name volumes can reference. Surface contents are not tracked for
/// changes: bump the volume's `regenerate` counter after the ground changes.
#[derive(Resource, Clone)]
pub struct PcgSurfaceRegistry {
    default: Arc<dyn Surface>,
    named: BTreeMap<String, Arc<dyn Surface>>,
}

impl Default for PcgSurfaceRegistry {
    fn default() -> Self {
        Self {
            default: Arc::new(FlatSurface::new(0.0)),
            named: BTreeMap::new(),
        }
    }
}

impl PcgSurfaceRegistry {
    /// Replaces the surface used by volumes with an empty `surface` name.
    pub fn set_default(&mut self, surface: Arc<dyn Surface>) {
        self.default = surface;
    }
    /// Registers (or replaces) a named surface.
    pub fn insert(&mut self, name: impl Into<String>, surface: Arc<dyn Surface>) {
        self.named.insert(name.into(), surface);
    }
    pub fn remove(&mut self, name: &str) -> Option<Arc<dyn Surface>> {
        self.named.remove(name)
    }
    /// The surface for `name` (`""` is the default); `None` when the name is unknown.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Surface>> {
        if name.is_empty() {
            Some(self.default.clone())
        } else {
            self.named.get(name).cloned()
        }
    }
}

/// The node-kind registry volumes run against. Starts as [`Registry::builtin`]; games add
/// their own kinds with `Registry::register` before the first tick.
#[derive(Resource, Clone)]
pub struct PcgNodes(pub Registry);

impl Default for PcgNodes {
    fn default() -> Self {
        Self(Registry::builtin())
    }
}
