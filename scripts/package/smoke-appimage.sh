#!/usr/bin/env bash
# Smoke-test a packaged .AppImage on a runner without FUSE: run it headless through
# --appimage-extract-and-run, extract it and check the AppDir, then start the extracted
# AppRun from a read-only tree with no inherited APPDIR.
#
# usage: smoke-appimage.sh <file.AppImage> <work-dir> [expected-id]
#   <work-dir> must not exist; the runs start there (an unrelated directory) and the
#   extraction goes to <work-dir>/squashfs-root. Write permission is restored on exit
#   so the caller can delete the directory. With [expected-id] the AppDir must hold
#   exactly <id>.desktop, <id>.png and .DirIcon for that id; without it the id is
#   taken from the single .desktop file found.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=scripts/package/smoke-lib.sh
. "$here/smoke-lib.sh"

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: smoke-appimage.sh <file.AppImage> <work-dir> [expected-id]" >&2
  exit 2
fi
image=$1
work=$2
expected_id=${3:-}
if [ ! -f "$image" ]; then
  echo "AppImage not found: $image" >&2
  exit 2
fi
if [ -e "$work" ]; then
  echo "work directory must not exist: $work" >&2
  exit 2
fi
image=$(cd "$(dirname "$image")" && pwd)/$(basename "$image")
mkdir -p "$work"
work=$(cd "$work" && pwd)
# The phases below make the tree read-only; always make it writable again.
trap 'chmod -R u+w "$work" 2>/dev/null || true' EXIT

check_file() {
  # No update information is embedded, so no .zsync may be left beside the image.
  if compgen -G "$(dirname "$image")/*.zsync" >/dev/null; then
    echo "unexpected .zsync file" >&2
    return 1
  fi
  test -x "$image"
}

run_image() {
  cd "$work"
  "$image" --appimage-extract-and-run --headless --ticks 3
}

check_appdir() {
  cd "$work"
  "$image" --appimage-extract
  ls -la squashfs-root
  test -x squashfs-root/AppRun
  test -x squashfs-root/runtime
  test -f squashfs-root/game/project.json
  test -f squashfs-root/.DirIcon
  local desktop id
  if [ -n "$expected_id" ]; then
    id=$expected_id
    test -f "squashfs-root/$id.desktop"
  else
    desktop=$(compgen -G 'squashfs-root/*.desktop')
    if [ "$(printf '%s\n' "$desktop" | wc -l)" -ne 1 ]; then
      echo "expected exactly one .desktop file, found:" >&2
      printf '%s\n' "$desktop" >&2
      return 1
    fi
    id=$(basename "$desktop" .desktop)
  fi
  test -f "squashfs-root/$id.png"
  desktop-file-validate "squashfs-root/$id.desktop"
}

# The image is mounted read-only; the game must start from a read-only tree, from an
# unrelated directory, with no inherited APPDIR.
run_appdir_read_only() {
  cd "$work"
  test -d squashfs-root
  chmod -R a-w squashfs-root
  env -u APPDIR squashfs-root/AppRun --headless --ticks 3
}

phase "Check the AppImage file" check_file
phase "Run the AppImage headless" run_image
phase "Check the extracted AppDir" check_appdir
phase "Run the extracted AppDir read-only" run_appdir_read_only
finish
