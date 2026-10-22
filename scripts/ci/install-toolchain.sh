#!/usr/bin/env bash
# Install the Rust toolchain pinned by rust-toolchain.toml (single source of truth).
set -euo pipefail

cd "$(dirname "$0")/../.."
channel=$(sed -nE 's/^channel[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' rust-toolchain.toml | head -n 1)
if [ -z "$channel" ]; then
    echo "error: no channel found in rust-toolchain.toml" >&2
    exit 1
fi
rustup toolchain install "$channel" --profile minimal --component rustfmt --component clippy
rustup default "$channel"
rustc --version --verbose
cargo --version
