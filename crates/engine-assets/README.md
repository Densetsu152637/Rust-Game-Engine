# engine-assets

A GPU-free glTF 2.0 importer and runtime asset store. This crate owns decoded
content, not authored project IDs: `engine-project` manifests remain the source
of stable identity.

```rust,no_run
use std::path::Path;
use engine_assets::{import_gltf, ImportLimits, AssetStore, LoadedAsset};

let scene = import_gltf(Path::new("game"), Path::new("assets/model.glb"), &ImportLimits::default())?;
let mut store = AssetStore::default();
let fingerprint = scene.fingerprint;
let handle = store.load(LoadedAsset { value: scene, fingerprint });
assert!(store.get(handle).is_some());
# Ok::<(), engine_assets::AssetError>(())
```

The neutral scene retains hierarchy, triangle geometry, PBR material and texture
associations, PNG pixels, skins, and TRS animation channels. It does not create ECS
entities or GPU resources. Runtime adapters decide how to render or animate the
data; cubic channels must be handled explicitly by adapters.

See [the asset pipeline](../../docs/asset-pipeline.md) for supported formats,
limits, dependency fingerprints, and reload semantics.

Validation: `cargo test -p engine-assets`, `cargo fmt -p engine-assets --check`,
and `cargo clippy -p engine-assets --all-targets -- -D warnings`.

Project authoring integration is available through
`engine_project::model_import::import_model`, the CLI's `project import-model`,
and the write-gated MCP `asset_import_model` tool. The importer writes a
versioned neutral model asset alongside renderable prefab/mesh/PNG and supported
animation assets; see the pipeline document for transactional guarantees and
explicit rendering limitations.
