#!/usr/bin/env bash
# Smoke-test a packaged .zip or .tar.gz the way a user receives it: extract it into a
# fresh directory, check the layout (and the exec bits on Unix) and start the runtime
# and the launcher with no project argument from an unrelated working directory.
#
# usage: smoke-archive.sh <archive> <extract-dir>
#   <extract-dir> must not exist; it is created and left in place for the caller.
# Unix hosts check `runtime` and `run.sh`; Windows hosts (Git bash) check
# `runtime.exe` and `run.cmd` and extract zips with PowerShell's Expand-Archive.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=scripts/package/smoke-lib.sh
. "$here/smoke-lib.sh"

if [ "$#" -ne 2 ]; then
  echo "usage: smoke-archive.sh <archive> <extract-dir>" >&2
  exit 2
fi
archive=$1
extract=$2
if [ ! -f "$archive" ]; then
  echo "archive not found: $archive" >&2
  exit 2
fi
if [ -e "$extract" ]; then
  echo "extract directory must not exist: $extract" >&2
  exit 2
fi
archive=$(cd "$(dirname "$archive")" && pwd)/$(basename "$archive")
mkdir -p "$extract"
extract=$(cd "$extract" && pwd)
package=""

# top_level: print the single top-level folder of the extraction, or fail.
top_level() {
  local entries
  entries=$(ls -A "$extract")
  if [ -z "$entries" ] || [ "$(printf '%s\n' "$entries" | wc -l)" -ne 1 ] || [ ! -d "$extract/$entries" ]; then
    echo "expected exactly one top-level folder, found:" >&2
    printf '%s\n' "$entries" >&2
    return 1
  fi
  printf '%s' "$extract/$entries"
}

do_extract() {
  case "$archive" in
    *.tar.gz)
      tar -tvzf "$archive" | sed -n 1,12p
      tar -xzf "$archive" -C "$extract"
      ;;
    *.zip)
      extract_zip "$archive" "$extract"
      ;;
    *)
      echo "unsupported archive type: $archive" >&2
      return 1
      ;;
  esac
  # One top-level folder, as documented in docs/desktop-packaging.md.
  ls -la "$(top_level)"
}

check_layout() {
  if is_windows; then
    test -f "$package/runtime.exe"
    test -f "$package/run.cmd"
  else
    test -x "$package/runtime"
    test -x "$package/run.sh"
  fi
  test -f "$package/game/project.json"
  test -f "$package/generation.json"
  test -f "$package/PACKAGE.txt"
  test -d "$package/licenses"
}

run_runtime() {
  cd "$(neutral_dir)"
  if is_windows; then
    "$package/runtime.exe" --headless --ticks 3
  else
    "$package/runtime" --headless --ticks 3
  fi
}

run_launcher() {
  cd "$(neutral_dir)"
  if is_windows; then
    # cmd.exe runs the launcher; //c stops MSYS from rewriting the switch.
    cmd.exe //c "$(cygpath -w "$package/run.cmd")" --headless --ticks 3
  else
    "$package/run.sh" --headless --ticks 3
  fi
}

# Extraction decides the package path, so the later phases only run when it worked.
phase "Extract $(basename "$archive")" do_extract
if package=$(top_level 2>/dev/null); then
  phase "Check the package layout" check_layout
  phase "Run the runtime with no project argument" run_runtime
  phase "Run the launcher" run_launcher
fi
finish
