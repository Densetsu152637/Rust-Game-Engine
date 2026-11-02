#!/usr/bin/env bash
# Check formatting of every workspace package, one `cargo fmt -p` at a time.
# `cargo fmt --all` hands every crate root to a single rustfmt process, which on
# Windows exceeds the 32k command-line limit once the workspace is large enough
# ("The filename or extension is too long. (os error 206)").
set -euo pipefail

cd "$(dirname "$0")/../.."
cargo metadata --no-deps --format-version 1 --locked \
    | python -c 'import json,sys; print("\n".join(sorted(p["name"] for p in json.load(sys.stdin)["packages"])))' \
    | tr -d '\r' > "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/fmt-packages.txt"
status=0
while IFS= read -r package; do
    cargo fmt --check -p "$package" || status=1
done < "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/fmt-packages.txt"
exit "$status"
