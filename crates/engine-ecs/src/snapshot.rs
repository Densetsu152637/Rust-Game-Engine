use std::collections::HashMap;

use bevy_ecs::prelude::*;

/// Native extraction systems may join `Components`; custom dependent extraction
/// work should use an explicit edge before/after that set.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExtractionSet {
    Cleanup,
    Components,
}

/// Source identity for an extracted entity. It is local to the simulation world,
/// not a persistent asset/entity ID and not an entity in the render world.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractedFrom(pub Entity);

/// Separate owned render snapshot, temporarily available as a simulation-world
/// resource during `Stage::Extract`. Do not remove or replace this resource.
#[derive(Resource, Default)]
pub struct RenderWorld {
    world: World,
    entities: HashMap<Entity, Entity>,
}

impl RenderWorld {
    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Returns the live render identity corresponding to a simulation identity.
    pub fn entity_for(&self, source: Entity) -> Option<Entity> {
        self.entities
            .get(&source)
            .copied()
            .filter(|entity| self.world.get_entity(*entity).is_ok())
    }

    /// Creates a render entity if extraction has not yet created one.
    pub fn entity_or_spawn(&mut self, source: Entity) -> Entity {
        if let Some(entity) = self.entity_for(source) {
            return entity;
        }
        let entity = self.world.spawn(ExtractedFrom(source)).id();
        self.entities.insert(source, entity);
        entity
    }
}

/// Despawns render entities whose simulation source no longer exists.
/// `EngineApp` installs this in `ExtractionSet::Cleanup`.
pub fn cleanup_extracted_entities(live: Query<Entity>, mut render: ResMut<RenderWorld>) {
    let stale: Vec<_> = render
        .entities
        .keys()
        .copied()
        .filter(|source| live.get(*source).is_err())
        .collect();
    for source in stale {
        if let Some(entity) = render.entities.remove(&source) {
            render.world.despawn(entity);
        }
    }
}

/// Copies only added/changed `T` values and propagates removals. Register through
/// `EngineApp::extract_component` or place it after `ExtractionSet::Cleanup`.
/// Native components need no serialization or reflection support to be extracted.
pub fn extract_component<T: Component + Clone>(
    changed: Query<(Entity, &T), Changed<T>>,
    mut removed: RemovedComponents<T>,
    mut render: ResMut<RenderWorld>,
) {
    for source in removed.read() {
        if let Some(entity) = render.entity_for(source) {
            render.world.entity_mut(entity).remove::<T>();
        }
    }
    for (source, value) in &changed {
        let entity = render.entity_or_spawn(source);
        render.world.entity_mut(entity).insert(value.clone());
    }
}
