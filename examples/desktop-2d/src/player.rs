pub fn factory(
    _: &mut engine::core::bevy_ecs::world::World,
    _: engine::core::bevy_ecs::entity::Entity,
) -> engine::project::Result<Box<dyn engine::project::NativeScript>> {
    Ok(Box::new(crate::controller::Controller::new(
        "male",
        "interactive",
    )))
}
