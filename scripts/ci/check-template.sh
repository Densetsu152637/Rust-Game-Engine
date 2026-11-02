#!/usr/bin/env bash
# Compile the whole game template (templates/game, including src/main.rs) against
# this checkout. The template pins `engine` by a release tag that may not exist yet,
# and Cargo fetches a [patch]ed git source anyway, so a temporary copy depends on the
# checkout by path instead. Nothing in templates/game is modified.
set -euo pipefail

cd "$(dirname "$0")/../.."
root="$(pwd)"
if command -v cygpath >/dev/null 2>&1; then
    root="$(cygpath -m "$root")"
fi
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/template}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp -R templates/game/. "$work/"
# Seed dependency versions from the workspace lockfile.
cp Cargo.lock "$work/Cargo.lock"
sed -i -E "s|^engine = \{ git = .*\$|engine = { path = \"$root/crates/engine\" }|" "$work/Cargo.toml"
if ! grep -q "^engine = { path = " "$work/Cargo.toml"; then
    echo "templates/game/Cargo.toml: engine dependency line not found" >&2
    exit 1
fi
cargo check --manifest-path "$work/Cargo.toml" --all-targets
