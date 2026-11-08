#!/usr/bin/env bash
# Make an untracked copy of a project with a `steam` manifest block, for the Steam-variant
# release jobs. The repository's projects stay as they are and the copy is never committed.
#
# usage: prepare-steam-project.sh <project-dir> [app-id]
#   <project-dir> holds project.json. app-id defaults to 480 (Valve's Spacewar test app).
#   Prints the copy's manifest path on stdout.
#
# The copy is a hidden sibling, <parent>/.steam-<name>, not a directory under the
# runner's temp area: a gameplay crate commonly depends on the engine by a relative
# path (examples/first-project uses ../../crates/engine), and a sibling keeps every
# such path valid. Any previous copy is replaced. `.derived` and `target` are not copied.
# The block replaces an existing `steam` key and keeps the other keys and their order.
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: prepare-steam-project.sh <project-dir> [app-id]" >&2
  exit 2
fi
source_dir=$(cd "$1" && pwd)
app_id=${2:-480}
case "$app_id" in
  '' | *[!0-9]* | 0*) echo "app-id must be a positive number" >&2; exit 2 ;;
esac
if [ ! -f "$source_dir/project.json" ]; then
  echo "no project.json in $source_dir" >&2
  exit 2
fi
if command -v python3 >/dev/null 2>&1; then
  python=python3
else
  python=python
fi

copy="$(dirname "$source_dir")/.steam-$(basename "$source_dir")"
rm -rf "$copy"
mkdir "$copy"
(cd "$source_dir" && tar -cf - --exclude=./.derived --exclude=./target --exclude=./.git .) | (cd "$copy" && tar -xf -)
"$python" -I - "$copy/project.json" "$app_id" <<'PY'
import json
import sys

path, app_id = sys.argv[1], int(sys.argv[2])
with open(path, encoding="utf-8") as handle:
    manifest = json.load(handle)
manifest["steam"] = {"app_id": app_id}
with open(path, "w", encoding="utf-8", newline="\n") as handle:
    json.dump(manifest, handle, indent=2)
    handle.write("\n")
PY
printf '%s\n' "$copy/project.json"
