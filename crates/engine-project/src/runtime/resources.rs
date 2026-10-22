use super::*;

#[derive(Component, Clone, Debug)]
pub struct AuthoredId {
    pub id: String,
    pub name: String,
}
#[derive(Resource, Clone)]
pub struct ProjectResource(pub Arc<Project>);
#[derive(Resource, Clone, Default)]
pub struct RuntimeLog(pub Vec<String>);
#[derive(Resource, Clone, Default)]
pub struct EntityLookup(pub BTreeMap<String, Entity>);
impl EntityLookup {
    pub fn get(&self, id: &str) -> Option<Entity> {
        self.0.get(id).copied()
    }
}
#[derive(Resource, Clone, Debug)]
pub struct InputSettings {
    pub bindings: BindingMap,
    pub deadzone: f32,
    pub revision: u64,
}
impl InputSettings {
    pub fn rebind(&mut self, bindings: BindingMap, deadzone: f32) -> Result<()> {
        let check = crate::input::validate_bindings(&bindings, false);
        if !check.errors.is_empty() || !deadzone.is_finite() || !(0.0..1.0).contains(&deadzone) {
            return Err(Error::new(
                "input.bindings.invalid",
                check.errors.join("; "),
            ));
        }
        self.bindings = bindings;
        self.deadzone = deadzone;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
}
#[derive(Resource, Clone, Default)]
pub struct TriggerEvents(pub Vec<TriggerEvent>);
#[derive(Resource, Clone, Default)]
pub struct GameState(pub serde_json::Map<String, serde_json::Value>);
#[derive(Resource, Clone)]
pub struct Persistence(pub Arc<PersistenceStore>);
#[derive(Resource, Clone, Default)]
pub struct ControlLocks(pub BTreeSet<String>);
#[derive(Clone, Debug)]
pub struct SceneRequest {
    pub asset: String,
    pub spawn: String,
    pub traveller: String,
}
#[derive(Resource, Clone, Default)]
pub struct PendingScene(pub Option<SceneRequest>);
#[derive(Component, Clone, Copy, Debug)]
pub struct SpriteFrame(pub usize);
#[derive(Component, Clone, Copy, Debug)]
pub struct OwnedBy(pub Entity);
