use engine_project::input::BindingMap;
/// Fixture configuration stays in the authored manifest, including gamepad and
/// pointer tokens. Settings save the complete map, not only a changed action.
pub fn defaults() -> BindingMap {
    serde_json::from_value(
        serde_json::from_str::<serde_json::Value>(include_str!("../project.json"))
            .expect("authored project JSON")["inputBindings"]
            .clone(),
    )
    .expect("authored binding map")
}
