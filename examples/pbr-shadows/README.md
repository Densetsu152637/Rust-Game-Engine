# PBR and directional shadows

Run from the repository root:

```powershell
cargo run -p engine-desktop --example pbr_shadows -- pbr-shadows.png
cargo run -p engine-desktop --example pbr_shadows -- --window
```

The same authored scene can be opened and changed through the project/MCP tools.
Three smooth-normal spheres compare metallic gold, rough copper and a blue
dielectric. A directional depth map casts their shadows onto the floor. Edit
`scene.json` material factors and `Camera3D.lighting.shadows` to compare results.
The sphere/floor meshes are procedural project-owned data with no external assets.
See [rendering contracts and limitations](../../docs/rendering3d.md).
