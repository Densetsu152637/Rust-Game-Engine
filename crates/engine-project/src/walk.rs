//! Narrow entity queries over a shared `&World`.
//!
//! `World::iter_entities` visits every entity, so a pass built on it costs time in
//! proportion to *all* entities, including the ones (a game's own simulation entities)
//! that carry none of the components the pass reads. Every runtime pass that only
//! needs entities with some component goes through here instead: the typed query
//! matches whole archetypes first, so entities without the component are never touched.
//!
//! The helpers work from `&World` (they build a [`QueryState`] on the fly), return the
//! matching [`EntityRef`]s in archetype order, and treat a component type that was
//! never registered in the world as "no entity has it".
use bevy_ecs::prelude::*;
use bevy_ecs::query::QueryFilter;

/// Entities matching the filter `F`, for example `With<A>` or `(With<A>, With<B>)`.
pub(crate) fn matching<'w, F: QueryFilter>(
    world: &'w World,
) -> impl Iterator<Item = EntityRef<'w>> + 'w {
    world
        .try_query_filtered::<EntityRef, F>()
        .map(|mut query| query.iter(world).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
}

/// Entities that have the component `T`.
pub(crate) fn with<'w, T: Component>(world: &'w World) -> impl Iterator<Item = EntityRef<'w>> + 'w {
    matching::<With<T>>(world)
}

/// Entities that have both components `A` and `B`.
pub(crate) fn with_both<'w, A: Component, B: Component>(
    world: &'w World,
) -> impl Iterator<Item = EntityRef<'w>> + 'w {
    matching::<(With<A>, With<B>)>(world)
}

/// The entities with `T`, collected (a part of [`with_any!`]).
pub(crate) fn collect<'w, T: Component>(world: &'w World) -> Vec<EntityRef<'w>> {
    with::<T>(world).collect()
}

/// The union of several entity lists: each entity once, in the order `World::iter_entities`
/// visits them (by archetype, then by row), so a pass keeps the order it always had.
pub(crate) fn merge<'w>(
    parts: impl IntoIterator<Item = Vec<EntityRef<'w>>>,
) -> impl Iterator<Item = EntityRef<'w>> + 'w {
    let mut all: Vec<_> = parts.into_iter().flatten().collect();
    let place = |e: &EntityRef| {
        let location = e.location();
        (
            location.archetype_id.index(),
            location.archetype_row.index(),
        )
    };
    all.sort_by_key(place);
    all.dedup_by_key(|e| e.id());
    all.into_iter()
}

/// Entities that have at least one of the listed components, each once, in storage order:
/// `with_any!(world, A, B, C)`. One typed query per component, so an unregistered
/// component simply contributes nothing.
macro_rules! with_any {
    ($world:expr, $($component:ty),+ $(,)?) => {
        $crate::walk::merge([$($crate::walk::collect::<$component>($world)),+])
    };
}
pub(crate) use with_any;

/// A copy of the component `T` of every entity that has it, keyed by entity.
pub(crate) fn cloned<T: Component + Clone>(world: &World) -> std::collections::BTreeMap<Entity, T> {
    with::<T>(world)
        .filter_map(|e| Some((e.id(), e.get::<T>()?.clone())))
        .collect()
}
