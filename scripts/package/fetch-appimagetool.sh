#!/usr/bin/env bash
# Download the pinned appimagetool and type2 runtime into <dest-dir> and verify
# their SHA-256 checksums. Prints the two file paths (appimagetool first, runtime
# second) on stdout, one per line; progress and errors go to stderr. Exports
# nothing: the caller sets ENGINE_APPIMAGETOOL / ENGINE_APPIMAGE_RUNTIME.
#
# Pins (tagged releases, never `continuous`; x86_64 only):
#   appimagetool 1.9.1
#     https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
#   type2-runtime 20251108
#     https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
# To bump a pin, change the version and checksum together.
set -euo pipefail

TOOL_VERSION=1.9.1
TOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
RUNTIME_VERSION=20251108
RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

if [ "$#" -ne 1 ]; then
  echo "usage: fetch-appimagetool.sh <dest-dir>" >&2
  exit 2
fi
dest=$1
mkdir -p "$dest"
dest=$(cd "$dest" && pwd)

fetch() { # <url> <file>
  curl --fail --location --silent --show-error --retry 5 --retry-all-errors \
    --output "$2" "$1" >&2
}

fetch "https://github.com/AppImage/appimagetool/releases/download/${TOOL_VERSION}/appimagetool-x86_64.AppImage" \
  "$dest/appimagetool-x86_64.AppImage"
fetch "https://github.com/AppImage/type2-runtime/releases/download/${RUNTIME_VERSION}/runtime-x86_64" \
  "$dest/runtime-x86_64"

(
  cd "$dest"
  printf '%s  %s\n%s  %s\n' \
    "$TOOL_SHA256" appimagetool-x86_64.AppImage \
    "$RUNTIME_SHA256" runtime-x86_64 | sha256sum -c - >&2
)
chmod +x "$dest/appimagetool-x86_64.AppImage"
printf '%s\n%s\n' "$dest/appimagetool-x86_64.AppImage" "$dest/runtime-x86_64"
