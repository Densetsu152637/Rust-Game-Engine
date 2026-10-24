#!/usr/bin/env bash
# Compile standalone native entry points on the current desktop host.
set -euo pipefail

cd "$(dirname "$0")/../.."
# Reuse dependencies across standalone crates; the workspace cache includes this
# directory. Respect explicit caller isolation (for example a worker worktree).
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/desktop-examples}"
for crate in examples/desktop-2d examples/first-project examples/native-extension; do
    cargo check --manifest-path "$crate/Cargo.toml" --all-targets
done
cargo check --manifest-path examples/programmable-world/Cargo.toml --all-targets --features desktop
