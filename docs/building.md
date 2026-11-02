# Building quickly on Windows

Measured on an i7-13700KF (24 threads), 64 GB RAM, 7.5 GB pagefile, Windows 11,
rustc 1.99.1 msvc, with other agents building at the same time (3 to 8 rustc and
2 to 6 link processes were running during most runs, so absolute numbers are noisy
and only the large differences are meaningful). Raw logs were kept outside the repo.

## Settings kept in the repo

- `Cargo.toml`: `[profile.dev] debug = "line-tables-only"` and
  `[profile.dev.package."*"] debug = false`. Backtraces keep file and line numbers;
  debugger variable inspection in workspace crates is reduced, and is absent for
  dependencies.
- `.cargo/config.toml`: `linker = "rust-lld"` for `x86_64-pc-windows-msvc`. rust-lld
  ships with the pinned toolchain. All 57 crates' test binaries (including
  CMake-built native dependencies) linked with it, and an ignored GPU suite passed.

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

Cold `cargo build --workspace --tests`, SSD target dir:

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
- sccache 0.19.0: 0 Rust cache hits over 630 compilations in a second cold build in
  a different target directory (90 s against 84 to 93 s without it); workspace
  crates are not cacheable because incremental compilation is on. Some runs also hit
  out-of-memory rustc crashes (other builds were using the commit limit, so the
  cause was not isolated). Not recommended here.
- Jobs above 12: no speedup, and free commit memory drops close to zero.

## Rollback

Remove the two `[profile.dev*]` tables from `Cargo.toml` and/or delete
`.cargo/config.toml`. To use the MSVC linker for one command:
`cargo --config "target.x86_64-pc-windows-msvc.linker='link.exe'" ...`.
