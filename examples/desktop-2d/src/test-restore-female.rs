pub fn factory(
    _: &mut engine_core::bevy_ecs::world::World,
    _: engine_core::bevy_ecs::entity::Entity,
) -> engine_project::Result<Box<dyn engine_project::NativeScript>> {
    Ok(Box::new(crate::controller::Controller::new(
        "female",
        "restore-test",
    )))
}
