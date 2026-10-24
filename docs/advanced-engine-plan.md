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
   mock is not evidence of an executing DLSS feature.

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
