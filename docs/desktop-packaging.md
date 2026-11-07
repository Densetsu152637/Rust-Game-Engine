# Desktop runtime packages

> Packaging is implemented by `engine-tooling` (`packaging::package`, with
> `artifacts::package_project` as the directory-format shortcut) and tested by its
> `tests/artifacts.rs` and `tests/runtime_pipeline.rs`.

`engine project package <manifest> <destination> [--profile release|dev] [--format dir|zip|tar.gz|appimage]`
compiles the project's Rust gameplay crate with the engine into a native generation
and exports a self-contained package that needs neither Cargo nor the source
checkout to run.

## Formats

`--format` selects how the package is delivered; `--format json` remains the global
output option and is not a package format.

| Format | `<destination>` | Result |
| --- | --- | --- |
| `dir` (default) | a new directory | the layout below, unchanged |
| `zip` | a new archive file | deflate zip with one top-level folder |
| `tar.gz` | a new archive file | gzip-compressed tar with one top-level folder |
| `appimage` | a new `.AppImage` file | Linux AppImage of the layout below (Linux host with `appimagetool` only; see [AppImage](#appimage)) |

The destination is the path of the thing created and must not exist (an existing
file or directory is refused with `tooling.destination.exists`, and a failed export
publishes nothing). An archive's top-level folder is the destination file name
without its `.zip` or `.tar.gz` suffix: `dist/mygame-1.0.tar.gz` unpacks to
`mygame-1.0/`, holding the layout below; a name that would leave `.` or `..` as
that folder (such as `...zip`) is refused with `tooling.destination.invalid`. Archives are built from the same staged
directory, so every format has identical content. Inside an archive directories and
the `runtime[.exe]` and `run.sh` files are mode 0755 and all other files 0644; the
mode is written into both formats (zip entries are marked as created on Unix), so
`tar -xzf` and `unzip` on Linux and macOS keep the executable bit even for an
archive built on Windows. Entries are sorted by path with a fixed 2020-01-01 UTC
modification time and no owner, so packaging the same input twice gives the same
archive bytes. `PACKAGE.txt` names the archive format.

```text
engine project package game/project.json dist/mygame.tar.gz --format tar.gz
tar -xzf dist/mygame.tar.gz -C /opt
/opt/mygame/runtime --headless --ticks 3      # finds game/project.json beside itself
/opt/mygame/run.sh --headless --ticks 3
```

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

With an archive format the same tree sits inside the archive's top-level folder.

- **Runtime layout lookup.** Started with no project argument (or with only
  options), the runtime loads `game/project.json` beside its executable, then,
  when the executable is `<X>.app/Contents/MacOS/<exe>`, `../Resources/game/project.json`
  (macOS app-bundle layout). A project argument as the first argument keeps its
  current meaning. With neither layout present the run fails with the usual
  `command.arguments` diagnostic naming the searched paths. Setting
  `ENGINE_RESOLVE_ONLY=1` makes the runtime print the resolved absolute
  `project.json` path to stdout and exit 0 without a window, GPU or Steam session,
  or exit 3 with a message on stderr when nothing resolves. FFmpeg is not bundled; a
  host that builds its video tools with `DecoderTools::discover()` searches an
  optional `tools/` folder next to the executable (or the bundle's
  `Resources/tools`) before `PATH`. The runtime does not do this itself; see
  [video.md](video.md).
- **Launching.** The runtime can be started directly with no arguments (or only
  options such as `--headless --ticks 3`): it loads the `game/project.json` beside
  itself. `run.cmd` and `run.sh` remain the documented launchers.
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

## AppImage

`--format appimage` packs the same staged layout into one executable
`<destination>` file (name it `*.AppImage`; any new file name is accepted). The
staged package becomes an AppDir with these files added at its root:

```text
AppRun               POSIX sh, 0755: resolves its own directory with readlink -f on $0, then exec "$HERE/runtime" "$@"
<id>.desktop         Type=Application, Name=<project name>, Exec=runtime, Icon=<id>, Categories=Game;, Terminal=false
<id>.png, .DirIcon   the default engine icon (256x256 PNG; the manifest has no icon field yet)
```

`<id>` is the project name reduced to lowercase ASCII letters and digits, each other
run of characters (including non-ASCII ones) becoming one `-`, with leading and
trailing `-` dropped and at most 64 characters; a name with no ASCII letter or digit
gives `game` (`First Project` is `first-project`). The runtime finds `game/project.json`
beside itself (see the lookup rules above), so `AppRun` passes no project argument.
`AppRun` never reads an inherited `APPDIR` (a nested AppImage or stale variable could
point at another image); it always locates itself from `$0`.

Building needs a Linux host and
[`appimagetool`](https://github.com/AppImage/appimagetool/releases):

| Variable | Meaning |
| --- | --- |
| `ENGINE_APPIMAGETOOL` | Path of the `appimagetool` executable. When set and non-empty it is the only candidate. Otherwise `appimagetool` is searched on `PATH`. |
| `ENGINE_APPIMAGE_RUNTIME` | Optional path of a pinned [type2 runtime](https://github.com/AppImage/type2-runtime/releases), passed as `--runtime-file` so the tool never downloads a runtime while packaging. |

`scripts/package/fetch-appimagetool.sh <dest-dir>` downloads the pinned `appimagetool` 1.9.1 and type2
runtime 20251108 (x86_64), verifies their SHA-256 checksums and prints the two paths;
CI and later release workflows use it so the pins live in one place.

Packaging fails before anything is staged, and before the game is compiled, with
`tooling.appimage.unsupported_host` (not a Linux host, for example Windows or macOS),
`tooling.appimage.tool_missing` (no executable `appimagetool`) or
`tooling.appimage.runtime_missing` (`ENGINE_APPIMAGE_RUNTIME` is not a file).
`tooling.appimage.failed` reports a tool that could not start, exited non-zero or wrote
no file. The tool runs with `APPIMAGE_EXTRACT_AND_RUN=1` (it needs no FUSE) and
`--no-appstream`. `ARCH` is always set from the host architecture (an inherited
`ARCH` is ignored), and the CI variables `appimagetool` 1.9.1 reads to guess update
information (`GITHUB_REPOSITORY`, `GITHUB_REPOSITORY_OWNER`, `GITHUB_TOKEN`,
`TRAVIS_*`, `CI_PROJECT_URL`, `CI_COMMIT_REF_NAME`, `CI_JOB_NAME`) and `OWD` are
removed from its environment, so no update information or `.zsync` file is
produced. The tool is given a not-yet-existing output path in a staging directory
next to the destination, and the result is published with
no-clobber, so an existing destination is refused with `tooling.destination.exists`
and a failure publishes nothing. The result key is `appimage` (not `archive`).

Limits of an AppImage package:

- **Host libraries.** The AppImage bundles only the runtime and the game. Vulkan
  (`libvulkan`), ALSA, X11 and Wayland libraries come from the host; `libvulkan` is
  deliberately not bundled because it must match the host GPU driver.
- **glibc.** The runtime links the glibc of the machine that built it, so it runs on
  that glibc or newer. CI builds on `ubuntu-22.04`, the oldest supported baseline.
- **Read-only mount.** An AppImage is a read-only squashfs mounted at run time, so the
  game must not write beside the executable. Player saves and settings already go to
  the user-data root (see Player data), never into the package.
- **FUSE.** Running the file needs FUSE 2, or `--appimage-extract-and-run`
  (`APPIMAGE_EXTRACT_AND_RUN=1`) where it is missing, as on CI runners.
- **Not provided.** Signing, update information and bundled shared libraries.

## Steam variant

A package built by a tooling binary with `--features steam` links the real Steam backend,
and then needs `steam_api64.dll` (Windows) next to `runtime.exe` and no `steam_appid.txt`.
See [Steam](steam.md#shipping-the-steam-variant-on-windows). Depot layout is not packaged yet.

## Verification status

Package tests build and run real external fixture crates, and launch the packaged
runtime and `run.cmd` headlessly from an unrelated working directory. Packaging
`examples/first-project` with the default release profile on Windows produced the
layout above, and `run.cmd --headless --ticks 3` run from another directory printed
an `ok` run result and returned the runtime's exit status (`2` for invalid
arguments). A packaged run with a real window, GPU playback and audio is a separate
check that headless launches do not establish. Windows is the validated packaging
platform; the Linux `tar.gz` round trip (package, extract with `tar`, run the runtime
with no arguments and `run.sh`, both `--headless --ticks 3`) is exercised in CI
(job "Workspace tests (Linux)"), while macOS packaging is unverified. The AppImage
layout, `AppRun`, `.desktop` and icon generation and the tool error codes are unit
tested on every OS; the real AppImage (`appimagetool` run, then the file started with
`--appimage-extract-and-run --headless --ticks 3`, `--appimage-extract` and
`desktop-file-validate`) is built and checked only by the CI job "AppImage (Linux)".
