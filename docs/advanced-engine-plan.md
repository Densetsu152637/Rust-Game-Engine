# Advanced engine implementation plan

User scope (2026-10-24): implement PBR/shadows, NavMesh and secure Internet
multiplayer in parallel worktrees, retaining Cargo builds, MCP programmability
and native callbacks. The user's subsequent clarification limits platform work
to Windows, Linux and macOS; mobile, browser and console hosts are out of scope.

## Acceptance criteria

1. Authored metallic/roughness PBR materials and configurable shadows produce
   verified GPU pixels while 2D and custom WGSL paths retain their contracts.
2. Triangle geometry can produce a validated navigable mesh, and bounded queries
   return traversable paths around obstacles with agent-clearance handling.
   Runtime agents and MCP use the same validated navigation APIs and events.
3. Encrypted multiplayer authenticates endpoints/participants before accepting
   gameplay input, preserves authority checks, bounds untrusted work, rejects
   invalid credentials and never silently falls back to plaintext. Native policy
   callbacks and MCP-visible nonsecret status/configuration remain available.
4. Windows, Linux and macOS retain usable Cargo host/build paths. Existing and
   introduced platform failures are fixed and validated on available runners;
   unavailable hardware checks remain explicitly unverified.
5. Integrated workspace/examples pass formatting, lint and behavior tests.
   Documentation and runnable examples describe the implemented boundaries.
6. User-requested DLSS support is integrated against available official NVIDIA
   interfaces, with runtime capability checks, explicit unsupported-feature errors,
   configurable MCP/native controls and a non-NVIDIA rendering path. DLSS 5
   availability and SDK access must be verified; a declared option or injected
   mock is not evidence of an executing DLSS feature. After SDK inspection the
   user confirmed they have no DLSS 5 neural-rendering SDK access and requested
   available features instead. Neural rendering is therefore deferred. Super
   Resolution, Frame Generation and Ray Reconstruction are assessed separately
   against their public interfaces and required renderer inputs; unsupported
   features must report their concrete prerequisites.

## Ownership and integration

Starting revision: `dbfef40eee342cb0ad390fe19a2e7c2e0458ff1d`, clean `main`.
Each writer uses its own Git worktree. Root owns shared workspace dependency
reconciliation, the final lockfile, global documentation and integrated acceptance.
Workers own their crate manifests and feature documents; root resolves any shared
schema/API changes before integration. No worker writes another checkout.

| Workstream | Owned area | Expected evidence |
| --- | --- | --- |
| PBR/shadows | Scene materials/lights, render backend, import mappings, rendering docs | GPU readback, material/schema/import regression tests |
| NavMesh | Navigation library, authored navigation runtime, navigation MCP tool module | Mesh bake/path/clearance tests and actual runtime/MCP behavior |
| Secure networking | Network transport/authentication and project network adapter | Real encrypted loopback and adversarial authentication/authority/bounds tests |
| Desktop portability | Windows/Linux/macOS hosts/platform reports, CI, packaging | Relevant local checks and hosted build/test results |
| DLSS | Official SDK/API feasibility and optional native rendering adapter | Verified feature/runtime availability and GPU execution when SDK access permits |

The work establishes concrete tested capabilities. It does not imply DDoS
resistance, platform certification, perfect photorealism, or unmeasured production
scale. Security controls and limitations require an independent scoped review.

## Status (2026-10-24)

| Workstream | State | Detail |
| --- | --- | --- |
| PBR/shadows | Implemented | Metallic-roughness materials, normal/metallic-roughness textures and a directional shadow map: [rendering](rendering3d.md), `examples/pbr-shadows` |
| NavMesh | Implemented | Layered polygon bake, bounded queries, `NavigationAgent` runtime mode and `navigation_bake`/`navigation_query` MCP tools: [navigation](navigation.md) |
| Secure networking | Implemented | Mutual-certificate TLS 1.3 with native authorization policy, plaintext APIs retained for trusted LANs: [networking](networking.md) |
| Desktop portability | Implemented, hosted results unverified | macOS data-alias fix and `scripts/ci/check-desktop-examples.sh` on Linux/macOS jobs; hosted runner results must be recorded separately |
| Spatial audio and acoustics | Implemented; GPU claims limited to what the RTX 5080 tests measured | Binaural/reverb/Doppler DSP, CPU and GPU (ray query, compute) propagation with fallback, 2D proxy world, authored components, `acoustics_*` MCP tools and `--acoustics-backend`: [acoustics](acoustics.md), [design record](spatial-audio-design.md), `examples/acoustics-showcase` |
| DLSS | Super Resolution crate only | Optional standalone `engine-dlss` crate ([DLSS](dlss.md)); Frame Generation and Ray Reconstruction are not implemented and DLSS 5 neural rendering is deferred (no SDK access) |

Known remaining boundaries: no image-based lighting, cascaded or point/spot
shadows, crowd avoidance, prediction/rollback, matchmaking or NAT traversal.
