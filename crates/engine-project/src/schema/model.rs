use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SceneEntity {
    pub id: String,
    pub name: String,
    pub components: Components,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub schema: u32,
    #[serde(rename = "scene_id")]
    pub id: String,
    pub entities: Vec<SceneEntity>,
    #[serde(skip)]
    pub source: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub id: String,
    pub path: PathBuf,
    pub kind: String,
}
#[derive(Clone, Debug)]
pub struct Project {
    pub schema: u32,
    pub name: String,
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub startup_scene: PathBuf,
    pub rust_manifest: PathBuf,
    pub assets: BTreeMap<String, Asset>,
    pub input_actions: BTreeMap<String, String>,
    pub input_bindings: BindingMap,
    pub scene: Scene,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub(super) schema: u32,
    pub(super) name: String,
    pub(super) startup_scene: PathBuf,
    pub(super) assets: Vec<Asset>,
    #[serde(default)]
    pub(super) rust: Option<RustConfig>,
    #[serde(default, rename = "inputActions")]
    pub(super) input_actions: BTreeMap<String, String>,
    #[serde(default, rename = "inputBindings")]
    pub(super) input_bindings: BindingMap,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RustConfig {
    pub manifest: PathBuf,
}
