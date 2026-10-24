# PBR, lights and shadows

Run from the repository root:

```powershell
cargo run -p engine-desktop --example pbr_shadows -- pbr-shadows.png
cargo run -p engine-desktop --example pbr_shadows -- --window
```

The same authored scene can be opened and changed through the project/MCP tools.
Three smooth-normal spheres compare metallic gold, rough copper and a blue
dielectric. A `DirectionalLight` (two camera-fitted shadow cascades), a warm
`SpotLight` with PCSS soft shadows and a blue `PointLight` light them. Edit
`scene.json` material factors and the light components (see
[rendering lights](../../docs/rendering-lights.md)) to compare results.
The sphere/floor meshes are procedural project-owned data with no external assets.
See [rendering contracts and limitations](../../docs/rendering3d.md).
