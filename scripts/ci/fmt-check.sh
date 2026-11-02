#!/usr/bin/env bash
# Check formatting of every workspace package.
#
# `cargo fmt --all` passes every source file to rustfmt on one command line, which
# exceeds the Windows limit (os error 206) for this workspace, so check one package
# at a time instead.
set -euo pipefail

cd "$(dirname "$0")/../.."
packages=$(cargo metadata --no-deps --format-version 1 \
    | python -c 'import json, sys; print("\n".join(p["name"] for p in json.load(sys.stdin)["packages"]))' \
    | tr -d '\r')

status=0
for package in $packages; do
    cargo fmt -p "$package" --check || status=1
done
exit "$status"
