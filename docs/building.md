# Building quickly on Windows

Measured on an i7-13700KF (24 threads), 64 GB RAM, 7.5 GB pagefile, Windows 11,
rustc 1.99.1 msvc, with other agents building at the same time (3 to 8 rustc and
2 to 6 link processes were running during most runs, so absolute numbers are noisy
and only the large differences are meaningful). Raw logs were kept outside the repo.

## Settings kept in the repo

- `Cargo.toml`: `[profile.dev] debug = "line-tables-only"` and
  `[profile.dev.package."*"] debug = false`. Backtraces keep file and line numbers
  only in workspace crates; dependency frames show function names only.
  Debugger variable inspection is reduced in workspace crates and absent for
  dependencies. The profile settings apply on all OSes but were measured on Windows
  only.
- `.cargo/config.toml`: `linker = "rust-lld"` for `x86_64-pc-windows-msvc`. rust-lld
  ships with the pinned toolchain. All 57 crates' test binaries (including
  CMake-built native dependencies) linked with it, and an ignored GPU suite passed. The DLSS NGX static link (`engine-dlss` feature
  `ngx-vulkan` with `NGX_SDK_DIR`) is unverified under rust-lld because no SDK was
  available; use the override in the rollback section if it fails.

## Machine-local recommendations (not in the repo)

- Put every checkout's `CARGO_TARGET_DIR` on an SSD, for example
  `F:/cargo-targets/<task-id>`. Do not share one target directory across
  worktrees (stale dependency metadata was observed before; see
  `docs/pending-features-plan.md`). On this machine `D:` is a hard disk.
- Cap jobs at about 12 (`[build] jobs = 12` in `~/.cargo/config.toml`). Workspace
  test builds can exhaust the Windows commit limit (paging file 7.5 GB): rustc
  aborts with "memory allocation failed" or "paging file is too small".
- Suggested, not measured: a Windows Defender exclusion for the target
  directories and `~/.cargo`. This is a user decision.

## Results

Cold `cargo build --workspace --tests`, SSD target dir. The 497 s to 195 s gain is
confounded by memory pressure: default runs had about 0.08 GB free commit memory.

| Settings | Jobs | Time | Lowest free commit memory |
|---|---|---|---|
| default | 24 | failed after 141 s (out of memory) | exhausted |
| default | 8 | 499 s | 0.09 GB |
| default | 12 | 497 s | 0.07 GB |
| debug settings | 8 | 255 s | 14.5 GB |
| debug settings | 12 | 195 s | 7.9 GB |
| debug settings | 16 | 197 s | 0.9 GB |
| debug settings | 24 | 206 s | 0.4 GB |
| debug settings + rust-lld | 12 | 183 s | 12.1 GB |

`-p engine-desktop --tests`, jobs 12, SSD target dir. Cold build, then three
rebuilds after appending a comment to `crates/engine-render-pipeline/src/lib.rs`
(seconds):

| Settings | Cold | Incremental rebuilds |
|---|---|---|
| default (A) | 163, 141 | 130, 167, 149 / 158, 163, 141 |
| debug settings (B) | 178, 163 | 17, 17, 17 / 18, 17, 17 |
| debug settings + rust-lld (C, kept) | 86, 84, 93 | 14, 14, 14 / 14, 14, 14 / 13, 14, 14 |

Rounds were interleaved A, B, C, A, B, C. Cold engine-desktop builds are not
faster from the debug settings alone (163 to 178 against 141 to 163, within noise);
the gain there comes from rust-lld and from the full-workspace and incremental cases.

Target directory drive, settings C: SSD cold 84 to 93 s and incremental 14 s;
hard disk (`D:`) cold 393 s and incremental 122 to 150 s.

## Rejected

- Dependencies at `opt-level = 1`: cold 144 s against 84 to 93 s (slower); the
  incremental rebuild was 11.6 s against 14 s. Not kept because the cold build
  regressed.
- `split-debuginfo = "unpacked"`: cold 82 s, incremental 14 s; no change.
- sccache 0.19.0, as first tried: 0 Rust hits across the two cold builds
  (cumulative stats, 630 compilations), second build 90 s against 84 to 93 s without
  it. Weak evidence: those runs had out-of-memory rustc crashes. Retried on a quiet
  machine, see "sccache follow-up".
- Jobs above 12: no speedup, and free commit memory drops close to zero.

## sccache follow-up (opt-in)

sccache hashes `CARGO_*` environment variables, so `CARGO_TARGET_DIR` (and
`CARGO_BUILD_TARGET_DIR`) made every Rust compilation differ between worktrees:
0 hits with the env var, 245 Rust hits with `--target-dir` instead. Two cold
`-p engine-desktop --tests` builds in different SSD target dirs, jobs 12: 87 s
(empty cache), then 71 s (one sample each; an uncached cold build is 84 to 93 s).
The gain is modest because workspace crates (incremental), build scripts and links
are not cached. Opt-in recipe, per shell, not enabled by default:

```text
export RUSTC_WRAPPER=<path>/sccache.exe SCCACHE_DIR=<fast drive>/sccache-cache
unset CARGO_TARGET_DIR
cargo build --target-dir <fast drive>/cargo-targets/<task-id> ...
```

## Other checks

Under the kept settings: `cargo build --release -p engine-desktop` succeeded
(181 s, rc 0). `cargo test --no-run` succeeded for `examples/desktop-2d`,
`first-project`, `native-extension`, `programmable-world` and
`assets/scripts/moving_entity` (these use `.cargo/config.toml` but not the root
`[profile.dev]`). `acoustics-showcase` and `pbr-shadows` contain no Cargo manifest.

## Rollback

Remove the two `[profile.dev*]` tables from `Cargo.toml` and/or delete
`.cargo/config.toml`. To use the MSVC linker for one command:
`CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=link.exe` or
`cargo --config "target.x86_64-pc-windows-msvc.linker='link.exe'" ...`.
