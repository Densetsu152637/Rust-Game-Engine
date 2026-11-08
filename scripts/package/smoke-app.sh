#!/usr/bin/env bash
# Smoke-test a packaged macOS .app bundle: the bundle files, both universal slices,
# the code signature, the layout lookup with no project argument, a headless run and,
# when Rosetta is installed, a headless run of the x86_64 slice. macOS only.
#
# usage: smoke-app.sh <bundle.app>
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=scripts/package/smoke-lib.sh
. "$here/smoke-lib.sh"

if [ "$#" -ne 1 ]; then
  echo "usage: smoke-app.sh <bundle.app>" >&2
  exit 2
fi
if [ ! -d "$1" ]; then
  echo "bundle not found: $1" >&2
  exit 2
fi
bundle=$(cd "$1" && pwd)

check_files() {
  plutil -lint "$bundle/Contents/Info.plist"
  plutil -p "$bundle/Contents/Info.plist"
  test -f "$bundle/Contents/PkgInfo"
  test -x "$bundle/Contents/MacOS/runtime"
  test -f "$bundle/Contents/Resources/game/project.json"
  # Exactly the one icon the bundle generator writes (<id>.icns).
  icons=$(compgen -G "$bundle/Contents/Resources/*.icns")
  test -n "$icons"
  test "$(printf '%s\n' "$icons" | wc -l)" -eq 1
  test -d "$bundle/Contents/Frameworks"
  if [ -e "$bundle/run.sh" ] || [ -e "$bundle/run.cmd" ] || [ -e "$bundle/Contents/run.sh" ]; then
    echo "launcher scripts must not be in a bundle" >&2
    return 1
  fi
  archs=$(lipo -archs "$bundle/Contents/MacOS/runtime")
  echo "lipo -archs: $archs"
  case " $archs " in *" x86_64 "*) ;; *) echo "x86_64 slice missing" >&2; return 1 ;; esac
  case " $archs " in *" arm64 "*) ;; *) echo "arm64 slice missing" >&2; return 1 ;; esac
  # Informational: the icon type and the deployment target of each slice.
  sips -g pixelWidth -g pixelHeight "$icons" || true
  vtool -arch all -show-build "$bundle/Contents/MacOS/runtime" || true
}

check_signature() {
  codesign --verify --deep --strict --verbose=2 "$bundle"
  codesign -dvv "$bundle"
}

# The runtime resolves the game from the bundle layout with no project argument,
# from an unrelated working directory.
run_bundle() {
  cd /
  resolved=$(ENGINE_RESOLVE_ONLY=1 "$bundle/Contents/MacOS/runtime")
  echo "resolved: $resolved"
  case "$resolved" in
    */Resources/game/project.json) ;;
    *) echo "unexpected resolved path" >&2; return 1 ;;
  esac
  "$bundle/Contents/MacOS/runtime" --headless --ticks 3
}

# Rosetta is not installed on every runner; the slice is only built, not run, when it
# is missing.
run_bundle_rosetta() {
  if arch -x86_64 /usr/bin/true 2>/dev/null; then
    cd /
    resolved=$(ENGINE_RESOLVE_ONLY=1 arch -x86_64 "$bundle/Contents/MacOS/runtime")
    echo "resolved (x86_64): $resolved"
    case "$resolved" in
      */Resources/game/project.json) ;;
      *) echo "unexpected resolved path" >&2; return 1 ;;
    esac
    arch -x86_64 "$bundle/Contents/MacOS/runtime" --headless --ticks 3
  else
    echo "Rosetta is not available: the x86_64 slice was built but not executed."
  fi
}

phase "Check the bundle files" check_files
phase "Verify the code signature" check_signature
phase "Resolve and run the bundle" run_bundle
phase "Run the x86_64 slice under Rosetta when available" run_bundle_rosetta
finish
