# Acoustics showcase

A data project (no Rust crate) with two scenes that exercise the spatial audio and
ray-traced acoustics of [docs/acoustics.md](../../docs/acoustics.md):

- `room.scene.json` (startup): a 8 x 3 x 6 m plaster room with a 2 m wide concrete divider
  between the listener (`x = -3`) and a looping speaker (`x = 3`). The acoustic
  world comes from the two `MeshRenderer` meshes. The listener hears the speaker
  occluded by the divider, with early reflections and a traced late reverb.
- `side.scene.json`: the same idea as a 2D side-view game. Five `Collider2D` boxes
  form a 10 x 6 unit room with a divider, and `proxy2d` (1 unit per metre, 6 m
  deep) turns them into a 3D proxy world in the background.

Both scenes use `backend: "auto"`: the engine tries hardware ray query, then GPU
compute, then the CPU, and the run report says which tier is active and why.

Run from the repository root (`engine` stands for
`cargo run -p engine-tooling --bin engine --`):

```powershell
engine project validate examples/acoustics-showcase/project.json --no-rust
engine run examples/acoustics-showcase/project.json --headless --ticks 120 --acoustics-backend cpu
engine mcp examples/acoustics-showcase    # acoustics_inspect / acoustics_query / acoustics_bake
```

Ask a running MCP server about the room, for example:

```json
{"name":"acoustics_query","arguments":{"scene":"room.scene.json",
 "listener":{"position":[-3,1.6,0]},"sources":[{"position":[3,1.2,0]}]}}
```

The speaker's `visible_fraction` is below 1 and its high bands are far quieter than
its low bands because the divider transmits less at high frequency. Moving the
speaker to `z = 2.5` (past the divider's edge) clears the line of sight.

`hum.wav` is a generated 200 Hz tone (8 kHz, 16-bit, 0.25 s, looped); the meshes
are procedural project-owned data with no external assets.
