//! Optional native-script ML service. Install explicitly on the authoritative ECS world.
use crate::{Error, Result};
use bevy_ecs::prelude::{Resource, World};
pub use engine_ml::{
    BackendStatus, Completion, DType, JobId, LoadOptions, Model, ModelInfo, ModelLimits,
    RuntimeConfig, Tensor, TensorData, backend_status,
};
use std::sync::Arc;
#[derive(Resource)]
struct MlService(std::sync::Mutex<engine_ml::InferenceRuntime>);
fn convert(error: engine_ml::MlError) -> Error {
    Error::new(error.code, error.message)
}
/// Creates bounded CPU workers. Reinstallation closes and joins the previous runtime.
pub fn install(world: &mut World, config: RuntimeConfig) -> Result<()> {
    let runtime = engine_ml::InferenceRuntime::new(config).map_err(convert)?;
    world.insert_resource(MlService(std::sync::Mutex::new(runtime)));
    Ok(())
}
pub fn available(world: &World) -> bool {
    world.contains_resource::<MlService>()
}
/// Resolve a project asset through the same path policy used by native scripts.
pub fn load_asset(world: &World, asset: &str, options: LoadOptions) -> Result<Arc<Model>> {
    let path = crate::services::resolve_asset(world, asset)?;
    Model::load_path(path, options)
        .map(Arc::new)
        .map_err(convert)
}
fn with_runtime<T>(
    world: &World,
    action: impl FnOnce(&mut engine_ml::InferenceRuntime) -> Result<T>,
) -> Result<T> {
    let service = world.get_resource::<MlService>().ok_or_else(|| {
        Error::new(
            "ml.runtime.unavailable",
            "Install the native ML service before submitting inference",
        )
    })?;
    let mut runtime = service
        .0
        .lock()
        .map_err(|_| Error::new("ml.runtime.poisoned", "ML service lock poisoned"))?;
    action(&mut runtime)
}
pub fn submit(world: &World, model: Arc<Model>, inputs: Vec<Tensor>) -> Result<JobId> {
    with_runtime(world, |runtime| {
        runtime.submit(model, inputs).map_err(convert)
    })
}
pub fn poll(world: &World) -> Result<Option<Completion>> {
    with_runtime(world, |runtime| Ok(runtime.poll()))
}
pub fn cancel(world: &World, id: JobId) -> Result<()> {
    with_runtime(world, |runtime| runtime.cancel(id).map_err(convert))
}
/// Removes the service and joins workers. Call outside the frame loop because a running
/// CPU inference finishes before its worker can stop.
pub fn shutdown(world: &mut World) {
    world.remove_resource::<MlService>();
}
