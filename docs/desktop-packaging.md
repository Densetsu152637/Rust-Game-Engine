# Desktop runtime packages

> Packaging is implemented by `engine-tooling` (`artifacts::package_project`) and
> tested by its `tests/artifacts.rs` and `tests/runtime_pipeline.rs`.

`engine project package <manifest> <new-directory> [--profile release|dev]`
compiles the project's Rust gameplay crate with the engine into a native generation
and exports a self-contained directory that needs neither Cargo nor the source
checkout to run.

```text
my-package/
  runtime[.exe]           compiled engine + game executable
  game/                   project content copied from the project root (project.json at top)
  licenses/               engine-LICENSE, runtime-Cargo.lock, per-dependency notices,
                          dependencies.json inventory
  generation.json         generation number, fingerprint and build metadata
  run.cmd, run.sh         launchers
  PACKAGE.txt             generation, fingerprint and launch notes
```

- **Launchers.** `run.cmd` and `run.sh` derive the package directory from their own
  location and pass `game/project.json` to the runtime, forwarding caller
  arguments, so `run.cmd --headless --ticks 120` works from any working directory
  and preserves the runtime's exit status.
- **Shaders.** Shaders are WGSL assets validated during `project shaders` and
  compiled by the GPU driver at run time; there are no precompiled shader binaries to
  package. Built-in engine shaders are compiled into the executable.
- **Licenses.** The engine `LICENSE` must exist in the engine checkout or packaging
  fails. The resolved `Cargo.lock` of the generation is included, and for every
  dependency in the build's Cargo metadata the license and notice files found at its
  package root are copied under `licenses/<name>-<version>/` and listed (with the
  declared license expression and whether the package is local) in
  `licenses/dependencies.json`. Projects remain responsible for their own content
  and asset licenses.
- **Player data.** Saves and settings live in the user-data profile, never in the
  package; `--user-data <dir>` selects another root. See
  [Audio and persistence](desktop-services.md).
- **Safety.** The destination must not exist and must not overlap authored source
  (generated `build`, `dist`, `release` or `.derived` directories under the project
  are allowed). Content is copied into a staging directory in the destination's
  parent and renamed into place, so a failed export publishes nothing. Source
  symlinks are not followed and cause the export to fail. The export boundary is for
  trusted host directories, not a sandbox against another process racing file system
  entries.

## Verification status

Package tests build and run real external fixture crates, and launch the packaged
runtime and `run.cmd` headlessly from an unrelated working directory. Packaging
`examples/first-project` with the default release profile on Windows produced the
layout above, and `run.cmd --headless --ticks 3` run from another directory printed
an `ok` run result and returned the runtime's exit status (`2` for invalid
arguments). A packaged run with a real window, GPU playback and audio is a separate
check that headless launches do not establish. Windows is the validated packaging
platform; `run.sh` and Unix packaging are unverified.
