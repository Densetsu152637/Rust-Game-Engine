use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SceneEntity {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
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
    pub component_catalog: crate::catalog::ComponentCatalog,
    /// Host functions graph scripts may call, from the `scriptFunctions` file.
    pub script_functions: Vec<crate::scripting::ScriptFunctionDecl>,
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
    /// Optional Steam settings; plain data, parsed whether or not Steam is compiled in.
    pub steam: Option<SteamManifest>,
    /// Extra read-only content roots (mods); empty unless a host registers some. Shared by
    /// every clone of the project.
    pub content_roots: crate::content_roots::ContentRoots,
}
/// The `steam` manifest block. Unknown keys are rejected.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SteamManifest {
    /// Steam App ID, non-zero.
    pub app_id: u32,
    /// Whether Steam Cloud save sync is wanted (used by a later stage).
    #[serde(default)]
    pub cloud: bool,
    /// When true, the game exits with code 4 if Steam is not available.
    #[serde(default)]
    pub required: bool,
    /// Rich presence defaults set once Steam is ready (at most 20 keys; keys up to
    /// 64 bytes, values up to 256 bytes, no NUL).
    #[serde(default)]
    pub rich_presence: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    #[serde(default, rename = "componentSchemas")]
    pub(super) component_schemas: Option<PathBuf>,
    #[serde(default, rename = "scriptFunctions")]
    pub(super) script_functions: Option<PathBuf>,
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
    #[serde(default)]
    pub(super) steam: Option<SteamManifest>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RustConfig {
    pub manifest: PathBuf,
}
