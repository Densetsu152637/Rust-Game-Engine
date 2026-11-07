# Desktop runtime packages

> Packaging is implemented by `engine-tooling` (`packaging::package`, with
> `artifacts::package_project` as the directory-format shortcut) and tested by its
> `tests/artifacts.rs` and `tests/runtime_pipeline.rs`.

`engine project package <manifest> <destination> [--profile release|dev] [--format dir|zip|tar.gz|appimage|app] [--bundle-id reverse.dns]`
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
| `app` | a new `<Name>.app` directory | macOS application bundle of the layout below, universal and signed (macOS host only; see [macOS app bundle](#macos-app-bundle)) |

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

## macOS app bundle

`--format app` arranges the same content as a macOS application bundle. The
destination is the new `<Name>.app` directory: it must end in `.app` (matched
case-insensitively, with a name before it) or it is refused with
`tooling.destination.invalid`, and an existing path is refused with
`tooling.destination.exists`, as for the other formats. The result key is `app`; the
result also reports `bundle_id`, `targets`, `signing` (`ad-hoc` or `identity`),
`generation`/`fingerprint` (the arm64 build, the one recorded in `generation.json`) and
`x86_64_generation`; `launchers` is empty.

```text
Name.app/
  Contents/
    Info.plist
    PkgInfo                 APPL????
    MacOS/runtime           universal (arm64 + x86_64) executable, 0755
    Resources/
      game/                 project content (project.json at top)
      licenses/             engine-LICENSE, runtime-Cargo.lock, notices, dependencies.json
      generation.json       the arm64 generation
      package-host.json     platform macos, architecture universal
      PACKAGE.txt
      <id>.icns             default engine icon
    Frameworks/             empty; nested code (such as the Steam dylib) goes here
```

There is no `run.cmd` or `run.sh`: the bundle starts from Finder or by running
`Contents/MacOS/runtime`, which resolves `../Resources/game/project.json` from its own
location (see the lookup rules above), so no project argument is needed. The staging
code is the same function as for the other formats with a different layout; nothing
is copied that the directory package would not contain.

`Info.plist` keys: `CFBundleExecutable` (`runtime`), `CFBundleIdentifier`,
`CFBundleName` (the project name cut to 15 characters, Apple's limit for the short
name), `CFBundleDisplayName` (the project name), `CFBundleShortVersionString` `0.1.0`
and `CFBundleVersion` `1` (the manifest has no version field yet), `CFBundlePackageType`
`APPL`, `CFBundleIconFile` (`<id>.icns`), `LSMinimumSystemVersion` `11.0`,
`NSHighResolutionCapable`, `LSApplicationCategoryType` `public.app-category.games`, and
`CFBundleDevelopmentRegion`, `CFBundleInfoDictionaryVersion` and `NSPrincipalClass`.
All values are XML-escaped. `<id>` is the project name reduced as for the AppImage
(`First Project` is `first-project`). `LSMinimumSystemVersion` is the higher of the
deployment targets of the pinned toolchain (`rustc --print deployment-target`, unstable,
reports 11.0 for `aarch64-apple-darwin` and 10.12 for `x86_64-apple-darwin` on Rust
1.99.1) so one value is true for both slices; setting `MACOSX_DEPLOYMENT_TARGET`
changes the binaries but not this key.

The default bundle identifier is `local.engine.<id>`, which claims no real domain.
Pass `--bundle-id <reverse.dns>` (for example `com.studio.game`) for anything shipped.
It must have at most 255 characters and two or more dot-separated components, each
made of ASCII letters, digits and `-` and neither starting nor ending in `-`;
anything else is refused with `tooling.app.bundle_id_invalid`, before the game is
compiled. `--bundle-id` with another format is a usage error.

The icon is the default engine icon (the manifest has no icon field) as an Apple Icon
Image file: the `icns` magic and total length, then one `ic08` entry (256x256 PNG; the
type table is on the "Apple Icon Image format" Wikipedia page) holding the PNG
unchanged, with big-endian lengths that include their 8-byte headers.

### Universal build

Packaging builds the generation twice, once per Rust target, and joins the two
executables with `lipo -create -output Contents/MacOS/runtime`. Both targets must be
installed before packaging:

```text
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

The build runs on a macOS host only. Before anything is compiled, packaging fails with
`tooling.app.unsupported_host` (not macOS, for example Windows or Linux),
`tooling.app.tool_missing` (`lipo`, `xattr` or `codesign` not on `PATH`),
`tooling.app.target_missing` (a Rust target is not installed; the message names the
`rustup target add` command) or `tooling.app.toolchain_failed` (`rustc` could not be
started to look for the targets). `tooling.app.lipo_failed` reports a `lipo` that could not
start, exited non-zero or wrote no file. The two builds go through the normal native
generation cache (a different fingerprint per target, sharing the usual native target
directory), so a second package run is fast. A failure at any step publishes nothing.

### Signing

The finished bundle is always signed. First every extended attribute is removed from
the staged bundle with `xattr -cr <bundle>` (Finder metadata or resource forks copied
from the project would make `codesign` fail with "detritus not allowed"; failure is
`tooling.app.xattr_failed`). Nested code under `Contents/Frameworks` is signed
first, one item at a time in name order, then the bundle; `--deep` is not used to sign.

| Variable | Meaning |
| --- | --- |
| `ENGINE_MACOS_SIGN_IDENTITY` | Optional signing identity (a `Developer ID Application: ...` certificate name or SHA-1 hash that is in the keychain). Unset or empty means an ad-hoc signature. The value is not stored in the bundle or written to the result. |

- Unset: `codesign --force --sign - --timestamp=none <path>` (ad-hoc, no certificate
  needed; this is what CI runs).
- Set: `codesign --force --sign <identity> --options runtime --timestamp <path>` (hardened
  runtime and a secure timestamp, as notarisation requires). No entitlements file is
  passed.
- `tooling.app.codesign_failed` reports a `codesign` that could not start or failed.

The bundle is staged in a `.engine-export-*` directory next to the destination under its
final `<Name>.app` name (`codesign` only treats a `*.app` directory as a bundle),
signed there, and then renamed into place. The destination is refused if it exists,
checked before the build starts and again just before the rename; the rename itself
is not atomic against another process creating an empty directory at the destination
in that instant (POSIX `rename` replaces an empty directory), so the export boundary
is for trusted directories, as for the other formats.

**Gatekeeper.** An ad-hoc signed bundle runs on the machine that built it and runs
anywhere it was not downloaded. A copy that arrives through a browser, chat or other
download carries the quarantine attribute and is blocked as from an unidentified
developer until the user approves it in System Settings, Privacy and Security (or
removes the attribute with `xattr -dr com.apple.quarantine <bundle>`). Distribution
without that prompt needs a Developer ID signature and notarisation.

**Notarisation (documented only, not run or verified).** After signing with a Developer
ID identity, a release job would run, with Apple credentials supplied from secrets:

```text
ditto -c -k --keepParent Name.app Name.zip
xcrun notarytool submit Name.zip --wait --keychain-profile <profile>
xcrun stapler staple Name.app
```

`engine` does not run these steps and no part of them has been executed: they stay
unverified until a signing identity and Apple credentials exist. Archiving a bundle also
needs `ditto` (not `zip`), which keeps the extended attributes and symbolic links a
signature depends on.

Notes for the Steam dylib: it belongs in `Contents/Frameworks/`. Packaging signs every
item there with `--force` and the bundle's own identity, replacing the vendor signature,
so the hardened runtime's library validation (same team as the executable) passes
without the `com.apple.security.cs.disable-library-validation` entitlement. That
entitlement would only be needed if a later step kept the library's original signature.

Limits of the bundle: it is verified only on macOS CI (job "App bundle (macOS)"); the
file generators, layout, identifier rules and tool error handling are unit tested on
every OS with fake tools.

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
(job "Workspace tests (Linux)"), while the macOS bundle is verified only by CI. The AppImage
layout, `AppRun`, `.desktop` and icon generation and the tool error codes are unit
tested on every OS; the real AppImage (`appimagetool` run, then the file started with
`--appimage-extract-and-run --headless --ticks 3`, `--appimage-extract` and
`desktop-file-validate`) is built and checked only by the CI job "AppImage (Linux)". The
`.app` layout, `Info.plist`, `.icns`, bundle identifier rules and tool error codes are
unit tested on every OS (the `lipo`/`codesign` calls with fake tools on Unix); the real
universal build, `plutil -lint`, `lipo -archs`, the resolve-only and headless launches,
`codesign --verify` and the Rosetta run of the x86_64 slice happen only in the CI job
"App bundle (macOS)".
