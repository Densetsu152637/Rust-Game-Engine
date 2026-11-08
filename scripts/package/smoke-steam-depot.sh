#!/usr/bin/env bash
# Smoke-test a Steam depot archive the way an uploader receives it: extract it, check
# the SteamPipe layout (content/<platform>/, the two VDF templates, README.txt), check
# that Valve's steam_api library is where the runtime loads it and that no
# steam_appid.txt exists, then run the packaged runtime headless with no Steam client.
# The depot is for the host platform (the one this script runs on).
#
# usage: smoke-steam-depot.sh <depot.zip> <extract-dir> <id> <app-id>
#   <extract-dir> must not exist; it is created and left in place for the caller.
#   <id> is the project id (the .app is named <id>.app on macOS); <app-id> is the
#   Steam app id the depot was built for.
# Environment:
#   SMOKE_EXPECT_STEAM_STATE  comma-separated steam.state values to accept (default
#                             not_running,init_failed: CI runners have no Steam
#                             client installed, which Steam reports as a failed init)
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=scripts/package/smoke-lib.sh
. "$here/smoke-lib.sh"

if [ "$#" -ne 4 ]; then
  echo "usage: smoke-steam-depot.sh <depot.zip> <extract-dir> <id> <app-id>" >&2
  exit 2
fi
archive=$1
extract=$2
id=$3
app_id=$4
expect_state=${SMOKE_EXPECT_STEAM_STATE:-not_running,init_failed}
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
extract=$(cd "$extract" && pwd -P)

if is_windows; then
  platform=windows
  library=steam_api64.dll
elif [ "$(uname -s)" = Darwin ]; then
  platform=macos
  library=libsteam_api.dylib
else
  platform=linux
  library=libsteam_api.so
fi
if command -v python3 >/dev/null 2>&1; then
  python=python3
else
  python=python
fi

depot=""
content=""    # the directory holding the runtime
resources=""  # the directory holding game/ and PACKAGE.txt
runtime=""

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
  extract_zip "$archive" "$extract"
  ls -la "$(top_level)"
}

check_layout() {
  test -d "$depot/content/$platform"
  # Only the host's platform directory: one platform per invocation.
  test "$(find "$depot/content" -mindepth 1 -maxdepth 1 | wc -l)" -eq 1
  test -f "$depot/README.txt"
  test -f "$depot/scripts/app_build_$app_id.vdf"
  grep -F "\"AppID\" \"$app_id\"" "$depot/scripts/app_build_$app_id.vdf"
  local depots=0 vdf
  for vdf in "$depot"/scripts/depot_build_*.vdf; do
    [ -e "$vdf" ] || continue
    depots=$((depots + 1))
    grep -F "\"LocalPath\" \"$platform/*\"" "$vdf"
    grep -F '"FileExclusion" "*.pdb"' "$vdf"
    # The app build script must reference this depot script by name.
    grep -F "\"$(basename "$vdf")\"" "$depot/scripts/app_build_$app_id.vdf"
  done
  test "$depots" -eq 1
  test -f "$resources/game/project.json"
  test -f "$resources/PACKAGE.txt"
  if is_windows; then
    test -f "$runtime"
  else
    test -x "$runtime"
  fi
  # Valve's library, where the runtime loads it from.
  if [ "$platform" = macos ]; then
    test -f "$content/../Frameworks/$library"
    test ! -e "$content/$library"
  else
    test -f "$content/$library"
  fi
  # steam_appid.txt is for development only and never shipped.
  local stray
  stray=$(find "$depot" -iname 'steam_appid.txt')
  if [ -n "$stray" ]; then
    echo "steam_appid.txt must not be packaged: $stray" >&2
    return 1
  fi
  echo "scripts:"
  head -n 40 "$depot"/scripts/*.vdf
}

# How the runtime finds the library, shown and checked without LD_LIBRARY_PATH or
# DYLD_* variables: an $ORIGIN run path on Linux, an @rpath reference plus the
# bundle's Frameworks rpath on macOS. Windows loads a DLL beside the executable.
check_loading() {
  case "$platform" in
    linux)
      readelf -d "$runtime" | grep -E 'NEEDED|RPATH|RUNPATH'
      readelf -d "$runtime" | grep -F "Shared library: [libsteam_api.so]"
      # shellcheck disable=SC2016  # $ORIGIN is the literal run path entry
      readelf -d "$runtime" | grep -E 'RUNPATH|RPATH' | grep -F '$ORIGIN'
      resolved=$(env -u LD_LIBRARY_PATH ldd "$runtime" | grep -F libsteam_api.so)
      echo "$resolved"
      case "$resolved" in
        *"=> $content/libsteam_api.so "*) ;;
        *) echo "libsteam_api.so does not resolve to the depot copy" >&2; return 1 ;;
      esac
      ;;
    macos)
      otool -L "$runtime" | grep -F '@rpath/libsteam_api.dylib'
      otool -l "$runtime" | grep -A2 LC_RPATH | grep -F '@executable_path/../Frameworks'
      archs=$(lipo -archs "$content/../Frameworks/$library")
      echo "library archs: $archs"
      case " $archs " in *" x86_64 "*" arm64 "* | *" arm64 "*" x86_64 "*) ;; *) echo "library is not universal" >&2; return 1 ;; esac
      ;;
    windows)
      test -f "$content/$library"
      ;;
  esac
}

# macOS: the bundle verifies as a whole and Valve's own signature is still the one on
# the library (it was copied byte for byte and never re-signed).
check_signature() {
  local bundle=$content/../..
  bundle=$(cd "$bundle" && pwd -P)
  codesign --verify --deep --strict --verbose=2 "$bundle"
  codesign --verify --strict --verbose=2 "$bundle/Contents/Frameworks/$library"
  codesign -dvv "$bundle/Contents/Frameworks/$library" 2>&1 | tee /dev/stderr | grep -F 'Identifier=com.valvesoftware.steam'
}

# The runtime starts from an unrelated directory without a Steam client, reports the
# soft Steam state and exits 0. No SteamAppId variable is set.
run_runtime() {
  cd "$(neutral_dir)"
  local report="$extract/run-report.json"
  env -u SteamAppId -u SteamGameId "$runtime" --headless --ticks 3 | tee "$report"
  "$python" -I - "$report" "$expect_state" <<'PY'
import json
import sys

wanted = [state for state in sys.argv[2].split(",") if state]
found = None
with open(sys.argv[1], encoding="utf-8", errors="replace") as handle:
    for line in handle:
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            value = json.loads(line)
        except ValueError:
            continue
        for candidate in (value, value.get("result") if isinstance(value, dict) else None):
            if isinstance(candidate, dict) and isinstance(candidate.get("steam"), dict):
                found = candidate["steam"]
print("steam report:", found)
if found is None or found.get("state") not in wanted:
    sys.exit(f"expected steam.state in {wanted!r}")
PY
}

phase "Extract $(basename "$archive")" do_extract
if depot=$(top_level 2>/dev/null); then
  if [ "$platform" = macos ]; then
    content=$depot/content/macos/$id.app/Contents/MacOS
    resources=$depot/content/macos/$id.app/Contents/Resources
  else
    content=$depot/content/$platform
    resources=$content
  fi
  runtime=$content/runtime
  if is_windows; then
    runtime=$content/runtime.exe
  fi
  phase "Check the depot layout" check_layout
  phase "Check how the runtime finds the Steam library" check_loading
  if [ "$platform" = macos ]; then
    phase "Verify the code signatures" check_signature
  fi
  phase "Run the packaged runtime with no Steam client" run_runtime
fi
finish
