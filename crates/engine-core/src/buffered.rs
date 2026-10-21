use bevy_ecs::prelude::*;

/// Published simulation value. Writers of `Next<T>` do not invalidate readers of this value.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct Current<T: Send + Sync + 'static>(pub T);

/// Pending simulation value. Publish after all dependent readers/writers using schedule edges.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct Next<T: Send + Sync + 'static>(pub T);

/// Copies pending values to current values only when different.
///
/// This is an ordinary native system: the caller chooses its dependency edges.
/// It does not run implicitly after every system, nor impose a second scheduler.
pub fn publish_buffered<T: Clone + PartialEq + Send + Sync + 'static>(
    mut values: Query<(&mut Current<T>, &Next<T>)>,
) {
    for (mut current, next) in &mut values {
        if current.0 != next.0 {
            current.0 = next.0.clone();
        }
    }
}
