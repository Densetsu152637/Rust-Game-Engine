#!/usr/bin/env bash
# Validate the release workflow inputs and derive the artifact naming parts.
# Run from anywhere; paths are relative to the repository root.
#
# Inputs come only from the environment (the workflow passes them through `env:`):
#   PROJECT    manifest directory, relative to the repository root (default examples/first-project)
#   PROFILE    release or dev (default release)
#   REF_TYPE   github.ref_type (tag, branch, ...)
#   REF_NAME   github.ref_name
#   SHA        github.sha
#
# Prints name=value lines (to $GITHUB_OUTPUT when it is set, else to stdout):
#   project    normalised project directory
#   manifest   <project>/project.json
#   profile    release or dev
#   id         project id: the manifest "name" reduced like the AppImage id (engine-tooling
#              packaging::appimage::sanitize_id): lowercase ASCII letters and digits, each
#              other run of characters one "-", at most 64 characters, "game" if empty
#   version    the tag without a leading "v" on a tag run; "dev-<first 7 characters of SHA>"
#              otherwise. The project manifest has no version field yet (an unknown key is
#              rejected), so there is nothing else to read; when it gains one, take it here.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
project=${PROJECT:-examples/first-project}
profile=${PROFILE:-release}
ref_type=${REF_TYPE:-}
ref_name=${REF_NAME:-}
sha=${SHA:-}

fail() {
  echo "error: $*" >&2
  exit 1
}

case "$profile" in
  release | dev) ;;
  *) fail "profile must be release or dev" ;;
esac

# Reject anything that is not a plain relative path before touching the file system.
[[ "$project" =~ ^[A-Za-z0-9._/-]+$ ]] || fail "project may only contain letters, digits, '.', '_', '-' and '/'"
case "$project" in
  /* | -*) fail "project must be a relative path inside the repository" ;;
esac
project=${project%/}
while [ "${project#./}" != "$project" ]; do project=${project#./}; done
[ -n "$project" ] || fail "project must not be empty"
IFS=/ read -r -a parts <<<"$project"
for part in "${parts[@]}"; do
  case "$part" in
    '' | . | ..) fail "project must not contain empty, '.' or '..' components" ;;
  esac
done
[ -d "$root/$project" ] || fail "project directory not found: $project"
[ -f "$root/$project/project.json" ] || fail "no project.json in $project"
# A symlink inside the repository must not lead out of it.
resolved=$(cd "$root/$project" && pwd -P)
root_real=$(cd "$root" && pwd -P)
case "$resolved/" in
  "$root_real"/*) ;;
  *) fail "project resolves outside the repository" ;;
esac

if command -v python3 >/dev/null 2>&1; then
  python=python3
else
  python=python
fi
id=$("$python" -I - "$root/$project/project.json" <<'PY'
import json
import re
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    name = json.load(handle)["name"]
if not isinstance(name, str):
    sys.exit("project.json: name must be a string")
identifier = re.sub(r"[^A-Za-z0-9]+", "-", name).strip("-").lower()[:64].rstrip("-")
print(identifier or "game")
PY
)

if [ "$ref_type" = tag ]; then
  version=${ref_name#v}
else
  [ -n "$sha" ] || fail "SHA is required outside tag runs"
  version="dev-${sha:0:7}"
fi
[[ "$version" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]] || fail "version '$version' is not usable in a file name (tag names may use letters, digits, '.', '_', '+' and '-')"

emit() {
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    printf '%s\n' "$1" >>"$GITHUB_OUTPUT"
  else
    printf '%s\n' "$1"
  fi
}
emit "project=$project"
emit "manifest=$project/project.json"
emit "profile=$profile"
emit "id=$id"
emit "version=$version"
